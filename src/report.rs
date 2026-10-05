//! Render a stored analysis for the terminal and as a self-contained HTML page.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use console::style;

use crate::coverage::{self, RecordingNotes};
use crate::db::{AnswerCheck, Db, Outcome, StoredAnalysis, StoredNextSteps};
use crate::history::History;
use crate::metrics::TalkMetrics;
use crate::models::{Direction, Evidence, Mode, Priority, Session, Verdict, fmt_ts, parse_ts};
use crate::temperature::{self, Kind, Signal};
use crate::video;

fn dots(score: Option<u8>) -> String {
    match score {
        None => "n/a".into(),
        Some(s) => format!("{}{}", "●".repeat(s as usize), "○".repeat(5usize.saturating_sub(s as usize))),
    }
}

fn verdict_badge(v: Verdict) -> String {
    let text = format!(" {} ", v.label().to_uppercase());
    let s = style(text).bold();
    match v {
        Verdict::Strong | Verdict::LeaningPositive => s.black().on_green(),
        Verdict::Mixed => s.black().on_yellow(),
        Verdict::LeaningNegative | Verdict::Weak => s.white().on_red(),
    }
    .to_string()
}

pub fn metric_lines(m: &TalkMetrics) -> Vec<String> {
    let mut fillers: Vec<_> = m.fillers.iter().filter(|(_, n)| **n > 0).collect();
    fillers.sort_by(|a, b| b.1.cmp(a.1));
    let fillers = fillers.iter().map(|(k, n)| format!("{k} ×{n}")).collect::<Vec<_>>().join(", ");

    let mut delivery = vec![];
    if let Some(wpm) = m.words_per_minute {
        delivery.push(format!("Pace {wpm:.0} words/min"));
    }
    delivery.push(format!(
        "Fillers {} per 100 words{}",
        m.fillers_per_100_words,
        if fillers.is_empty() { String::new() } else { format!(" ({fillers})") }
    ));
    let q = m.questions_you_asked;
    delivery.push(format!("You asked {q} question{}", if q == 1 { "" } else { "s" }));

    let mut lines = vec![
        format!(
            "You talked {:.0}% of the time · {} answers averaging {:.0}s (longest {:.0}s at {})",
            m.your_share * 100.0, m.answers, m.avg_answer_s, m.longest_answer_s, fmt_ts(m.longest_answer_at)
        ),
        delivery.join(" · "),
    ];
    if let (Some(you), Some(them)) = (m.you_interrupted, m.they_interrupted) {
        let mut timing = format!("Interruptions: you {you}, them {them}");
        if let Some(gap) = m.median_response_gap_s {
            let _ = write!(timing, " · median pause before answering {gap:.1}s");
        }
        lines.push(timing);
    }
    lines
}

/// What's missing from this session's recording; derived from its tracks, so pages for older
/// reports get it too.
fn notes(session: &Session) -> RecordingNotes {
    coverage::recording_notes(Path::new(&session.dir), session.mode)
}

/// The answer-by-answer checks, in report order, with how each reads in a table.
const ANSWER_CHECKS: [(&str, &str); 6] = [
    ("leads_with_point", "Led with the point"),
    ("has_quantified_result", "Gave a number"),
    ("star_missing", "Complete story"),
    ("ownership", "Your own part clear"),
    ("specificity", "Specific"),
    ("structure", "Structured"),
];

/// What a check found, in a few words ("✓", "no result", "we", "3/5").
fn check_cell(c: &AnswerCheck) -> String {
    let mark = match c.verdict.as_str() {
        "pass" => "✓",
        "fail" => "✗",
        _ => "?",
    };
    match c.check_id.as_str() {
        "star_missing" if c.verdict == "fail" => match c.pick.as_str() {
            "situation" => "✗ no context".into(),
            "action" => "✗ no actions".into(),
            "result" => "✗ no result".into(),
            other => format!("✗ {other}"),
        },
        "ownership" => format!("{mark} {}", match c.pick.as_str() { "i" => "I", "we" => "we", _ => "team + me" }),
        "specificity" | "structure" => format!("{mark} {}/5", c.pick),
        _ => mark.into(),
    }
}

/// Answers in order, each with its checks by id.
fn answers_with_checks(checks: &[AnswerCheck]) -> Vec<(f64, &str, std::collections::HashMap<&str, &AnswerCheck>)> {
    let mut out: Vec<(f64, &str, std::collections::HashMap<&str, &AnswerCheck>)> = vec![];
    for c in checks {
        match out.last_mut() {
            Some((start, _, map)) if *start == c.answer_start => {
                map.insert(c.check_id.as_str(), c);
            }
            _ => out.push((c.answer_start, c.question.as_str(), std::collections::HashMap::from([(c.check_id.as_str(), c)]))),
        }
    }
    out
}

/// "Led with the point 3 of 7 · Gave a number 5 of 7 · …"
pub fn answer_check_tally(checks: &[AnswerCheck]) -> String {
    let answers = answers_with_checks(checks);
    ANSWER_CHECKS
        .iter()
        .filter_map(|(id, label)| {
            let judged: Vec<_> = answers.iter().filter_map(|(_, _, m)| m.get(id)).collect();
            (!judged.is_empty()).then(|| {
                format!("{label} {} of {}", judged.iter().filter(|c| c.verdict == "pass").count(), judged.len())
            })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

const METRICS_LEFT_OUT: &str = "Left out: part of the conversation wasn't recorded, so talk-time numbers would be wrong.";

fn quote(ev: &Evidence) -> String {
    format!("{} {}", style(&ev.timestamp).dim(), style(format!("\"{}\"", ev.quote)).italic())
}

pub fn print_report(session: &Session, stored: &StoredAnalysis, outcome: Option<&Outcome>, full: bool) {
    let (a, m) = (&stored.analysis, &stored.metrics);
    let ctx = &a.context;
    let header: Vec<String> = [
        Some(session.title.clone()),
        session.company.clone().or(ctx.company.clone()),
        Some(ctx.stage.label().to_string()),
        Some(fmt_ts(session.duration_s.unwrap_or(0.0))),
    ]
    .into_iter()
    .flatten()
    .collect();
    outln!("{}", style(format!("━━ {} ━━", header.join(" · "))).bold());
    let notes = notes(session);
    if !notes.is_empty() {
        outln!("\n{}", style("Part of this interview wasn't recorded").yellow().bold());
        for note in &notes.for_you {
            outln!(" {}", style(note).yellow());
        }
    }

    let actual = match outcome {
        Some(o) => format!("   Actual outcome: {}", style(o.result.label()).bold()),
        None => style(format!("   Record the real result later: ic outcome {} advanced|rejected|offer|…", session.id))
            .dim()
            .to_string(),
    };
    outln!("\n{} {}{actual}\n", verdict_badge(a.outlook.verdict), style(format!("({} confidence)", a.outlook.confidence)).dim());
    outln!("{}\n", a.outlook.reasoning);
    outln!("{}\n{}\n", style("Summary").bold(), a.summary);

    outln!("{}", style("Interviewer signals").bold());
    for s in &a.outlook.signals {
        let mark = match s.direction {
            Direction::Positive => style("+").green().bold(),
            Direction::Negative => style("−").red().bold(),
        };
        outln!(" {mark} {}\n    {}", s.signal, quote(&s.evidence));
    }

    print_room(stored, full);

    outln!("\n{}", style("By the numbers").bold());
    if notes.incomplete {
        outln!(" {}", style(METRICS_LEFT_OUT).dim());
    } else {
        for line in metric_lines(m) {
            outln!(" {line}");
        }
    }

    if !stored.answer_checks.is_empty() {
        outln!("\n{}", style("Answer by answer").bold());
        outln!(" {}", answer_check_tally(&stored.answer_checks));
        if full {
            for (start, question, checks) in answers_with_checks(&stored.answer_checks) {
                let cells: Vec<String> = ANSWER_CHECKS
                    .iter()
                    .filter_map(|(id, label)| checks.get(id).map(|c| format!("{label}: {}", check_cell(c))))
                    .collect();
                outln!(" {} {}\n    {}", style(fmt_ts(start)).dim(), question, style(cells.join(" · ")).dim());
            }
        }
    }

    outln!("\n{}", style("Rubric").bold());
    for (label, score) in a.rubric.items() {
        outln!(" {label:<22} {}  {}", style(format!("{:<5}", dots(score.score))).cyan(), style(&score.rationale).dim());
    }

    outln!("\n{}", style("Top things to work on").bold());
    for (i, c) in a.coaching.iter().enumerate() {
        outln!(
            "\n {}\n    {}\n    {}\n    {} {}\n    {} {}",
            style(format!("{}. {}", i + 1, c.title)).bold(),
            c.why_it_matters,
            quote(&c.evidence),
            style("Try:").green(),
            c.fix,
            style("Drill:").cyan(),
            c.drill
        );
    }

    outln!("\n{}", style("Question by question").bold());
    for q in &a.questions {
        outln!(" {} {} {} {}", style(&q.timestamp).dim(), style(dots(q.score)).cyan(), q.question,
                 style(format!("({})", q.kind)).dim());
        if full {
            outln!("    {} {}\n    {} {}\n    {} {}\n    {} {}\n", style("You:").dim(), q.answer_summary,
                     style("Worked:").green(), q.what_worked, style("Missing:").yellow(), q.what_was_missing,
                     style("Stronger:").cyan(), q.stronger_answer);
        }
    }

    if full {
        outln!("\n{}", style("Strengths").bold());
        for h in &a.strengths {
            outln!(" {} {}\n    {}", style("✓").green(), h.point, quote(&h.evidence));
        }
        if !a.red_flags.is_empty() {
            outln!("\n{}", style("Red flags").bold());
            for h in &a.red_flags {
                outln!(" {} {}\n    {}", style("!").red().bold(), h.point, quote(&h.evidence));
            }
        }
    } else {
        outln!("\n{}", style(format!("More detail: ic report {0} --full · HTML: ic report {0} --open", session.id)).dim());
    }

    if !stored.unverified_quotes.is_empty() {
        outln!("\n{} {} quoted line(s) aren't word-for-word in the transcript and may be paraphrased.",
                 style("Note:").yellow(), stored.unverified_quotes.len());
    }
}

/// A turn's words, cut to about `max` words.
fn excerpt(text: &str, max: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max { words.join(" ") } else { format!("{}…", words[..max].join(" ")) }
}

/// The question each of your answers follows (the last substantive interviewer turn before it).
fn answers_with_questions(signals: &[Signal]) -> Vec<(&Signal, Option<&Signal>)> {
    let mut question = None;
    let mut out = vec![];
    for s in signals {
        match s.kind {
            Kind::Substantive => question = Some(s),
            Kind::Answer => out.push((s, question)),
            Kind::Backchannel => {}
        }
    }
    out
}

fn shift_sentence(shift: &temperature::Shift, signals: &[Signal]) -> String {
    let at = signals.iter().find(|s| s.turn_idx == shift.after_answer).map_or(0.0, |s| s.start);
    format!("The room {} most after your answer at {}.", if shift.delta > 0.0 { "warmed" } else { "cooled" }, fmt_ts(at))
}

pub fn print_room(stored: &StoredAnalysis, full: bool) {
    let signals = &stored.turn_signals;
    if !signals.iter().any(|s| s.temperature.is_some()) {
        return;
    }
    let m = temperature::moments(signals);
    outln!("\n{}", style("How the room felt").bold());
    let line = |label: &str, s: &Signal| {
        outln!(" {label} {} {}", style(fmt_ts(s.start)).dim(), style(format!("\"{}\"", excerpt(&s.text, 16))).italic());
    };
    match &m.shift {
        Some(shift) => outln!(" {}", shift_sentence(shift, signals)),
        None => outln!(" {}", style("No clear shift: the room stayed about the same.").dim()),
    }
    for s in m.warmest.iter().take(if full { 3 } else { 1 }) {
        line(&style("warm").green().to_string(), s);
    }
    for s in m.coolest.iter().take(if full { 3 } else { 1 }) {
        line(&style("cool").red().to_string(), s);
    }
    for s in &m.next_steps {
        line(&style("★").yellow().to_string(), s);
    }
    if full {
        for (answer, question) in answers_with_questions(signals) {
            let notes = temperature::voice_notes(answer);
            if !notes.is_empty() {
                let on = question.map_or(String::new(), |q| format!(" on \"{}\"", excerpt(&q.text, 10)));
                outln!(" {} you{on}: {}", style(fmt_ts(answer.start)).dim(), notes.join(", "));
            }
        }
    }
    if let Some(summary) = video::summary(signals) {
        let sentence = video::sentence(&summary, &video::GATE);
        if !sentence.is_empty() {
            outln!(" {} {}", style("video").cyan(), sentence);
        }
        if full {
            for (answer, question) in answers_with_questions(signals) {
                let notes = answer.video.as_ref().map(|c| video::notes(c, &video::GATE)).unwrap_or_default();
                if !notes.is_empty() {
                    let on = question.map_or(String::new(), |q| format!(" on \"{}\"", excerpt(&q.text, 10)));
                    outln!(" {} while you answered{on}: {}", style(fmt_ts(answer.start)).dim(), notes.join(", "));
                }
            }
        }
    }
}

// --- HTML -------------------------------------------------------------------------------------

const CSS: &str = include_str!("../mac/Resources/Brand/report.css");
const BRAND: &str = include_str!("../mac/Resources/Brand/report-brand.html");

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// A timestamp the app turns into "play from here" (a browser just ignores the link).
fn seek_html(seconds: f64, label: &str) -> String {
    format!("<a class='ts' href='#t={seconds:.1}'>{}</a>", esc(label))
}

fn ts_html(ts: &str) -> String {
    match parse_ts(ts) {
        Some(seconds) => seek_html(seconds, ts),
        None => format!("<span class='ts'>{}</span>", esc(ts)),
    }
}

fn q_html(ev: &Evidence) -> String {
    format!("<blockquote>{}“{}”</blockquote>", ts_html(&ev.timestamp), esc(&ev.quote))
}

// --- How the room felt ------------------------------------------------------------------------

const SVG_W: f64 = 800.0;
const SVG_H: f64 = 210.0;
const PLOT_LEFT: f64 = 40.0;
const PLOT_RIGHT: f64 = 790.0;
const PLOT_TOP: f64 = 28.0;
const PLOT_BOTTOM: f64 = 182.0;

fn temp_class(t: f64) -> &'static str {
    if t >= temperature::CLEAR {
        "warm"
    } else if t <= -temperature::CLEAR {
        "cool"
    } else {
        "mid"
    }
}

/// The timeline chart: your answers as shaded bands, each interviewer turn as a dot (warm up, cool
/// down), the smoothed line, and markers above. Every shape links to its moment. No script.
fn room_svg(signals: &[Signal], duration: f64) -> String {
    let duration = duration.max(signals.iter().map(|s| s.end).fold(1.0, f64::max));
    let x = |t: f64| PLOT_LEFT + (t / duration).clamp(0.0, 1.0) * (PLOT_RIGHT - PLOT_LEFT);
    let y = |v: f64| PLOT_TOP + (1.0 - v.clamp(-1.0, 1.0)) / 2.0 * (PLOT_BOTTOM - PLOT_TOP);
    let mut h = format!("<svg class='room' viewBox='0 0 {SVG_W} {SVG_H}' role='img' \
                         aria-label='How warm or cool each interviewer turn was, over the interview'>");
    for a in signals.iter().filter(|s| s.kind == Kind::Answer) {
        let seen = a.video.as_ref().map(|c| video::notes(c, &video::GATE)).filter(|n| !n.is_empty()).map_or(String::new(), |n| format!(" ({})", n.join(", ")));
        let _ = write!(h, "<a href='#t={:.1}'><title>Your answer, {}{}</title><rect class='band' x='{:.1}' y='{PLOT_TOP}' \
                           width='{:.1}' height='{:.1}'/></a>",
                       a.start, fmt_ts(a.start), esc(&seen), x(a.start), (x(a.end) - x(a.start)).max(1.0), PLOT_BOTTOM - PLOT_TOP);
    }
    let _ = write!(h, "<line class='grid' x1='{PLOT_LEFT}' x2='{PLOT_RIGHT}' y1='{PLOT_TOP}' y2='{PLOT_TOP}'/>\
                       <line class='grid' x1='{PLOT_LEFT}' x2='{PLOT_RIGHT}' y1='{PLOT_BOTTOM}' y2='{PLOT_BOTTOM}'/>\
                       <line class='zero' x1='{PLOT_LEFT}' x2='{PLOT_RIGHT}' y1='{0:.1}' y2='{0:.1}'/>\
                       <text x='0' y='{1:.1}'>warm</text><text x='0' y='{0:.1}'>0</text><text x='0' y='{2:.1}'>cool</text>",
                   y(0.0) + 4.0, PLOT_TOP + 4.0, PLOT_BOTTOM);
    let step = [60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0].into_iter().find(|s| duration / s <= 8.0).unwrap_or(3600.0);
    let mut t = 0.0;
    while t <= duration {
        let label = if t == 0.0 { "0".to_string() } else { format!("{} min", (t / 60.0).round()) };
        let _ = write!(h, "<text class='tick' x='{:.1}' y='{:.1}'>{label}</text>", x(t), SVG_H - 6.0);
        t += step;
    }
    let turns: Vec<(&Signal, f64)> = signals.iter().filter_map(|s| Some((s, s.temperature?))).collect();
    let line: Vec<String> = signals
        .iter()
        .filter_map(|s| Some(format!("{:.1},{:.1}", x(s.start), y(s.smoothed?))))
        .collect();
    if line.len() > 1 {
        let _ = write!(h, "<polyline class='line' points='{}'/>", line.join(" "));
    }
    for (s, temp) in &turns {
        let cues = temperature::cues(s);
        let why = if cues.is_empty() { String::new() } else { format!(" ({})", cues.join(", ")) };
        let _ = write!(h, "<a href='#t={:.1}'><title>{} {:+.2}{} — “{}”</title><circle class='{}' cx='{:.1}' cy='{:.1}' r='5'/></a>",
                       s.start, fmt_ts(s.start), temp, esc(&why), esc(&excerpt(&s.text, 24)), temp_class(*temp), x(s.start), y(*temp));
        let marks: Vec<&str> = temperature::MARKERS.iter().filter(|(id, ..)| temperature::flagged(s, id)).map(|(_, m, _)| *m).collect();
        if !marks.is_empty() {
            let _ = write!(h, "<a href='#t={:.1}'><text class='mark' x='{:.1}' y='16'>{}</text></a>", s.start, x(s.start), marks.join(""));
        }
    }
    h.push_str("</svg>");
    h
}

fn moment_html(s: &Signal) -> String {
    let cues = temperature::cues(s);
    let cues = if cues.is_empty() { String::new() } else { format!("<div class='cues'>{}</div>", esc(&cues.join(" · "))) };
    format!("<li>{}“{}”{cues}</li>", seek_html(s.start, &fmt_ts(s.start)), esc(&excerpt(&s.text, 40)))
}

fn room_html(h: &mut String, session: &Session, stored: &StoredAnalysis) {
    let signals = &stored.turn_signals;
    if !signals.iter().any(|s| s.temperature.is_some()) {
        return;
    }
    let m = temperature::moments(signals);
    h.push_str("<h2>How the room felt</h2>");
    match &m.shift {
        Some(shift) => {
            let _ = write!(h, "<p>{}</p>", esc(&shift_sentence(shift, signals)));
        }
        None => h.push_str("<p>No clear shift: the room stayed about the same throughout.</p>"),
    }
    h.push_str(&room_svg(signals, session.duration_s.unwrap_or(0.0)));
    h.push_str("<p class='muted legend'><i class='warm'>●</i> warmer <i class='mid'>●</i> neutral <i class='cool'>●</i> cooler \
                · line: the trend · shaded: your answers");
    for (id, mark, label) in temperature::MARKERS {
        if signals.iter().any(|s| temperature::flagged(s, id)) {
            let _ = write!(h, " · {mark} {label}");
        }
    }
    h.push_str(" · click any point to play it</p>");
    for (title, list) in [("Warmest moments", &m.warmest), ("Coolest moments", &m.coolest), ("Next steps mentioned", &m.next_steps)] {
        if !list.is_empty() {
            let _ = write!(h, "<h3>{title}</h3><ul class='plain moments'>");
            for s in list.iter() {
                h.push_str(&moment_html(s));
            }
            h.push_str("</ul>");
        }
    }
    let voice: Vec<String> = answers_with_questions(signals)
        .into_iter()
        .filter_map(|(answer, question)| {
            let notes = temperature::voice_notes(answer);
            (!notes.is_empty()).then(|| {
                let on = question.map_or(String::new(), |q| format!(" on “{}”", esc(&excerpt(&q.text, 12))));
                format!("<li>{}You{on}: {}</li>", seek_html(answer.start, &fmt_ts(answer.start)), esc(&notes.join(", ")))
            })
        })
        .collect();
    if !voice.is_empty() {
        let _ = write!(h, "<h3>Your voice</h3><ul class='plain moments'>{}</ul>", voice.join(""));
    }
    video_html(h, signals, stored.video_method.as_deref());
    let placed = match &stored.timeline_scorer {
        Some(scorer) => format!("by its words (checked turn by turn by {}) and by how they sounded compared with the rest of \
                                 this call", esc(scorer)),
        None => "only by how they sounded compared with the rest of this call (add a TypeSafe key in Setup to include \
                 their words)".into(),
    };
    let single = if session.mode == Mode::Single {
        " This recording is one mixed track, so who said what comes from speaker detection: where the transcript mixes \
         people up, so does this timeline, and the mm-hmms while you talk can't be counted. Record with the app, or \
         import both tracks, for a reliable read."
    } else {
        ""
    };
    let _ = write!(h, "<p class='muted'>Each dot is one thing the interviewer said, placed {placed}. It's behaviour, not \
                       mind-reading: every point is something you can replay.{single} Method {}.</p>", temperature::METHOD);
}

/// What the call's video showed of the other people while you answered: only the cues the eval
/// hasn't failed (video::GATE), tagged experimental until every one shown has passed.
fn video_html(h: &mut String, signals: &[Signal], method: Option<&str>) {
    let Some(summary) = video::summary(signals) else { return };
    let gate = &video::GATE;
    let sentence = video::sentence(&summary, gate);
    let notable: Vec<String> = answers_with_questions(signals)
        .into_iter()
        .filter_map(|(answer, question)| {
            let notes = video::notes(answer.video.as_ref()?, gate);
            (!notes.is_empty()).then(|| {
                let on = question.map_or(String::new(), |q| format!(" on “{}”", esc(&excerpt(&q.text, 12))));
                format!("<li>{}While you answered{on}: {} <a class='fix muted' href='#fix={:.1}'>Not what you saw?</a></li>",
                        seek_html(answer.start, &fmt_ts(answer.start)), esc(&notes.join(", ")), answer.start)
            })
        })
        .collect();
    if sentence.is_empty() && notable.is_empty() {
        return;
    }
    let tag = if gate.experimental() { " <span class='tag'>experimental</span>" } else { "" };
    let _ = write!(h, "<h3>What the video showed{tag}</h3>");
    if !sentence.is_empty() {
        let _ = write!(h, "<p>{}</p>", esc(&sentence));
    }
    if !notable.is_empty() {
        let _ = write!(h, "<ul class='plain moments'>{}</ul>", notable.join(""));
    }
    let checked = if gate.experimental() {
        "Not yet checked against labelled calls (docs/eval/video-decision.md): small video tiles hide small nods, a layout \
         change (speaker view) can mix people up, and looking down may be note-taking."
    } else {
        "Each cue shown passed a check against labelled calls (docs/eval/video-decision.md)."
    };
    let _ = write!(h, "<p class='muted'>From the call's window, read on this Mac: where faces were and which way they \
                       pointed, never expressions or emotions, and nothing that identifies anyone. Your own face is told \
                       apart by whose speech its mouth moves with. {checked} If the video got an answer wrong or missed \
                       something, correct it in Janus (Correct video cues): each correction helps measure how accurate \
                       these are. Method {}.</p>",
                   esc(method.unwrap_or(video::METHOD)));
}

fn when(created_at: &str) -> String {
    format!("{} UTC", created_at.chars().take(16).collect::<String>().replace('T', " "))
}

/// "Version 2 of 3 · claude-opus-5-5 · made … · current", under the title.
fn version_line(stored: &StoredAnalysis, history: &History) -> String {
    let Some(v) = history.version(stored.id) else { return String::new() };
    let status = match history.current {
        Some(c) if c == stored.id => " · the current version".to_string(),
        Some(c) => history.version(c).map_or(String::new(), |cv| format!(" · not current (<a href='{c}.html'>v{}</a> is)", cv.number)),
        None => String::new(),
    };
    format!("<p class='muted version'>Version {} of {} · {} · made {}{status} · <a href='#history'>all versions</a></p>",
            v.number, history.total_versions, esc(crate::versions::short_model(&v.model)), esc(&when(&v.created_at)))
}

fn history_html(h: &mut String, this: i64, history: &History) {
    h.push_str("<h2 id='history'>Versions</h2><p class='muted'>Re-running with nothing changed shows the same version \
                again, with no new model call, so its verdict, scores and call stats stay exactly as they were. A new \
                version appears when the transcript, the model or the prompt changes.</p><ul class='tree'>");
    for r in &history.revisions {
        let title = if r.number == 0 { "Earlier transcript".to_string() } else { format!("Transcript {}", r.number) };
        let same = r.same_as.map_or(String::new(), |n| format!(" · same lines as Transcript {n}"));
        let at = if r.created_at.is_empty() { String::new() } else { format!(" · {}", esc(&when(&r.created_at))) };
        let _ = write!(h, "<li><span class='rev'>{title}</span> <span class='muted'>· {}{at}{same}</span>", esc(&r.how));
        if r.versions.is_empty() {
            h.push_str("<div class='muted'>No report built on it.</div></li>");
            continue;
        }
        h.push_str("<ul>");
        for v in &r.versions {
            let label = if v.analysis_id == this {
                format!("<b>v{}</b>", v.number)
            } else {
                format!("<a href='{}.html'>v{}</a>", v.analysis_id, v.number)
            };
            let mut tags = String::new();
            if history.current == Some(v.analysis_id) {
                tags.push_str("<span class='tag'>current</span>");
            }
            if v.analysis_id == this {
                tags.push_str("<span class='tag'>this page</span>");
            }
            let mut nums = vec![format!("{} ({} confidence)", v.verdict, v.confidence)];
            if let Some(share) = v.your_share {
                nums.push(format!("you spoke {:.0}%", share * 100.0));
            }
            if let Some(room) = v.room {
                nums.push(format!("room {room:+.2}"));
            }
            nums.push(format!("signals +{} −{}", v.positive_signals, v.negative_signals));
            if let Some(r) = v.rubric_mean {
                nums.push(format!("rubric {r:.1}"));
            }
            let _ = write!(h, "<li{}><div>{label} · {} · {}{tags}</div><div class='nums'>{}</div>",
                           if v.analysis_id == this { " class='this'" } else { "" },
                           esc(crate::versions::short_model(&v.model)), esc(&when(&v.created_at)), esc(&nums.join(" · ")));
            if let Some(p) = v.parent {
                let what = if v.changes.is_empty() { "re-run".to_string() } else { v.changes.join(", ") };
                let _ = write!(h, "<div class='muted'>From v{p}: {}</div>", esc(&what));
            }
            if !v.inputs_recorded {
                h.push_str("<div class='muted'>Made before versions recorded their inputs.</div>");
            }
            if v.reruns_reused > 0 {
                let _ = write!(h, "<div class='muted'>Shown again by {} unchanged re-run{}.</div>", v.reruns_reused,
                               if v.reruns_reused == 1 { "" } else { "s" });
            }
            for (at, model) in &v.next_steps {
                let _ = write!(h, "<div class='muted'>Next steps planned {} with {}.</div>", esc(&when(at)),
                               esc(crate::versions::short_model(model)));
            }
            h.push_str("</li>");
        }
        h.push_str("</ul></li>");
    }
    h.push_str("</ul>");
}

pub fn render_html(session: &Session, stored: &StoredAnalysis, outcome: Option<&Outcome>, history: Option<&History>) -> String {
    let (a, m, ctx) = (&stored.analysis, &stored.metrics, &stored.analysis.context);
    let meta: Vec<String> = [
        session.company.clone().or(ctx.company.clone()),
        ctx.role_title.clone(),
        Some(ctx.stage.label().to_string()),
        Some(session.created_at.chars().take(10).collect()),
        Some(fmt_ts(session.duration_s.unwrap_or(0.0))),
    ]
    .into_iter()
    .flatten()
    .map(|s| esc(&s))
    .collect();
    let stats = [
        (format!("{:.0}%", m.your_share * 100.0), "your share of talk time".to_string()),
        (format!("{:.0}s", m.avg_answer_s), format!("average answer ({} answers)", m.answers)),
        (format!("{:.0}s", m.longest_answer_s), format!("longest answer, at {}", fmt_ts(m.longest_answer_at))),
        (m.words_per_minute.map_or("—".into(), |w| format!("{w:.0}")), "words per minute".into()),
        (m.fillers_per_100_words.to_string(), "filler words per 100".into()),
        (m.questions_you_asked.to_string(), "questions you asked".into()),
    ];

    let notes = notes(session);

    let mut h = String::from(BRAND);
    let _ = write!(h, "<h1>{}</h1><div class='meta'>{}</div>", esc(&session.title), meta.join(" · "));
    if let Some(history) = history {
        h.push_str(&version_line(stored, history));
    }
    if !notes.is_empty() {
        h.push_str("<div class='notice'><b>Part of this interview wasn't recorded</b>");
        for note in &notes.for_you {
            let _ = write!(h, "<p>{}</p>", esc(note));
        }
        h.push_str("</div>");
    }
    let _ = write!(h, "<span class='verdict v-{}'>{}</span><span class='muted'>{} confidence{}</span>",
                   a.outlook.verdict, a.outlook.verdict.label(), a.outlook.confidence,
                   outcome.map_or(String::new(), |o| format!(" · actual outcome: <b>{}</b>", o.result.label())));
    let _ = write!(h, "<p>{}</p><p>{}</p>", esc(&a.outlook.reasoning), esc(&a.summary));

    h.push_str("<h2>Top things to work on</h2>");
    for (i, c) in a.coaching.iter().enumerate() {
        let _ = write!(h, "<div class='card'><h3>{}. {}</h3><p>{}</p>{}<p><span class='label'>Try:</span> {}</p>\
                           <p><span class='label'>Drill:</span> {}</p></div>",
                       i + 1, esc(&c.title), esc(&c.why_it_matters), q_html(&c.evidence), esc(&c.fix), esc(&c.drill));
    }

    h.push_str("<h2>By the numbers</h2>");
    if notes.incomplete {
        let _ = write!(h, "<p class='muted'>{METRICS_LEFT_OUT}</p>");
    } else {
        h.push_str("<div class='stats'>");
        for (value, label) in &stats {
            let _ = write!(h, "<div class='stat'><b>{}</b><span>{}</span></div>", esc(value), esc(label));
        }
        h.push_str("</div>");
    }
    if !stored.answer_checks.is_empty() {
        h.push_str("<h2>Answer by answer</h2>");
        let _ = write!(h, "<p>{}</p><table class='answers'><tr><th>Question</th>", esc(&answer_check_tally(&stored.answer_checks)));
        for (_, label) in ANSWER_CHECKS {
            let _ = write!(h, "<th>{}</th>", esc(label));
        }
        h.push_str("</tr>");
        for (start, question, checks) in answers_with_checks(&stored.answer_checks) {
            let _ = write!(h, "<tr><td>{}{}</td>", seek_html(start, &fmt_ts(start)), esc(question));
            for (id, _) in ANSWER_CHECKS {
                match checks.get(id) {
                    Some(c) => { let _ = write!(h, "<td class='{}'>{}</td>", esc(&c.verdict), esc(&check_cell(c))); }
                    None => h.push_str("<td class='unclear'>—</td>"),
                }
            }
            h.push_str("</tr>");
        }
        let scorer = stored.answer_checks.first().map(|c| c.scorer.as_str()).unwrap_or_default();
        let _ = write!(h, "</table><p class='muted'>Checked answer by answer by {}. ✓ meets the bar, ✗ doesn't, ? too close to call.</p>",
                       esc(scorer));
    }
    h.push_str("<h2>Interviewer signals</h2><ul class='plain'>");
    for s in &a.outlook.signals {
        let _ = write!(h, "<li class='sig-{}'>{}{}</li>", s.direction, esc(&s.signal), q_html(&s.evidence));
    }
    h.push_str("</ul>");
    room_html(&mut h, session, stored);
    h.push_str("<h2>Rubric</h2><div class='rubric'>");
    for (label, s) in a.rubric.items() {
        let _ = write!(h, "<span class='label'>{label}</span><span class='dots'>{}</span><span class='muted'>{}</span>",
                       dots(s.score), esc(&s.rationale));
    }
    h.push_str("</div><h2>Question by question</h2>");
    for q in &a.questions {
        let _ = write!(h, "<details><summary>{}<span class='dots'>{}</span> {}</summary>\
                           <p><span class='label'>You said:</span> {}</p><p><span class='label'>Worked:</span> {}</p>\
                           <p><span class='label'>Missing:</span> {}</p><p><span class='label'>Stronger answer:</span> {}</p></details>",
                       ts_html(&q.timestamp), dots(q.score), esc(&q.question), esc(&q.answer_summary),
                       esc(&q.what_worked), esc(&q.what_was_missing), esc(&q.stronger_answer));
    }
    h.push_str("<h2>Strengths</h2><ul class='plain'>");
    for s in &a.strengths {
        let _ = write!(h, "<li>{}{}</li>", esc(&s.point), q_html(&s.evidence));
    }
    h.push_str("</ul>");
    if !a.red_flags.is_empty() {
        h.push_str("<h2>Red flags</h2><ul class='plain'>");
        for s in &a.red_flags {
            let _ = write!(h, "<li>{}{}</li>", esc(&s.point), q_html(&s.evidence));
        }
        h.push_str("</ul>");
    }
    if !stored.unverified_quotes.is_empty() {
        let _ = write!(h, "<p class='muted'>{} quoted line(s) aren't word-for-word in the transcript and may be paraphrased.</p>",
                       stored.unverified_quotes.len());
    }
    if let Some(history) = history {
        history_html(&mut h, stored.id, history);
    }
    let _ = write!(h, "<p class='muted'>Analysed {} UTC with {} ({}). Transcript: transcript.md in this folder.</p>",
                   esc(&stored.created_at.chars().take(16).collect::<String>().replace('T', " ")),
                   esc(&stored.model), esc(&stored.prompt_version));

    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>\
         <meta name='viewport' content='width=device-width, initial-scale=1'>\
         <title>{} — Janus interview review</title><style>{CSS}</style></head><body><main>{h}</main></body></html>",
        esc(&session.title)
    )
}

/// Writes a page only when its content changed, so a page open in the app isn't reloaded for nothing.
fn write_if_changed(path: &Path, html: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(html) {
        std::fs::create_dir_all(path.parent().expect("a folder"))?;
        std::fs::write(path, html)?;
    }
    Ok(())
}

/// Every report version gets its own page in `reports/`, so earlier versions stay viewable.
pub fn analysis_html_path(session: &Session, analysis_id: i64) -> PathBuf {
    Path::new(&session.dir).join("reports").join(format!("{analysis_id}.html"))
}

pub fn write_analysis_html(session: &Session, stored: &StoredAnalysis, outcome: Option<&Outcome>, history: Option<&History>)
    -> std::io::Result<PathBuf> {
    let path = analysis_html_path(session, stored.id);
    write_if_changed(&path, &render_html(session, stored, outcome, history))?;
    Ok(path)
}

/// Every version's page (each shows the whole history), and `report.html`, which opens the current
/// version. Returns `report.html`'s path, or None before the first report.
pub fn write_pages(db: &Db, session: &Session, outcome: Option<&Outcome>) -> anyhow::Result<Option<PathBuf>> {
    let history = crate::history::build(db, session.id)?;
    let analyses = db.analyses(session.id)?;
    for a in &analyses {
        write_analysis_html(session, a, outcome, Some(&history))?;
    }
    let Some(shown) = history.current.or(analyses.first().map(|a| a.id)) else { return Ok(None) };
    let path = Path::new(&session.dir).join("report.html");
    write_if_changed(&path, &format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='0; url=reports/{shown}.html'>\
         <title>{} — Janus interview review</title></head><body><a href='reports/{shown}.html'>Open the report</a></body></html>",
        esc(&session.title)))?;
    Ok(Some(path))
}

pub fn print_next_steps(session: &Session, next: &StoredNextSteps) {
    let plan = &next.plan;
    outln!("{}", style(format!("━━ What to do next · {} ━━", session.title)).bold());
    outln!("\n{}\n", style(&plan.headline).bold());
    outln!("{}", style("Next-round prep").bold());
    for (i, item) in plan.next_round_prep.iter().enumerate() {
        outln!("\n {}\n    {}\n    {}\n    {} {}", style(format!("{}. {}", i + 1, item.topic)).bold(), item.why,
                 quote(&item.evidence), style("Prepare:").green(), item.how_to_prepare);
        for q in &item.likely_questions {
            outln!("    {} {q}", style("?").cyan());
        }
    }
    outln!("\n{}", style("Practice plan").bold());
    for p in &plan.practice_plan {
        let priority = match p.priority {
            Priority::High => style("high").red(),
            Priority::Medium => style("medium").yellow(),
            Priority::Low => style("low").dim(),
        };
        outln!(" {} {} {} {}\n    {}", style("□").dim(), style(&p.skill).bold(),
                 style(format!("({} min · {priority} priority)", p.minutes)).dim(), style(format!("— {}", p.from_coaching)).dim(),
                 p.drill);
    }
    if !next.unverified_quotes.is_empty() {
        outln!("\n{} {} quoted line(s) aren't word-for-word in the transcript and may be paraphrased.",
                 style("Note:").yellow(), next.unverified_quotes.len());
    }
    outln!("\n{}", style(format!("Generated {} with {}", &next.created_at[..16.min(next.created_at.len())], next.model)).dim());
}
