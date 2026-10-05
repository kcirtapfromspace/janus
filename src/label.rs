//! Labelling the call's video for docs/eval/video-decision.md. `ic eval clips` cuts interviews into
//! the clips the rule scores; `ic eval label` serves a page on this Mac that loops each clip and
//! records what a person sees, blind to anything Janus measured.
//!
//! Labels stay outside the repository (by default ~/InterviewCoach/eval/video): `clips.jsonl` holds
//! one `eval::Item` per clip, and `you.jsonl` where your own face is in each interview.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::{Map, Value, json};

use crate::models::{Segment, Session};
use crate::temperature::{self, Kind};
use crate::video::{self, Faces};

/// A clip's length, and the shortest answer that gets one.
pub const CLIP_S: f64 = 20.0;
pub const MIN_ANSWER_S: f64 = 10.0;
pub const MAX_CLIPS_PER_ANSWER: usize = 3;
pub const CLIPS_FILE: &str = "clips.jsonl";
pub const YOU_FILE: &str = "you.jsonl";

/// What each clip is labelled with (docs/eval/video-decision.md). A check left out is one the
/// labeller couldn't tell.
pub const YES_NO: [&str; 2] = ["nodded", "smiled"];
pub const CHOICES: [(&str, &[&str]); 3] = [
    ("nod_count", &["none", "1-2", "3-5", "6+"]),
    ("looked_away", &["rarely", "sometimes", "mostly"]),
    ("on_camera", &["0", "1", "2", "3+"]),
];
pub const LAYOUTS: [&str; 3] = ["gallery", "speaker", "other"];

const PAGE: &str = include_str!("label.html");
/// The most video sent per request: the browser asks for the next piece as it plays.
const VIDEO_CHUNK: u64 = 4 << 20;
const MAX_HEAD: u64 = 16 << 10;
const MAX_BODY: usize = 64 << 10;

pub fn default_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("eval/video")
}

fn tenth(t: f64) -> f64 {
    (t * 10.0).round() / 10.0
}

/// Where one answer's clips fall: none under 10 s, the whole answer up to 20 s, then one, two or
/// three 20-second clips that don't overlap (the middle; the start and end; all three).
pub fn windows(start: f64, end: f64) -> Vec<(f64, f64)> {
    let len = end - start;
    if len < MIN_ANSWER_S {
        return vec![];
    }
    if len <= CLIP_S {
        return vec![(tenth(start), tenth(end))];
    }
    let middle = start + (len - CLIP_S) / 2.0;
    let starts = match ((len / CLIP_S).floor() as usize).min(MAX_CLIPS_PER_ANSWER) {
        1 => vec![middle],
        2 => vec![start, end - CLIP_S],
        _ => vec![start, middle, end - CLIP_S],
    };
    starts.into_iter().map(|s| (tenth(s), tenth(s + CLIP_S))).collect()
}

/// The faces' typical height in a window, as a fraction of the frame: the tile-size band the rule
/// breaks results down by.
fn face_height(faces: &Faces, start: f64, end: f64) -> Option<f64> {
    let mut heights: Vec<f64> = faces
        .samples
        .iter()
        .filter(|s| s.t >= start && s.t <= end)
        .flat_map(|s| s.faces.iter().map(|f| f.h))
        .collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(f64::total_cmp);
    Some((heights[heights.len() / 2] * 1000.0).round() / 1000.0)
}

/// One interview's clips, unlabelled: `eval::Item`s whose input is the session and the window.
pub fn session_clips(session: &Session, segments: &[Segment], faces: Option<&Faces>, origin: &str) -> Vec<Value> {
    let set = format!("s{:03}", session.id);
    let answers = temperature::conversation(segments).into_iter().filter(|t| t.kind == Kind::Answer);
    let mut out = vec![];
    for (n, answer) in answers.enumerate() {
        for (k, (start, end)) in windows(answer.start, answer.end).into_iter().enumerate() {
            let variant = format!("a{:02}-c{}", n + 1, k + 1);
            out.push(json!({
                "id": format!("{set}-{variant}"), "set": set, "variant": variant, "origin": origin,
                "title": session.title, "session_dir": session.dir, "start": start, "end": end,
                "face_height": faces.and_then(|f| face_height(f, start, end)), "layout": null, "labels": {},
            }));
        }
    }
    out
}

/// Check a clip's labels against the conventions, including that they agree with each other.
pub fn check_labels(labels: &Map<String, Value>) -> Result<()> {
    for (key, value) in labels {
        if YES_NO.contains(&key.as_str()) {
            if !value.is_boolean() {
                bail!("{key} is yes or no");
            }
        } else if let Some((_, options)) = CHOICES.iter().find(|(id, _)| id == key) {
            if !value.as_str().is_some_and(|v| options.contains(&v)) {
                bail!("{key} is one of {}", options.join(", "));
            }
        } else {
            bail!("there's no check called {key}");
        }
    }
    let yes = |k: &str| labels.get(k).and_then(Value::as_bool);
    let pick = |k: &str| labels.get(k).and_then(Value::as_str);
    match (yes("nodded"), pick("nod_count")) {
        (Some(false), Some(count)) if count != "none" => bail!("No nod, but {count} nods: change one of them"),
        (Some(true), Some("none")) => bail!("A nod, but none counted: change one of them"),
        _ => {}
    }
    if pick("on_camera") == Some("0") && (yes("nodded") == Some(true) || yes("smiled") == Some(true)) {
        bail!("Nobody else was on camera, so nobody else could nod or smile");
    }
    Ok(())
}

// --- the files --------------------------------------------------------------------------------------

/// The label files. Every change rewrites a file whole, through a temporary file, so an
/// interrupted save never leaves half a line. Changes hold an OS lock on the folder's `.lock`, so
/// `ic eval clips` adding interviews while `ic eval label` saves labels can't undo either.
pub struct Store {
    dir: PathBuf,
}

/// What adding an interview's clips did.
#[derive(Debug, Default, PartialEq)]
pub struct Added {
    pub new: usize,
    /// Clips already listed whose window has since moved (the transcript was re-run). Their labels
    /// describe the old window, so they're kept as they are and reported.
    pub moved: Vec<String>,
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Store { dir: dir.to_path_buf() }
    }

    /// Held until dropped.
    fn lock(&self) -> Result<std::fs::File> {
        std::fs::create_dir_all(&self.dir)?;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join(".lock"))?;
        file.lock_exclusive().context("locking the label files")?;
        Ok(file)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn read(&self, file: &str) -> Result<Vec<Value>> {
        let path = self.dir.join(file);
        if !path.exists() {
            return Ok(vec![]);
        }
        std::fs::read_to_string(&path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).with_context(|| format!("bad line in {}: {l}", path.display())))
            .collect()
    }

    fn write(&self, file: &str, lines: &[Value]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
        for line in lines {
            writeln!(tmp, "{}", serde_json::to_string(line)?)?;
        }
        tmp.persist(self.dir.join(file))?;
        Ok(())
    }

    /// Add clips not already listed (by id), keeping every label.
    pub fn add_clips(&self, clips: Vec<Value>) -> Result<Added> {
        let _held = self.lock()?;
        let mut lines = self.read(CLIPS_FILE)?;
        let mut added = Added::default();
        for clip in clips {
            match lines.iter().find(|l| l["id"] == clip["id"]) {
                Some(listed) if listed["start"] != clip["start"] || listed["end"] != clip["end"] => {
                    added.moved.push(clip["id"].as_str().unwrap_or_default().to_string());
                }
                Some(_) => {}
                None => {
                    lines.push(clip);
                    added.new += 1;
                }
            }
        }
        if added.new > 0 {
            self.write(CLIPS_FILE, &lines)?;
        }
        Ok(added)
    }

    /// Save one clip's labels and layout. Returns the clip as stored.
    pub fn label(&self, id: &str, labels: Map<String, Value>, layout: &str) -> Result<Value> {
        check_labels(&labels)?;
        if !LAYOUTS.contains(&layout) {
            bail!("layout is one of {}", LAYOUTS.join(", "));
        }
        let _held = self.lock()?;
        let mut lines = self.read(CLIPS_FILE)?;
        let clip = lines.iter_mut().find(|l| l["id"] == id).with_context(|| format!("there's no clip {id}"))?;
        clip["labels"] = Value::Object(labels);
        clip["layout"] = layout.into();
        clip["labelled_at"] = crate::db::now_iso().into();
        let saved = clip.clone();
        self.write(CLIPS_FILE, &lines)?;
        Ok(saved)
    }

    /// Where your face is in one interview: a point on it at a moment it shows, or that it never
    /// does (camera off). One line per interview, replaced when marked again.
    pub fn mark_you(&self, set: &str, at: Option<(f64, f64, f64)>) -> Result<Value> {
        if let Some((t, x, y)) = at
            && !(t >= 0.0 && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y))
        {
            bail!("the point must be inside the video");
        }
        let _held = self.lock()?;
        let clips = self.read(CLIPS_FILE)?;
        let clip = clips.iter().find(|c| c["set"] == set).with_context(|| format!("there's no interview {set}"))?;
        let (t, x, y) = at.map_or((Value::Null, Value::Null, Value::Null), |(t, x, y)| {
            ((tenth(t)).into(), ((x * 1000.0).round() / 1000.0).into(), ((y * 1000.0).round() / 1000.0).into())
        });
        let line = json!({
            "id": format!("{set}-you"), "set": set, "variant": "you", "origin": clip["origin"],
            "session_dir": clip["session_dir"], "at": t, "x": x, "y": y,
            "labels": {"you_visible": at.is_some()}, "labelled_at": crate::db::now_iso(),
        });
        let mut lines: Vec<Value> = self.read(YOU_FILE)?.into_iter().filter(|l| l["set"] != set).collect();
        lines.push(line.clone());
        self.write(YOU_FILE, &lines)?;
        Ok(line)
    }

    /// What the page shows: every clip (without its folder) and where your face is per interview.
    fn state(&self) -> Result<Value> {
        let clips: Vec<Value> = self
            .read(CLIPS_FILE)?
            .into_iter()
            .map(|c| {
                json!({
                    "id": c["id"], "set": c["set"], "variant": c["variant"], "title": c["title"],
                    "start": c["start"], "end": c["end"], "layout": c["layout"], "labels": c["labels"],
                    "labelled": !c["labelled_at"].is_null(),
                })
            })
            .collect();
        let you: Map<String, Value> = self
            .read(YOU_FILE)?
            .into_iter()
            .filter_map(|l| {
                let set = l["set"].as_str()?.to_string();
                Some((set, json!({"visible": l["labels"]["you_visible"], "at": l["at"], "x": l["x"], "y": l["y"]})))
            })
            .collect();
        Ok(json!({"clips": clips, "you": you}))
    }

    /// The video of interview `set`, found through its clips: the page never names a file.
    fn video(&self, set: &str) -> Result<PathBuf> {
        let clips = self.read(CLIPS_FILE)?;
        let dir = clips
            .iter()
            .find(|c| c["set"] == set)
            .and_then(|c| c["session_dir"].as_str())
            .with_context(|| format!("there's no interview {set}"))?;
        let path = Path::new(dir).join(video::VIDEO_FILE);
        if !path.is_file() {
            bail!("{} is missing", path.display());
        }
        Ok(path)
    }
}

// --- the page's server ------------------------------------------------------------------------------

/// Serves the labelling page to this Mac only. Every request needs the random token from the
/// printed link and a 127.0.0.1 or localhost Host, so other pages in the browser can't read or
/// change the labels.
pub struct Server {
    store: Store,
    token: String,
    port: u16,
}

impl Server {
    pub fn new(dir: &Path, token: &str, port: u16) -> Self {
        Server { store: Store::new(dir), token: token.to_string(), port }
    }

    /// Answer requests until the process ends (Ctrl+C), one thread per connection.
    pub fn run(self: Arc<Self>, listener: TcpListener) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let server = self.clone();
            std::thread::spawn(move || {
                let _ = server.handle(stream);
            });
        }
    }

    fn handle(&self, mut stream: TcpStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let Ok(req) = Request::read(&stream) else {
            return respond(&mut stream, 400, "text/plain", b"Bad request", &[]);
        };
        if req.path == "/favicon.ico" {
            return respond(&mut stream, 404, "text/plain", b"", &[]);
        }
        let hosts = [format!("127.0.0.1:{}", self.port), format!("localhost:{}", self.port)];
        let host_ok = req.headers.get("host").is_some_and(|h| hosts.contains(h));
        let token_ok = req.query.get("t").or(req.headers.get("x-janus-token")) == Some(&self.token);
        if !host_ok || !token_ok {
            return respond(&mut stream, 403, "text/plain", b"Open the link `ic eval label` printed.", &[]);
        }
        let result = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => return respond(&mut stream, 200, "text/html; charset=utf-8", PAGE.as_bytes(), &[]),
            ("GET", "/api/state") => self.store.state(),
            ("POST", "/api/label") => self.post_label(&req.body),
            ("POST", "/api/you") => self.post_you(&req.body),
            ("GET", path) if path.starts_with("/video/") => {
                return match self.store.video(&path["/video/".len()..]) {
                    Ok(file) => send_video(&mut stream, &file, req.headers.get("range").map(String::as_str)),
                    Err(e) => respond(&mut stream, 404, "text/plain", format!("{e:#}").as_bytes(), &[]),
                };
            }
            _ => return respond(&mut stream, 404, "text/plain", b"Not found", &[]),
        };
        match result {
            Ok(value) => respond(&mut stream, 200, "application/json", value.to_string().as_bytes(), &[]),
            Err(e) => respond(&mut stream, 400, "text/plain; charset=utf-8", format!("{e:#}").as_bytes(), &[]),
        }
    }

    fn post_label(&self, body: &[u8]) -> Result<Value> {
        let v: Value = serde_json::from_slice(body).context("the labels aren't JSON")?;
        let id = v["id"].as_str().context("which clip?")?;
        let labels = v["labels"].as_object().cloned().unwrap_or_default();
        self.store.label(id, labels, v["layout"].as_str().unwrap_or_default())
    }

    fn post_you(&self, body: &[u8]) -> Result<Value> {
        let v: Value = serde_json::from_slice(body).context("the mark isn't JSON")?;
        let set = v["set"].as_str().context("which interview?")?;
        let at = match v["visible"].as_bool() {
            Some(false) => None,
            _ => Some((
                v["at"].as_f64().context("when?")?,
                v["x"].as_f64().context("where?")?,
                v["y"].as_f64().context("where?")?,
            )),
        };
        self.store.mark_you(set, at)
    }
}

/// Serve the labelling page for the clips in `dir`, printing (and, with `open`, opening) its link.
pub fn serve(dir: &Path, port: u16, open: bool) -> Result<()> {
    let store = Store::new(dir);
    let clips = store.read(CLIPS_FILE)?;
    if clips.is_empty() {
        bail!("There are no clips to label in {}. Add some first: ic eval clips <interview id>", dir.display());
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).context("starting the labelling page")?;
    let port = listener.local_addr()?.port();
    let token = format!("{:032x}", rand::random::<u128>());
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    let done = clips.iter().filter(|c| !c["labelled_at"].is_null()).count();
    let count = if clips.len() == 1 { "1 clip".to_string() } else { format!("{} clips", clips.len()) };
    outln!("Labelling {count} ({done} done) in {}", dir.display());
    outln!("Open {url}\nPress Ctrl+C here when you're finished; every label is saved as you go.");
    if open {
        let _ = std::process::Command::new("open").arg(&url).status();
    }
    Arc::new(Server::new(dir, &token, port)).run(listener);
    Ok(())
}

// --- HTTP, just enough for one local page -------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn read(stream: &TcpStream) -> Result<Request> {
        let mut reader = BufReader::new(stream.take(MAX_HEAD + MAX_BODY as u64));
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let mut parts = line.split_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else { bail!("no request line") };
        if !target.starts_with('/') {
            bail!("bad target");
        }
        let method = method.to_string();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let path = path.to_string();
        let query = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let length: usize = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
        if length > MAX_BODY {
            bail!("body too large");
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        Ok(Request { method, path, query, headers, body })
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8], extra: &[(&str, String)]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

/// The bytes a Range header asks for, inclusive, at most `VIDEO_CHUNK` of them; None when it's
/// unsatisfiable. No header means from the start.
pub fn byte_range(header: Option<&str>, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let (start, end) = match header.and_then(|h| h.strip_prefix("bytes=")) {
        None => (0, len - 1),
        Some(spec) => {
            let (a, b) = spec.split(',').next()?.split_once('-')?;
            match (a.trim().parse::<u64>().ok(), b.trim().parse::<u64>().ok()) {
                (Some(a), Some(b)) => (a, b.min(len - 1)),
                (Some(a), None) => (a, len - 1),
                (None, Some(n)) if n > 0 => (len.saturating_sub(n), len - 1),
                _ => return None,
            }
        }
    };
    (start <= end && start < len).then(|| (start, end.min(start + VIDEO_CHUNK - 1)))
}

/// Video in pieces (206 Partial Content), so the browser can seek straight to a clip.
fn send_video(stream: &mut TcpStream, path: &Path, range: Option<&str>) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let Some((start, end)) = byte_range(range, len) else {
        return respond(stream, 416, "text/plain", b"", &[("Content-Range", format!("bytes */{len}"))]);
    };
    let mut body = vec![0; (end - start + 1) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut body)?;
    respond(stream, 206, "video/mp4", &body, &[
        ("Accept-Ranges", "bytes".into()),
        ("Content-Range", format!("bytes {start}-{end}/{len}")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{INTERVIEWER, YOU};

    #[test]
    fn answers_become_clips_that_never_overlap() {
        assert!(windows(0.0, 8.0).is_empty(), "too short to label");
        assert_eq!(windows(5.0, 20.0), [(5.0, 20.0)], "the whole answer");
        assert_eq!(windows(0.0, 30.0), [(5.0, 25.0)], "the middle");
        assert_eq!(windows(0.0, 45.0), [(0.0, 20.0), (25.0, 45.0)], "the start and end");
        assert_eq!(windows(10.0, 100.0), [(10.0, 30.0), (45.0, 65.0), (80.0, 100.0)], "start, middle, end");
        for (s, e) in [(0.0, 41.0), (3.3, 63.4), (0.0, 600.0)] {
            let w = windows(s, e);
            assert!(w.len() <= MAX_CLIPS_PER_ANSWER);
            assert!(w.windows(2).all(|p| p[0].1 <= p[1].0), "{w:?}");
            assert!(w.iter().all(|&(a, b)| a >= s - 0.05 && b <= e + 0.05), "{w:?} inside {s}–{e}");
        }
    }

    fn session() -> Session {
        use crate::models::{Mode, Source, Status};
        Session {
            id: 3, created_at: "2026-10-05T00:00:00+00:00".into(), title: "Acme HM".into(), company: None, stage: None,
            role_id: None, source: Source::Recording, mode: Mode::Dual, source_path: None, dir: "/tmp/s3".into(),
            duration_s: Some(100.0), num_speakers: None, consent: Some(true), status: Status::Analyzed, error: None,
            archived_at: None, deleted_at: None, role_set: false,
        }
    }

    #[test]
    fn an_interview_gives_one_item_per_clip_with_the_faces_size() {
        let segments = [
            Segment::new(0.0, 4.0, "Tell me about a project you're proud of.", INTERVIEWER),
            Segment::new(5.0, 50.0, "I rebuilt our checkout and conversion went up.", YOU),
            Segment::new(51.0, 54.0, "That's great. How did you test it?", INTERVIEWER),
            Segment::new(55.0, 60.0, "With an A/B test.", YOU),
        ];
        let faces: Faces = serde_json::from_value(json!({"version": 1, "fps": 6, "samples": [
            {"t": 6.0, "faces": [{"x": 0.1, "y": 0.1, "w": 0.1, "h": 0.2}, {"x": 0.6, "y": 0.1, "w": 0.1, "h": 0.12}]},
            {"t": 7.0, "faces": [{"x": 0.1, "y": 0.1, "w": 0.1, "h": 0.2}]}]}))
        .unwrap();
        let clips = session_clips(&session(), &segments, Some(&faces), "designed");
        let ids: Vec<&str> = clips.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["s003-a01-c1", "s003-a01-c2"], "a 45 s answer gives two clips; a 5 s one none");
        assert_eq!(clips[0]["start"], 5.0);
        assert_eq!(clips[0]["face_height"], 0.2);
        assert_eq!(clips[1]["face_height"], Value::Null, "no faces in that window");
        assert_eq!(clips[0]["origin"], "designed");
        assert_eq!(clips[0]["session_dir"], "/tmp/s3");
        let item: crate::eval::Item = serde_json::from_value(clips[0].clone()).unwrap();
        assert_eq!(item.expected("nodded"), None, "unlabelled, and still a valid eval item");
    }

    fn labels(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn labels_follow_the_conventions() {
        assert!(check_labels(&labels(json!({"nodded": true, "nod_count": "1-2", "looked_away": "rarely", "on_camera": "2", "smiled": false}))).is_ok());
        assert!(check_labels(&labels(json!({}))).is_ok(), "every check can be 'can't tell'");
        for bad in [
            json!({"nodded": "yes"}),
            json!({"nod_count": "7"}),
            json!({"frowned": true}),
            json!({"nodded": false, "nod_count": "3-5"}),
            json!({"nodded": true, "nod_count": "none"}),
            json!({"on_camera": "0", "smiled": true}),
        ] {
            assert!(check_labels(&labels(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn saving_keeps_every_other_line_and_marks_you_once_per_interview() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        let clip = |id: &str| json!({"id": id, "set": "s003", "variant": "v", "origin": "designed", "session_dir": "/tmp/s3",
                                     "start": 0.0, "end": 20.0, "layout": null, "labels": {}, "extra": "kept"});
        assert_eq!(store.add_clips(vec![clip("a"), clip("b")]).unwrap().new, 2);
        assert_eq!(store.add_clips(vec![clip("b"), clip("c")]).unwrap(), Added { new: 1, moved: vec![] }, "b is already there");
        let mut moved = clip("a");
        moved["start"] = json!(3.0);
        assert_eq!(store.add_clips(vec![moved]).unwrap(), Added { new: 0, moved: vec!["a".into()] }, "a re-run transcript moved it");
        let saved = store.label("b", labels(json!({"nodded": true, "nod_count": "3-5"})), "gallery").unwrap();
        assert_eq!(saved["layout"], "gallery");
        assert!(saved["labelled_at"].is_string());
        assert!(store.label("b", labels(json!({})), "grid").is_err(), "an unknown layout");
        assert!(store.label("zzz", labels(json!({})), "gallery").is_err(), "an unknown clip");
        let lines = store.read(CLIPS_FILE).unwrap();
        assert_eq!(lines.iter().map(|l| l["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(lines[1]["extra"], "kept");
        assert!(lines[0]["labelled_at"].is_null());

        store.mark_you("s003", Some((95.04, 0.78, 0.6))).unwrap();
        store.mark_you("s003", None).unwrap();
        let you = store.read(YOU_FILE).unwrap();
        assert_eq!(you.len(), 1, "marking again replaces the mark");
        assert_eq!(you[0]["labels"]["you_visible"], false);
        assert!(store.mark_you("s003", Some((5.0, 1.4, 0.5))).is_err(), "outside the video");
        assert!(store.mark_you("s999", None).is_err());
    }

    /// Separate stores on one folder stand in for `ic eval clips` and a running `ic eval label`:
    /// with the folder locked, no change undoes another.
    #[test]
    fn two_processes_saving_at_once_lose_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        Store::new(&dir)
            .add_clips(vec![json!({"id": "x", "set": "s001", "variant": "v", "origin": "designed", "session_dir": "/tmp/s1",
                                   "start": 0.0, "end": 20.0, "layout": null, "labels": {}})])
            .unwrap();
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let store = Store::new(&dir);
                    if i % 2 == 0 {
                        store.add_clips(vec![json!({"id": format!("c{i}"), "set": "s001", "variant": "v", "origin": "designed",
                                                    "session_dir": "/tmp/s1", "start": 0.0, "end": 20.0, "layout": null,
                                                    "labels": {}})]).unwrap();
                    } else {
                        store.label("x", labels(json!({"on_camera": if i % 4 == 1 { "1" } else { "2" }})), "gallery").unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let lines = Store::new(&dir).read(CLIPS_FILE).unwrap();
        assert_eq!(lines.len(), 9, "every added clip is there");
        assert!(lines[0]["labelled_at"].is_string(), "and the label wasn't overwritten by an add");
    }

    #[test]
    fn ranges_are_clamped_to_the_file_and_to_one_piece() {
        assert_eq!(byte_range(None, 100), Some((0, 99)));
        assert_eq!(byte_range(Some("bytes=10-19"), 100), Some((10, 19)));
        assert_eq!(byte_range(Some("bytes=90-"), 100), Some((90, 99)));
        assert_eq!(byte_range(Some("bytes=-10"), 100), Some((90, 99)));
        assert_eq!(byte_range(Some("bytes=95-500"), 100), Some((95, 99)));
        assert_eq!(byte_range(Some("bytes=100-"), 100), None);
        assert_eq!(byte_range(Some("bytes=0-"), 100 << 20), Some((0, VIDEO_CHUNK - 1)), "one piece at a time");
    }

    /// The real server, over TCP: the token and host are required, video comes in ranges, and a
    /// label posted from the page lands in the file.
    #[test]
    fn the_page_needs_its_token_and_saves_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = tmp.path().join("session");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(video::VIDEO_FILE), (0..=255u8).collect::<Vec<_>>()).unwrap();
        let store = Store::new(&tmp.path().join("labels"));
        store
            .add_clips(vec![json!({"id": "s001-a01-c1", "set": "s001", "variant": "a01-c1", "origin": "designed",
                                   "session_dir": session_dir, "start": 5.0, "end": 25.0, "layout": null, "labels": {}})])
            .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = Arc::new(Server::new(store.dir(), "tok", port));
        std::thread::spawn(move || server.run(listener));

        let call = |request: String| -> String {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(request.as_bytes()).unwrap();
            let mut out = Vec::new();
            s.read_to_end(&mut out).unwrap();
            String::from_utf8_lossy(&out).into_owned()
        };
        let host = format!("127.0.0.1:{port}");
        assert!(call(format!("GET /?t=tok HTTP/1.1\r\nHost: {host}\r\n\r\n")).contains("<title>Label video clips</title>"));
        assert!(call(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n")).starts_with("HTTP/1.1 403"), "no token");
        assert!(call("GET /?t=tok HTTP/1.1\r\nHost: evil.example\r\n\r\n".into()).starts_with("HTTP/1.1 403"), "another host");

        let video = call(format!("GET /video/s001?t=tok HTTP/1.1\r\nHost: {host}\r\nRange: bytes=10-13\r\n\r\n"));
        assert!(video.starts_with("HTTP/1.1 206") && video.contains("Content-Range: bytes 10-13/256"), "{video}");
        assert!(call(format!("GET /video/..%2F..?t=tok HTTP/1.1\r\nHost: {host}\r\n\r\n")).starts_with("HTTP/1.1 404"),
                "only an interview's own video");

        let body = r#"{"id": "s001-a01-c1", "layout": "speaker", "labels": {"nodded": false, "nod_count": "none", "on_camera": "1"}}"#;
        let saved = call(format!("POST /api/label HTTP/1.1\r\nHost: {host}\r\nX-Janus-Token: tok\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(saved.starts_with("HTTP/1.1 200"), "{saved}");
        let bad = r#"{"id": "s001-a01-c1", "layout": "speaker", "labels": {"nodded": false, "nod_count": "6+"}}"#;
        let refused = call(format!("POST /api/label HTTP/1.1\r\nHost: {host}\r\nX-Janus-Token: tok\r\nContent-Length: {}\r\n\r\n{bad}", bad.len()));
        assert!(refused.starts_with("HTTP/1.1 400") && refused.contains("change one of them"), "{refused}");
        let state = call(format!("GET /api/state?t=tok HTTP/1.1\r\nHost: {host}\r\n\r\n"));
        assert!(state.contains(r#""labelled":true"#) && !state.contains("session_dir"), "{state}");
        assert_eq!(store.read(CLIPS_FILE).unwrap()[0]["labels"]["on_camera"], "1");
    }
}
