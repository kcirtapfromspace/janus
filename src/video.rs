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
//! Experimental (`METHOD`): thresholds are set from first principles, not yet from labelled calls.
//! Tracks break when the call's layout changes (speaker view swaps who is in the big tile), and a
//! small tile hides small nods.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::models::{Segment, YOU};
use crate::progress::Progress;
use crate::temperature::{Kind, Signal};
use crate::tools::Tool;

/// The cue formulas below, versioned so stored timelines say how they were computed.
pub const METHOD: &str = "video-v1";
/// What the recorder writes (see mac/Sources/ICRecorderCore/ScreenCapture.swift).
pub const VIDEO_FILE: &str = "video.mov";
pub const FACES_FILE: &str = "faces.json";
/// Samples per second `ic-vision` looks at: nods take about half a second.
const SAMPLES_PER_S: f64 = 6.0;

/// Faces smaller than this (a fraction of the frame's height) are too small to read.
const MIN_FACE_H: f64 = 0.04;
/// A face in the next sample continues a track when their boxes overlap at least this much.
const MATCH_IOU: f64 = 0.3;
/// A track that hasn't been seen for this long has ended.
const TRACK_GAP_S: f64 = 2.0;
/// Mouth-movement pairs a track needs while you speak, and while they speak, to be labelled.
const MIN_LABEL_PAIRS: usize = 12;
/// How much more a track's mouth must move while you speak than while they do to be yours.
const YOU_MARGIN: f64 = 0.25;
/// A nod: the head drops at least this much (in face heights) and comes back up…
const NOD_DROP: f64 = 0.06;
/// …within this long on each side of its lowest point.
const NOD_WINDOW_S: f64 = 0.8;
/// Lowest points closer together than this are one nod.
const NOD_GAP_S: f64 = 0.35;
/// Turned at least this far (degrees) is looking away: aside at another screen, or down at notes.
const AWAY_YAW: f64 = 30.0;
const AWAY_PITCH: f64 = 25.0;
/// An answer needs this much video for cues.
const MIN_SEEN_S: f64 = 3.0;

#[derive(Debug, Clone, Deserialize)]
pub struct Faces {
    pub version: u32,
    /// Samples per second asked for.
    pub fps: f64,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sample {
    /// Seconds from the start of the recording (the video starts with the audio).
    pub t: f64,
    pub faces: Vec<Face>,
}

/// One face in one sample: its box as fractions of the frame from the top-left, its angles in
/// degrees, and how open its mouth is (a fraction of the face's height).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
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
}

/// Run `ic-vision` on the recording's video, writing `out`.
pub fn extract(video: &Path, out: &Path, progress: &mut dyn Progress) -> Result<()> {
    let mut child = Tool::Vision
        .command()?
        .arg("faces")
        .arg("--video")
        .arg(video)
        .arg("--out")
        .arg(out)
        .args(["--fps", &SAMPLES_PER_S.to_string(), "--progress"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting ic-vision")?;
    let stdout = child.stdout.take().context("ic-vision's output")?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(done) = event["progress"].as_f64()
        {
            progress.step((done * 1000.0).round() as u64, 1000);
        }
    }
    let finished = child.wait_with_output()?;
    if !finished.status.success() {
        bail!("ic-vision couldn't read the video: {}", String::from_utf8_lossy(&finished.stderr).trim());
    }
    Ok(())
}

/// The session's faces, if its video has been read.
pub fn load(dir: &Path) -> Result<Option<Faces>> {
    let path = dir.join(FACES_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let faces: Faces = serde_json::from_str(&std::fs::read_to_string(&path)?).with_context(|| format!("reading {}", path.display()))?;
    if faces.version != 1 {
        bail!("{} is version {}; this ic reads version 1", path.display(), faces.version);
    }
    Ok(Some(faces))
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
pub fn tracks(faces: &Faces) -> Vec<Track> {
    let mut tracks: Vec<Track> = vec![];
    let mut active: Vec<usize> = vec![];
    for sample in &faces.samples {
        active.retain(|&i| tracks[i].points.last().is_some_and(|(t, _)| sample.t - t <= TRACK_GAP_S));
        let seen: Vec<&Face> = sample.faces.iter().filter(|f| f.h >= MIN_FACE_H).collect();
        let mut pairs: Vec<(f64, usize, usize)> = vec![];
        for (f, face) in seen.iter().enumerate() {
            for (a, &track) in active.iter().enumerate() {
                let overlap = iou(face, &tracks[track].points.last().expect("tracks start with a point").1);
                if overlap >= MATCH_IOU {
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

/// When you spoke and when someone else did, from the transcript.
pub struct Speech {
    you: Vec<(f64, f64)>,
    them: Vec<(f64, f64)>,
}

impl Speech {
    pub fn from_segments(segments: &[Segment]) -> Self {
        let spans = |you: bool| {
            let mut spans: Vec<(f64, f64)> =
                segments.iter().filter(|s| (s.speaker == YOU) == you && !s.text.trim().is_empty()).map(|s| (s.start, s.end)).collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            spans
        };
        Speech { you: spans(true), them: spans(false) }
    }

    fn during(spans: &[(f64, f64)], t: f64) -> bool {
        // Spans are sorted by start; the last one starting at or before t is the only candidate,
        // unless spans overlap, so check back while they could still cover t.
        let i = spans.partition_point(|s| s.0 <= t);
        spans[..i].iter().rev().take(4).any(|s| t <= s.1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    You,
    /// Someone else, or a face that couldn't be told apart (it's never counted as you).
    Other,
}

/// Your face is the one whose mouth moves more while you speak than while they do. A face needs
/// enough of both to be labelled; otherwise it's someone else.
pub fn who(track: &Track, speech: &Speech) -> Who {
    let (mut you, mut them) = ((0.0, 0usize), (0.0, 0usize));
    for pair in track.points.windows(2) {
        let ((t0, a), (t1, b)) = (pair[0], pair[1]);
        let (Some(ma), Some(mb)) = (a.mouth, b.mouth) else { continue };
        if t1 - t0 > 0.6 {
            continue;
        }
        let t = (t0 + t1) / 2.0;
        let motion = (mb - ma).abs();
        match (Speech::during(&speech.you, t), Speech::during(&speech.them, t)) {
            (true, false) => you = (you.0 + motion, you.1 + 1),
            (false, true) => them = (them.0 + motion, them.1 + 1),
            _ => {}
        }
    }
    if you.1 < MIN_LABEL_PAIRS || them.1 < MIN_LABEL_PAIRS {
        return Who::Other;
    }
    let (a, b) = (you.0 / you.1 as f64, them.0 / them.1 as f64);
    if a > 0.0 && (a - b) / (a + b) >= YOU_MARGIN { Who::You } else { Who::Other }
}

// --- cues -----------------------------------------------------------------------------------------

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// When a track's head dipped and came back up. Its height in the frame is measured in its own
/// face heights, so a big tile and a small one nod alike; a dip that moves sideways as much as
/// down is someone shifting in their seat, not a nod.
pub fn nods(track: &Track) -> Vec<f64> {
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
        let before = v[..i].iter().rev().take_while(|p| t - p.0 <= NOD_WINDOW_S).min_by(|a, b| a.1.total_cmp(&b.1));
        let after = v[i + 1..].iter().take_while(|p| p.0 - t <= NOD_WINDOW_S).min_by(|a, b| a.1.total_cmp(&b.1));
        let (Some(before), Some(after)) = (before, after) else { continue };
        let drop = (y - before.1).min(y - after.1);
        let sideways = (x - before.2).abs().max((x - after.2).abs());
        if drop >= NOD_DROP && sideways < drop && out.last().is_none_or(|&last| t - last >= NOD_GAP_S) {
            out.push(t);
        }
    }
    out
}

fn away(face: &Face) -> Option<bool> {
    let yaw = face.yaw?;
    Some(yaw.abs() >= AWAY_YAW || face.pitch.is_some_and(|p| p.abs() >= AWAY_PITCH))
}

/// The other people's cues between `start` and `end`; None when the video covers too little of it.
pub fn cues(faces: &Faces, others: &[(&Track, Vec<f64>)], start: f64, end: f64) -> Option<VideoCues> {
    let within = |t: f64| t >= start && t <= end;
    let samples = faces.samples.iter().filter(|s| within(s.t)).count();
    let seen_s = samples as f64 / faces.fps.max(1e-6);
    if seen_s < MIN_SEEN_S.min((end - start) * 0.5) || samples == 0 {
        return None;
    }
    let (mut present, mut turned, mut angled, mut nodded) = (0usize, 0usize, 0usize, 0usize);
    for (track, nods) in others {
        for (_, face) in track.points.iter().filter(|(t, _)| within(*t)) {
            present += 1;
            if let Some(a) = away(face) {
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
    })
}

/// Add the others' video cues to each of your answers.
pub fn annotate(signals: &mut [Signal], faces: &Faces, segments: &[Segment]) {
    let speech = Speech::from_segments(segments);
    let tracks = tracks(faces);
    let others: Vec<(&Track, Vec<f64>)> = tracks.iter().filter(|t| who(t, &speech) == Who::Other).map(|t| (t, nods(t))).collect();
    for s in signals.iter_mut().filter(|s| s.kind == Kind::Answer) {
        s.video = cues(faces, &others, s.start, s.end);
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

/// What stood out in one answer's video, in words: "they nodded 3 times", "looked away about half the time".
pub fn notes(cues: &VideoCues) -> Vec<String> {
    let mut out = vec![];
    if cues.on_camera < 0.5 {
        out.push("nobody else was on camera".to_string());
        return out;
    }
    match cues.nods {
        0 => {}
        1 => out.push("they nodded once".into()),
        n => out.push(format!("they nodded {n} times")),
    }
    if let Some(away) = cues.looking_away.filter(|a| *a >= 0.4) {
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
        Faces { version: 1, fps: 6.0, samples: (0..n).map(|i| i as f64 / 6.0).map(|t| Sample { t, faces: at(t) }).collect() }
    }

    fn talking(t: f64) -> f64 {
        if (t * 6.0).round() as i64 % 2 == 0 { 0.02 } else { 0.25 }
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
        let t = tracks(&v);
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
        let t = tracks(&v);
        assert_eq!(t.len(), 2, "the face that was gone for 3 s comes back as a new track");
        assert!(t.iter().all(|t| t.points.iter().all(|(_, f)| f.h >= MIN_FACE_H)));
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
        let t = tracks(&v);
        assert_eq!(t.iter().map(|t| who(t, &speech)).collect::<Vec<_>>(), [Who::You, Who::Other]);
        let silent = Track { points: t[0].points.iter().take(30).copied().collect() };
        assert_eq!(who(&silent, &speech), Who::Other, "too little of both to call it yours");
    }

    #[test]
    fn a_dip_and_return_is_a_nod_and_a_slow_drift_or_a_shift_is_not() {
        let head = |ys: &[(f64, f64)]| Track { points: ys.iter().map(|&(t, y)| (t, face(0.4, y, 0.05))).collect() };
        // Two nods: the head drops 0.025 of the frame (10% of its 0.25 height) and comes back.
        let nodding: Vec<(f64, f64)> = (0..60).map(|i| {
            let t = i as f64 / 6.0;
            (t, if (t - 3.0).abs() < 0.1 || (t - 6.0).abs() < 0.1 { 0.225 } else { 0.2 })
        }).collect();
        assert_eq!(nods(&head(&nodding)), [3.0, 6.0]);
        let drifting: Vec<(f64, f64)> = (0..60).map(|i| (i as f64 / 6.0, 0.2 + i as f64 * 0.001)).collect();
        assert!(nods(&head(&drifting)).is_empty(), "slowly sinking isn't nodding");
        let jitter: Vec<(f64, f64)> = (0..60).map(|i| (i as f64 / 6.0, 0.2 + if i % 3 == 0 { 0.004 } else { 0.0 })).collect();
        assert!(nods(&head(&jitter)).is_empty(), "the detector's wobble isn't nodding");
        let shifting = Track { points: (0..60).map(|i| {
            let t = i as f64 / 6.0;
            let moved = (t - 3.0).abs() < 0.1;
            (t, face(if moved { 0.46 } else { 0.4 }, if moved { 0.225 } else { 0.2 }, 0.05))
        }).collect() };
        assert!(nods(&shifting).is_empty(), "leaning to the side isn't a nod");
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
        assert!(signals.iter().filter(|s| s.kind != Kind::Answer).all(|s| s.video.is_none()));
        assert_eq!(notes(&answer), ["they nodded 3 times"]);
        let summary = summary(&signals).unwrap();
        assert_eq!((summary.usually_on_camera, summary.nods, summary.answers_with_nods), (1, 3, 1));
    }

    #[test]
    fn too_little_video_gives_no_cues_and_an_empty_call_says_so() {
        let v = video(2.0, |_| vec![]);
        assert_eq!(cues(&v, &[], 0.0, 30.0), None, "2 s of a 30 s answer");
        let empty = video(30.0, |_| vec![]);
        let c = cues(&empty, &[], 0.0, 30.0).unwrap();
        assert_eq!(c.on_camera, 0.0);
        assert_eq!(notes(&c), ["nobody else was on camera"]);
    }

    #[test]
    fn faces_json_from_the_tool_parses() {
        let json = r#"{"version":1,"tool":"vision-faces-v1","video":"video.mov","fps":6,"width":1280,"height":720,"duration_s":1,
                      "samples":[{"t":0,"faces":[]},{"t":0.2,"faces":[{"x":0.1,"y":0.2,"w":0.1,"h":0.2,"yaw":-3.5,"pitch":null,"roll":1,"mouth":0.04,"conf":0.98}]}]}"#;
        let f: Faces = serde_json::from_str(json).unwrap();
        assert_eq!(f.samples[1].faces[0].pitch, None);
        assert_eq!(f.samples[1].faces[0].yaw, Some(-3.5));
    }
}
