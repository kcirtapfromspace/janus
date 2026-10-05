//! The call's video, seen on this Mac: who was on camera, and how the other people on the call
//! moved while you answered. Behaviour, not mind-reading: head movements and which way faces point,
//! never emotions, and nothing that identifies anyone.
//!
//! `ic-vision` (mac/Sources/ICVision) samples the recorder's `video.mov` a few times a second and
//! writes every face Vision finds to `faces.json`. Here detections become tracks (the same face in
//! the same place over time). Each track is labelled as you or someone else by whose speech its
//! mouth moves with. Then each of your answers gets the others' cues: how many were on camera, how
//! often they nodded, and how much of the time they looked away.
//!
//! Every threshold is in `Params`, so the eval (video_eval.rs, docs/eval/video-decision.md) can
//! compare versions and sweep them. Until a cue passes that test it's shown as experimental, and a
//! cue that fails isn't shown (`GATE`).

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::models::{Segment, YOU};
use crate::progress::Progress;
use crate::temperature::{Kind, Signal};
use crate::tools::Tool;

/// What the recorder writes (see mac/Sources/ICRecorderCore/ScreenCapture.swift).
pub const VIDEO_FILE: &str = "video.mov";
pub const FACES_FILE: &str = "faces.json";
/// Samples per second `ic-vision` looks at: nods take about half a second.
pub const SAMPLES_PER_S: f64 = 6.0;

/// The thresholds and choices behind the cues.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Params {
    /// Stored with every timeline built with these, and the eval arm's name.
    pub name: &'static str,
    /// Faces smaller than this (a fraction of the frame's height) are too small to read.
    pub min_face_h: f64,
    /// A face in the next sample continues a track when their boxes overlap at least this much.
    pub match_iou: f64,
    /// A track that hasn't been seen for this long has ended.
    pub track_gap_s: f64,
    /// Mouth-movement pairs needed while you speak, and while they speak, to tell a face apart.
    pub min_label_pairs: usize,
    /// How much more your mouth must move while you speak than while they do.
    pub you_margin: f64,
    /// A nod: the head drops at least this much (in face heights) and comes back up…
    pub nod_drop: f64,
    /// …within this long on each side of its lowest point.
    pub nod_window_s: f64,
    /// Lowest points closer together than this are one nod.
    pub nod_gap_s: f64,
    /// Turned at least this far (degrees) is looking away: aside at another screen, or down at notes.
    pub away_yaw: f64,
    pub away_pitch: f64,
    /// An answer needs this much video for cues.
    pub min_seen_s: f64,
    /// Pool the evidence of tracks in the same place (one tile), so a face whose track breaks into
    /// pieces (looking down, a layout change) is still told apart.
    pub pool_tiles: bool,
    /// Leave faces that can't be told apart out of the others' cues, instead of counting them as
    /// someone else (where your own fragments would add "their" nods).
    pub exclude_unknown: bool,
    /// Measure how much video covered an answer from the gaps between samples. Samples ÷ the rate
    /// asked for undercounts: the recorder writes a frame only when the picture changes.
    pub coverage_from_gaps: bool,
}

/// As shipped in 0.1.0-preview.14. Kept so the eval can measure what later versions change.
pub const V1: Params = Params {
    name: "video-v1",
    min_face_h: 0.04,
    match_iou: 0.3,
    track_gap_s: 2.0,
    min_label_pairs: 12,
    you_margin: 0.25,
    nod_drop: 0.06,
    nod_window_s: 0.8,
    nod_gap_s: 0.35,
    away_yaw: 30.0,
    away_pitch: 25.0,
    min_seen_s: 3.0,
    pool_tiles: false,
    exclude_unknown: false,
    coverage_from_gaps: false,
};

/// V1's thresholds with the fixes from reviewing it: fragments of one face are pooled, faces that
/// can't be told apart are left out, and coverage counts frames that stood still.
pub const V1_1: Params = Params { name: "video-v1.1", pool_tiles: true, exclude_unknown: true, coverage_from_gaps: true, ..V1 };

/// What reviews are built with.
pub const CURRENT: Params = V1_1;
/// The cue formulas in use, stored with each timeline.
pub const METHOD: &str = CURRENT.name;

impl Params {
    /// A named arm for the eval: `video-v1` or `video-v1.1`.
    pub fn named(name: &str) -> Option<Params> {
        [V1, V1_1].into_iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Faces {
    pub version: u32,
    /// Samples per second asked for.
    pub fps: f64,
    pub samples: Vec<Sample>,
    /// False when the video couldn't be read to the end (a recording cut short): the samples run
    /// up to where it stopped.
    #[serde(default = "complete_by_default")]
    pub complete: bool,
    /// Frames Vision couldn't read, left out.
    #[serde(default)]
    pub skipped: usize,
}

fn complete_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    /// Seconds from the start of the recording (the video starts with the audio).
    pub t: f64,
    pub faces: Vec<Face>,
}

/// One face in one sample: its box as fractions of the frame from the top-left, its angles in
/// degrees, and how open its mouth is (a fraction of the face's height).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Face {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub yaw: Option<f64>,
    #[serde(default)]
    pub pitch: Option<f64>,
    #[serde(default)]
    pub mouth: Option<f64>,
}

/// What the video showed of the other people on the call during one of your answers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VideoCues {
    /// People on camera besides you, on average.
    pub on_camera: f64,
    /// Their nods.
    pub nods: usize,
    /// Share of their time on camera spent turned away (aside, or down at notes), when their
    /// faces' angles were measured.
    pub looking_away: Option<f64>,
    /// Seconds of the answer the video covered.
    pub seen_s: f64,
    /// Their faces' typical height, as a fraction of the frame: small tiles hide small nods.
    #[serde(default)]
    pub face_h: Option<f64>,
}

/// Run `ic-vision` on the recording's video, writing `out`. Returns warnings (a video that
/// couldn't be read to the end, frames Vision couldn't read).
pub fn extract(video: &Path, out: &Path, progress: &mut dyn Progress) -> Result<Vec<String>> {
    extract_with(video, out, SAMPLES_PER_S, &[], progress)
}

/// `extract`, at `fps` and only within `ranges` (seconds) when there are any: the eval reads its
/// clips densely without reading the whole call.
pub fn extract_with(video: &Path, out: &Path, fps: f64, ranges: &[(f64, f64)], progress: &mut dyn Progress) -> Result<Vec<String>> {
    let mut cmd = Tool::Vision.command()?;
    cmd.arg("faces").arg("--video").arg(video).arg("--out").arg(out).args(["--fps", &fps.to_string(), "--progress"]);
    if !ranges.is_empty() {
        let spec: Vec<String> = ranges.iter().map(|(a, b)| format!("{a:.2}-{b:.2}")).collect();
        cmd.args(["--ranges", &spec.join(",")]);
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("starting ic-vision")?;
    // Drained alongside stdout, so a chatty framework can't fill the pipe and stall the tool.
    let mut stderr = child.stderr.take().context("ic-vision's errors")?;
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let stdout = child.stdout.take().context("ic-vision's output")?;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            let _ = child.kill();
            break;
        };
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(done) = event["progress"].as_f64()
        {
            progress.step((done * 1000.0).round() as u64, 1000);
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("ic-vision couldn't read the video: {}", errors.trim());
    }
    let faces = load_file(out)?;
    let mut warnings = vec![];
    if !faces.complete {
        let last = faces.samples.last().map_or(0.0, |s| s.t);
        warnings.push(format!("The video could only be read up to {}: {}", crate::models::fmt_ts(last), errors.trim()));
    }
    if faces.skipped > 0 {
        warnings.push(format!("{} video frames couldn't be read and were skipped.", faces.skipped));
    }
    Ok(warnings)
}

fn load_file(path: &Path) -> Result<Faces> {
    let faces: Faces = serde_json::from_str(&std::fs::read_to_string(path)?).with_context(|| format!("reading {}", path.display()))?;
    if faces.version != 1 {
        bail!("{} is version {}; this ic reads version 1", path.display(), faces.version);
    }
    Ok(faces)
}

/// The session's faces, if its video has been read.
pub fn load(dir: &Path) -> Result<Option<Faces>> {
    let path = dir.join(FACES_FILE);
    if !path.exists() {
        return Ok(None);
    }
    load_file(&path).map(Some)
}

// --- tracks ---------------------------------------------------------------------------------------

/// One face followed through the call: the same tile, sample after sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub points: Vec<(f64, Face)>,
}

fn iou(a: &Face, b: &Face) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    inter / (a.w * a.h + b.w * b.h - inter)
}

/// Follow faces from sample to sample by where they are. The best-overlapping pairs are matched
/// first; a face that matches nothing starts a new track.
pub fn tracks(faces: &Faces, p: &Params) -> Vec<Track> {
    let mut tracks: Vec<Track> = vec![];
    let mut active: Vec<usize> = vec![];
    for sample in &faces.samples {
        active.retain(|&i| tracks[i].points.last().is_some_and(|(t, _)| sample.t - t <= p.track_gap_s));
        let seen: Vec<&Face> = sample.faces.iter().filter(|f| f.h >= p.min_face_h).collect();
        let mut pairs: Vec<(f64, usize, usize)> = vec![];
        for (f, face) in seen.iter().enumerate() {
            for (a, &track) in active.iter().enumerate() {
                let overlap = iou(face, &tracks[track].points.last().expect("tracks start with a point").1);
                if overlap >= p.match_iou {
                    pairs.push((overlap, f, a));
                }
            }
        }
        pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
        let (mut face_taken, mut track_taken) = (vec![false; seen.len()], vec![false; active.len()]);
        for (_, f, a) in pairs {
            if !face_taken[f] && !track_taken[a] {
                face_taken[f] = true;
                track_taken[a] = true;
                tracks[active[a]].points.push((sample.t, *seen[f]));
            }
        }
        for f in (0..seen.len()).filter(|&f| !face_taken[f]) {
            tracks.push(Track { points: vec![(sample.t, *seen[f])] });
            active.push(tracks.len() - 1);
        }
    }
    tracks
}

// --- who is who -----------------------------------------------------------------------------------

/// Sorted speech spans, with the latest end so far, so "is someone speaking at t" is exact even
/// when spans overlap.
struct Spans {
    starts: Vec<f64>,
    max_end: Vec<f64>,
}

impl Spans {
    fn new(mut spans: Vec<(f64, f64)>) -> Self {
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut latest = f64::NEG_INFINITY;
        let max_end = spans.iter().map(|s| {
            latest = latest.max(s.1);
            latest
        });
        let max_end = max_end.collect();
        Spans { starts: spans.iter().map(|s| s.0).collect(), max_end }
    }

    fn during(&self, t: f64) -> bool {
        let i = self.starts.partition_point(|&s| s <= t);
        i > 0 && self.max_end[i - 1] >= t
    }
}

/// When you spoke and when someone else did, from the transcript.
pub struct Speech {
    you: Spans,
    them: Spans,
}

impl Speech {
    pub fn from_segments(segments: &[Segment]) -> Self {
        let spans = |you: bool| {
            Spans::new(segments.iter().filter(|s| (s.speaker == YOU) == you && !s.text.trim().is_empty()).map(|s| (s.start, s.end)).collect())
        };
        Speech { you: spans(true), them: spans(false) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    You,
    Other,
    /// Too little of both kinds of speech to tell (left out of the others' cues with `exclude_unknown`).
    Unknown,
}

/// How much a face's mouth moved while you spoke, and while someone else did.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Evidence {
    you: (f64, usize),
    them: (f64, usize),
}

fn evidence(track: &Track, speech: &Speech) -> Evidence {
    let mut e = Evidence::default();
    for pair in track.points.windows(2) {
        let ((t0, a), (t1, b)) = (pair[0], pair[1]);
        let (Some(ma), Some(mb)) = (a.mouth, b.mouth) else { continue };
        if t1 - t0 > 0.6 {
            continue;
        }
        let t = (t0 + t1) / 2.0;
        let motion = (mb - ma).abs();
        match (speech.you.during(t), speech.them.during(t)) {
            (true, false) => e.you = (e.you.0 + motion, e.you.1 + 1),
            (false, true) => e.them = (e.them.0 + motion, e.them.1 + 1),
            _ => {}
        }
    }
    e
}

fn label(e: Evidence, p: &Params) -> Who {
    if e.you.1 < p.min_label_pairs || e.them.1 < p.min_label_pairs {
        return if p.exclude_unknown { Who::Unknown } else { Who::Other };
    }
    let (a, b) = (e.you.0 / e.you.1 as f64, e.them.0 / e.them.1 as f64);
    if a > 0.0 && (a - b) / (a + b) >= p.you_margin { Who::You } else { Who::Other }
}

/// Your face is the one whose mouth moves more while you speak than while they do.
pub fn who(track: &Track, speech: &Speech, p: &Params) -> Who {
    label(evidence(track, speech), p)
}

/// A track's typical box: where its tile is.
fn median_box(track: &Track) -> Face {
    let mid = |f: fn(&Face) -> f64| median(&mut track.points.iter().map(|(_, face)| f(face)).collect::<Vec<_>>());
    Face { x: mid(|f| f.x), y: mid(|f| f.y), w: mid(|f| f.w), h: mid(|f| f.h), yaw: None, pitch: None, mouth: None }
}

/// Who each track is. With `pool_tiles`, tracks that sit in the same place share their evidence,
/// so the pieces of one face are labelled together.
pub fn labels(tracks: &[Track], speech: &Speech, p: &Params) -> Vec<Who> {
    let ev: Vec<Evidence> = tracks.iter().map(|t| evidence(t, speech)).collect();
    if !p.pool_tiles {
        return ev.into_iter().map(|e| label(e, p)).collect();
    }
    let boxes: Vec<Face> = tracks.iter().map(median_box).collect();
    let mut group: Vec<usize> = (0..tracks.len()).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for i in 0..tracks.len() {
        for j in i + 1..tracks.len() {
            if iou(&boxes[i], &boxes[j]) >= p.match_iou {
                let (a, b) = (root(&mut group, i), root(&mut group, j));
                group[a] = b;
            }
        }
    }
    let mut pooled: std::collections::HashMap<usize, Evidence> = std::collections::HashMap::new();
    for (i, e) in ev.iter().enumerate() {
        let g = pooled.entry(root(&mut group, i)).or_default();
        g.you = (g.you.0 + e.you.0, g.you.1 + e.you.1);
        g.them = (g.them.0 + e.them.0, g.them.1 + e.them.1);
    }
    (0..tracks.len()).map(|i| label(pooled[&root(&mut group, i)], p)).collect()
}

// --- cues -----------------------------------------------------------------------------------------

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// When a track's head dipped and came back up. Its height in the frame is measured in its own
/// face heights, so a big tile and a small one nod alike; a dip that moves sideways as much as
/// down is someone shifting in their seat, not a nod.
pub fn nods(track: &Track, p: &Params) -> Vec<f64> {
    let pts = &track.points;
    if pts.len() < 3 {
        return vec![];
    }
    let h = median(&mut pts.iter().map(|(_, f)| f.h).collect::<Vec<_>>()).max(1e-6);
    let v: Vec<(f64, f64, f64)> = pts.iter().map(|(t, f)| (*t, (f.y + f.h / 2.0) / h, (f.x + f.w / 2.0) / h)).collect();
    let mut out: Vec<f64> = vec![];
    for i in 1..v.len() - 1 {
        let (t, y, x) = v[i];
        // The head's lowest point (y grows downwards).
        if y < v[i - 1].1 || y < v[i + 1].1 {
            continue;
        }
        let before = v[..i].iter().rev().take_while(|q| t - q.0 <= p.nod_window_s).min_by(|a, b| a.1.total_cmp(&b.1));
        let after = v[i + 1..].iter().take_while(|q| q.0 - t <= p.nod_window_s).min_by(|a, b| a.1.total_cmp(&b.1));
        let (Some(before), Some(after)) = (before, after) else { continue };
        let drop = (y - before.1).min(y - after.1);
        let sideways = (x - before.2).abs().max((x - after.2).abs());
        if drop >= p.nod_drop && sideways < drop && out.last().is_none_or(|&last| t - last >= p.nod_gap_s) {
            out.push(t);
        }
    }
    out
}

fn away(face: &Face, p: &Params) -> Option<bool> {
    let yaw = face.yaw?;
    Some(yaw.abs() >= p.away_yaw || face.pitch.is_some_and(|pitch| pitch.abs() >= p.away_pitch))
}

/// Seconds of `start..end` the video covered. Each sample stands until the next (the recorder
/// only writes frames that changed), up to the gap after which a track ends: a longer silence is
/// more likely capture stalling than a picture standing still.
fn coverage(faces: &Faces, start: f64, end: f64, p: &Params) -> f64 {
    let within: Vec<f64> = faces.samples.iter().map(|s| s.t).filter(|t| *t >= start && *t <= end).collect();
    if !p.coverage_from_gaps {
        return within.len() as f64 / faces.fps.max(1e-6);
    }
    let step = 1.0 / faces.fps.max(1e-6);
    within
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let next = within.get(i + 1).copied().unwrap_or(t + step).min(end);
            (next - t).clamp(0.0, p.track_gap_s)
        })
        .sum()
}

/// The other people's cues between `start` and `end`; None when the video covers too little of it.
pub fn cues(faces: &Faces, others: &[(&Track, Vec<f64>)], start: f64, end: f64, p: &Params) -> Option<VideoCues> {
    let within = |t: f64| t >= start && t <= end;
    let samples = faces.samples.iter().filter(|s| within(s.t)).count();
    let seen_s = coverage(faces, start, end, p);
    if samples == 0 || seen_s < p.min_seen_s.min((end - start) * 0.5) {
        return None;
    }
    let (mut present, mut turned, mut angled, mut nodded) = (0usize, 0usize, 0usize, 0usize);
    let mut heights = vec![];
    for (track, nods) in others {
        for (_, face) in track.points.iter().filter(|(t, _)| within(*t)) {
            present += 1;
            heights.push(face.h);
            if let Some(a) = away(face, p) {
                angled += 1;
                turned += a as usize;
            }
        }
        nodded += nods.iter().filter(|&&t| within(t)).count();
    }
    Some(VideoCues {
        on_camera: present as f64 / samples as f64,
        nods: nodded,
        looking_away: (angled > 0).then(|| turned as f64 / angled as f64),
        seen_s: seen_s.min(end - start),
        face_h: (!heights.is_empty()).then(|| (median(&mut heights) * 1000.0).round() / 1000.0),
    })
}

/// Everyone but you, each with their nods: what `cues` counts.
pub fn others<'a>(tracks: &'a [Track], speech: &Speech, p: &Params) -> Vec<(&'a Track, Vec<f64>)> {
    let who = labels(tracks, speech, p);
    tracks.iter().zip(who).filter(|(_, w)| *w == Who::Other).map(|(t, _)| (t, nods(t, p))).collect()
}

/// Add the others' video cues to each of your answers.
pub fn annotate(signals: &mut [Signal], faces: &Faces, segments: &[Segment]) {
    let p = CURRENT;
    let speech = Speech::from_segments(segments);
    let tracks = tracks(faces, &p);
    let others = others(&tracks, &speech, &p);
    for s in signals.iter_mut().filter(|s| s.kind == Kind::Answer) {
        s.video = cues(faces, &others, s.start, s.end, &p);
    }
}

// --- what the review shows ------------------------------------------------------------------------

/// Which cues the review shows, from the eval (docs/eval/video-decision.md). A check that passed is
/// shown plainly; one that failed isn't shown; one not yet tested is shown as experimental. A check
/// can also be hidden for faces smaller than a share of the frame's height.
pub struct Gate {
    pub passed: &'static [&'static str],
    pub failed: &'static [&'static str],
    pub hidden_below: &'static [(&'static str, f64)],
}

/// Nothing has been tested yet.
pub const GATE: Gate = Gate { passed: &[], failed: &[], hidden_below: &[] };

impl Gate {
    pub fn shows(&self, check: &str, face_h: Option<f64>) -> bool {
        !self.failed.contains(&check) && !self.hidden_below.iter().any(|(c, h)| *c == check && face_h.is_some_and(|f| f < *h))
    }

    /// Whether the video section still needs its "experimental" tag: some check it shows hasn't passed.
    pub fn experimental(&self) -> bool {
        ["on_camera", "nodded", "nod_count", "looked_away"].iter().any(|c| !self.failed.contains(c) && !self.passed.contains(c))
    }
}

/// What the video showed across the interview, for the report's summary line.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// People usually on camera besides you (the median over your answers, rounded).
    pub usually_on_camera: usize,
    pub nods: usize,
    pub answers: usize,
    /// Answers with at least one nod.
    pub answers_with_nods: usize,
}

pub fn summary(signals: &[Signal]) -> Option<Summary> {
    let seen: Vec<&VideoCues> = signals.iter().filter(|s| s.kind == Kind::Answer).filter_map(|s| s.video.as_ref()).collect();
    if seen.is_empty() {
        return None;
    }
    Some(Summary {
        usually_on_camera: median(&mut seen.iter().map(|c| c.on_camera).collect::<Vec<_>>()).round() as usize,
        nods: seen.iter().map(|c| c.nods).sum(),
        answers: seen.len(),
        answers_with_nods: seen.iter().filter(|c| c.nods > 0).count(),
    })
}

/// "Usually 2 people on camera besides you. They nodded 14 times, during 6 of your 9 answers.",
/// leaving out what the gate doesn't show. Empty when it shows nothing.
pub fn sentence(s: &Summary, gate: &Gate) -> String {
    let mut parts = vec![];
    if gate.shows("on_camera", None) {
        parts.push(match s.usually_on_camera {
            0 => "Usually nobody else was on camera.".to_string(),
            1 => "Usually 1 person on camera besides you.".to_string(),
            n => format!("Usually {n} people on camera besides you."),
        });
    }
    if gate.shows("nod_count", None) {
        parts.push(match s.nods {
            0 => "No nods were seen while you answered.".to_string(),
            1 => "They nodded once while you answered.".to_string(),
            n => format!("They nodded {n} times, during {} of your {} answers.", s.answers_with_nods, s.answers),
        });
    } else if gate.shows("nodded", None) && s.answers_with_nods > 0 {
        parts.push(format!("They nodded during {} of your {} answers.", s.answers_with_nods, s.answers));
    }
    parts.join(" ")
}

/// What stood out in one answer's video, in words ("they nodded 3 times", "looked away about half
/// the time"), leaving out what the gate doesn't show.
pub fn notes(cues: &VideoCues, gate: &Gate) -> Vec<String> {
    let mut out = vec![];
    if cues.on_camera < 0.5 {
        if gate.shows("on_camera", cues.face_h) {
            out.push("nobody else was on camera".to_string());
        }
        return out;
    }
    if gate.shows("nod_count", cues.face_h) {
        match cues.nods {
            0 => {}
            1 => out.push("they nodded once".into()),
            n => out.push(format!("they nodded {n} times")),
        }
    } else if gate.shows("nodded", cues.face_h) && cues.nods > 0 {
        out.push("they nodded".into());
    }
    if gate.shows("looked_away", cues.face_h)
        && let Some(away) = cues.looking_away.filter(|a| *a >= 0.4)
    {
        out.push(match away {
            a if a >= 0.75 => "they looked away most of the time".into(),
            a if a >= 0.55 => "they looked away more than half the time".into(),
            _ => "they looked away almost half the time".into(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::INTERVIEWER;

    fn face(x: f64, y: f64, mouth: f64) -> Face {
        Face { x, y, w: 0.15, h: 0.25, yaw: Some(0.0), pitch: Some(-5.0), mouth: Some(mouth) }
    }

    /// `seconds` of samples at 6 a second, each with the faces `at(t)` returns.
    fn video(seconds: f64, at: impl Fn(f64) -> Vec<Face>) -> Faces {
        let n = (seconds * 6.0) as usize;
        Faces {
            version: 1,
            fps: 6.0,
            samples: (0..n).map(|i| i as f64 / 6.0).map(|t| Sample { t, faces: at(t) }).collect(),
            complete: true,
            skipped: 0,
        }
    }

    fn talking(t: f64) -> f64 {
        if (t * 6.0).round() as i64 % 2 == 0 { 0.02 } else { 0.25 }
    }

    /// The shipped preview.14 behaviour must not move: V1 is what its timelines were built with.
    #[test]
    fn v1_is_exactly_what_shipped() {
        assert_eq!((V1.min_face_h, V1.match_iou, V1.track_gap_s, V1.min_label_pairs, V1.you_margin), (0.04, 0.3, 2.0, 12, 0.25));
        assert_eq!((V1.nod_drop, V1.nod_window_s, V1.nod_gap_s, V1.away_yaw, V1.away_pitch, V1.min_seen_s), (0.06, 0.8, 0.35, 30.0, 25.0, 3.0));
        const { assert!(!V1.pool_tiles && !V1.exclude_unknown && !V1.coverage_from_gaps) };
        let same_thresholds = Params { name: V1.name, pool_tiles: false, exclude_unknown: false, coverage_from_gaps: false, ..V1_1 };
        assert_eq!(same_thresholds, V1, "1.1 only adds the fixes");
        assert_eq!(METHOD, "video-v1.1");
        assert_eq!(Params::named("video-v1"), Some(V1));
        assert_eq!(Params::named("video-v9"), None);
    }

    #[test]
    fn faces_in_the_same_place_are_one_track_and_a_new_place_is_another() {
        let v = video(10.0, |t| {
            let mut f = vec![face(0.1, 0.2, 0.05)];
            if t >= 5.0 {
                f.push(face(0.6, 0.2, 0.05));
            }
            f
        });
        let t = tracks(&v, &CURRENT);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].points.len(), 60);
        assert_eq!(t[1].points.first().map(|p| p.0), Some(5.0));
    }

    #[test]
    fn tiny_faces_and_long_gaps_are_left_out() {
        let v = video(10.0, |t| {
            let mut f = vec![Face { h: 0.02, ..face(0.5, 0.5, 0.0) }];
            if !(3.0..6.0).contains(&t) {
                f.push(face(0.1, 0.2, 0.05));
            }
            f
        });
        let t = tracks(&v, &CURRENT);
        assert_eq!(t.len(), 2, "the face that was gone for 3 s comes back as a new track");
        assert!(t.iter().all(|t| t.points.iter().all(|(_, f)| f.h >= CURRENT.min_face_h)));
    }

    /// Overlapping spans: a long one followed by many short ones inside it.
    #[test]
    fn speaking_is_found_inside_any_overlapping_span() {
        let mut segs = vec![Segment::new(0.0, 100.0, "a long answer", YOU)];
        segs.extend((0..10).map(|i| Segment::new(i as f64, i as f64 + 0.5, "um", YOU)));
        let speech = Speech::from_segments(&segs);
        assert!(speech.you.during(50.0), "inside the long span, past the short ones");
        assert!(!speech.you.during(150.0));
        assert!(!speech.them.during(1.0));
    }

    /// You talk 0–20 s, they talk 20–40 s. Your mouth moves with your speech; theirs with theirs.
    #[test]
    fn your_face_is_the_one_that_talks_when_you_do() {
        let segments = [Segment::new(0.0, 20.0, "my answer", YOU), Segment::new(20.0, 40.0, "their question", INTERVIEWER)];
        let speech = Speech::from_segments(&segments);
        let v = video(40.0, |t| {
            let (mine, theirs) = if t < 20.0 { (talking(t), 0.03) } else { (0.03, talking(t)) };
            vec![face(0.1, 0.2, mine), face(0.6, 0.2, theirs)]
        });
        let t = tracks(&v, &CURRENT);
        assert_eq!(labels(&t, &speech, &CURRENT), [Who::You, Who::Other]);
        let silent = Track { points: t[0].points.iter().take(30).copied().collect() };
        assert_eq!(who(&silent, &speech, &V1), Who::Other, "v1: too little to tell counts as someone else");
        assert_eq!(who(&silent, &speech, &V1_1), Who::Unknown, "v1.1: too little to tell is left out");
    }

    /// The reviewer's case: your face keeps dropping out (looking down), so it breaks into 3-second
    /// tracks, each too short to label. Pooled by tile, they're you again.
    #[test]
    fn your_broken_track_is_still_you_and_never_adds_their_nods() {
        let segments: Vec<Segment> = (0..6)
            .flat_map(|k| {
                let s = k as f64 * 20.0;
                [Segment::new(s, s + 10.0, "my answer", YOU), Segment::new(s + 10.0, s + 20.0, "their question", INTERVIEWER)]
            })
            .collect();
        let speech = Speech::from_segments(&segments);
        let v = video(120.0, |t| {
            let mine_speaking = (t % 20.0) < 10.0;
            let (mine, theirs) = if mine_speaking { (talking(t), 0.03) } else { (0.03, talking(t)) };
            // You nod while you talk, and you're out of view for 2 s in every 5.
            let my_y = if mine_speaking && (t * 6.0).round() as i64 % 6 == 3 { 0.23 } else { 0.2 };
            let mut f = vec![face(0.6, 0.2, theirs)];
            if (t % 5.0) < 3.0 {
                f.push(face(0.1, my_y, mine));
            }
            f
        });
        let t = tracks(&v, &CURRENT);
        assert!(t.len() > 3, "your face breaks into pieces: {}", t.len());
        let v1_nods: usize = others(&t, &speech, &V1).iter().map(|(_, n)| n.len()).sum();
        let v11_nods: usize = others(&t, &speech, &V1_1).iter().map(|(_, n)| n.len()).sum();
        assert!(v1_nods > 0, "v1 counts your pieces as someone else, and your nods as theirs");
        assert_eq!(v11_nods, 0, "v1.1 pools them into you");
        let answer = cues(&v, &others(&t, &speech, &V1_1), 0.0, 10.0, &V1_1).unwrap();
        assert!((answer.on_camera - 1.0).abs() < 0.05, "one other person, not two: {answer:?}");
    }

    #[test]
    fn a_dip_and_return_is_a_nod_and_a_slow_drift_or_a_shift_is_not() {
        let p = CURRENT;
        let head = |ys: &[(f64, f64)]| Track { points: ys.iter().map(|&(t, y)| (t, face(0.4, y, 0.05))).collect() };
        // Two nods: the head drops 0.025 of the frame (10% of its 0.25 height) and comes back.
        let nodding: Vec<(f64, f64)> = (0..60).map(|i| {
            let t = i as f64 / 6.0;
            (t, if (t - 3.0).abs() < 0.1 || (t - 6.0).abs() < 0.1 { 0.225 } else { 0.2 })
        }).collect();
        assert_eq!(nods(&head(&nodding), &p), [3.0, 6.0]);
        let drifting: Vec<(f64, f64)> = (0..60).map(|i| (i as f64 / 6.0, 0.2 + i as f64 * 0.001)).collect();
        assert!(nods(&head(&drifting), &p).is_empty(), "slowly sinking isn't nodding");
        let jitter: Vec<(f64, f64)> = (0..60).map(|i| (i as f64 / 6.0, 0.2 + if i % 3 == 0 { 0.004 } else { 0.0 })).collect();
        assert!(nods(&head(&jitter), &p).is_empty(), "the detector's wobble isn't nodding");
        let shifting = Track { points: (0..60).map(|i| {
            let t = i as f64 / 6.0;
            let moved = (t - 3.0).abs() < 0.1;
            (t, face(if moved { 0.46 } else { 0.4 }, if moved { 0.225 } else { 0.2 }, 0.05))
        }).collect() };
        assert!(nods(&shifting, &p).is_empty(), "leaning to the side isn't a nod");
        let bigger = Params { nod_drop: 0.15, ..p };
        assert!(nods(&head(&nodding), &bigger).is_empty(), "the threshold is a parameter");
    }

    #[test]
    fn answer_cues_count_the_others_nods_and_turning_away() {
        let segments = [Segment::new(0.0, 20.0, "my answer", YOU), Segment::new(20.0, 40.0, "their question", INTERVIEWER)];
        let v = video(40.0, |t| {
            let (mine, theirs) = if t < 20.0 { (talking(t), 0.03) } else { (0.03, talking(t)) };
            let nod = [4.0, 9.0, 14.0].iter().any(|n| (t - n).abs() < 0.1);
            // They look down at their notes for the first 5 s of your answer.
            let pitch = if t < 5.0 { -35.0 } else { -5.0 };
            // Your own head bobs while you talk; that must not count as their nod.
            let my_y = if t < 20.0 && (t * 6.0).round() as i64 % 12 == 0 { 0.23 } else { 0.2 };
            vec![face(0.1, my_y, mine), Face { pitch: Some(pitch), ..face(0.6, if nod { 0.225 } else { 0.2 }, theirs) }]
        });
        let mut signals = crate::temperature::build(&crate::temperature::conversation(&segments), &Default::default(), None);
        annotate(&mut signals, &v, &segments);
        let answer = signals.iter().find(|s| s.kind == Kind::Answer).and_then(|s| s.video).expect("cues on the answer");
        assert_eq!(answer.nods, 3);
        assert!((answer.on_camera - 1.0).abs() < 0.05, "{answer:?}");
        assert!((answer.looking_away.unwrap() - 0.25).abs() < 0.05, "{answer:?}");
        assert_eq!(answer.face_h, Some(0.25));
        assert!(signals.iter().filter(|s| s.kind != Kind::Answer).all(|s| s.video.is_none()));
        assert_eq!(notes(&answer, &GATE), ["they nodded 3 times"]);
        let summary = summary(&signals).unwrap();
        assert_eq!((summary.usually_on_camera, summary.nods, summary.answers_with_nods), (1, 3, 1));
        assert_eq!(sentence(&summary, &GATE), "Usually 1 person on camera besides you. They nodded 3 times, during 1 of your 1 answers.");
    }

    /// The reviewer's case: a still picture gives a frame every few seconds, not 6 a second.
    #[test]
    fn a_picture_that_stands_still_still_covers_the_answer() {
        let sparse = Faces {
            version: 1,
            fps: 6.0,
            samples: (0..10).map(|i| Sample { t: i as f64 * 2.0, faces: vec![face(0.6, 0.2, 0.03)] }).collect(),
            complete: true,
            skipped: 0,
        };
        assert_eq!(cues(&sparse, &[], 0.0, 20.0, &V1), None, "v1 thinks 10 samples ÷ 6 = 1.7 s were seen");
        let c = cues(&sparse, &[], 0.0, 20.0, &V1_1).expect("v1.1 counts each frame until the next");
        assert!((c.seen_s - (18.0 + 1.0 / 6.0)).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn too_little_video_gives_no_cues_and_an_empty_call_says_so() {
        let v = video(2.0, |_| vec![]);
        assert_eq!(cues(&v, &[], 0.0, 30.0, &CURRENT), None, "2 s of a 30 s answer");
        let empty = video(30.0, |_| vec![]);
        let c = cues(&empty, &[], 0.0, 30.0, &CURRENT).unwrap();
        assert_eq!(c.on_camera, 0.0);
        assert_eq!(c.face_h, None);
        assert_eq!(notes(&c, &GATE), ["nobody else was on camera"]);
    }

    #[test]
    fn the_gate_hides_failed_checks_and_small_faces() {
        let c = VideoCues { on_camera: 1.0, nods: 3, looking_away: Some(0.6), seen_s: 20.0, face_h: Some(0.06) };
        assert_eq!(notes(&c, &GATE), ["they nodded 3 times", "they looked away more than half the time"]);
        assert!(GATE.experimental(), "nothing has been tested yet");
        let tested = Gate { passed: &["on_camera", "nodded", "nod_count"], failed: &["looked_away"], hidden_below: &[] };
        assert_eq!(notes(&c, &tested), ["they nodded 3 times"], "a failed check isn't shown");
        assert!(!tested.experimental(), "everything shown has passed");
        let small = Gate { passed: &[], failed: &[], hidden_below: &[("nod_count", 0.08)] };
        assert_eq!(notes(&c, &small), ["they nodded", "they looked away more than half the time"],
                   "too small to count nods, so only whether they nodded");
        let none = Gate { passed: &[], failed: &["nod_count", "nodded"], hidden_below: &[] };
        let s = Summary { usually_on_camera: 2, nods: 5, answers: 4, answers_with_nods: 3 };
        assert_eq!(sentence(&s, &none), "Usually 2 people on camera besides you.");
    }

    #[test]
    fn faces_json_from_the_tool_parses() {
        let json = r#"{"version":1,"tool":"vision-faces-v1","video":"video.mov","fps":6,"width":1280,"height":720,"duration_s":1,
                      "samples":[{"t":0,"faces":[]},{"t":0.2,"faces":[{"x":0.1,"y":0.2,"w":0.1,"h":0.2,"yaw":-3.5,"pitch":null,"roll":1,"mouth":0.04,"conf":0.98}]}]}"#;
        let f: Faces = serde_json::from_str(json).unwrap();
        assert_eq!(f.samples[1].faces[0].pitch, None);
        assert_eq!(f.samples[1].faces[0].yaw, Some(-3.5));
        assert!(f.complete, "files from before `complete` existed were complete");
        let cut: Faces = serde_json::from_str(r#"{"version":1,"fps":6,"samples":[],"complete":false,"skipped":2}"#).unwrap();
        assert!(!cut.complete);
        assert_eq!(cut.skipped, 2);
    }

    /// Timelines stored before `face_h` existed still load.
    #[test]
    fn older_cues_load() {
        let c: VideoCues = serde_json::from_str(r#"{"on_camera":1.0,"nods":2,"looking_away":null,"seen_s":12.0}"#).unwrap();
        assert_eq!(c.face_h, None);
    }
}
