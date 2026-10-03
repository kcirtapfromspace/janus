//! Render a stored analysis for the terminal and as a self-contained HTML page.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use console::style;

use crate::coverage::{self, RecordingNotes};
use crate::db::{AnswerCheck, Outcome, StoredAnalysis, StoredNextSteps};
use crate::metrics::TalkMetrics;
use crate::models::{Direction, Evidence, Priority, Session, Verdict, fmt_ts};

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
    println!("{}", style(format!("━━ {} ━━", header.join(" · "))).bold());
    let notes = notes(session);
    if !notes.is_empty() {
        println!("\n{}", style("Part of this interview wasn't recorded").yellow().bold());
        for note in &notes.for_you {
            println!(" {}", style(note).yellow());
        }
    }

    let actual = match outcome {
        Some(o) => format!("   Actual outcome: {}", style(o.result.label()).bold()),
        None => style(format!("   Record the real result later: ic outcome {} advanced|rejected|offer|…", session.id))
            .dim()
            .to_string(),
    };
    println!("\n{} {}{actual}\n", verdict_badge(a.outlook.verdict), style(format!("({} confidence)", a.outlook.confidence)).dim());
    println!("{}\n", a.outlook.reasoning);
    println!("{}\n{}\n", style("Summary").bold(), a.summary);

    println!("{}", style("Interviewer signals").bold());
    for s in &a.outlook.signals {
        let mark = match s.direction {
            Direction::Positive => style("+").green().bold(),
            Direction::Negative => style("−").red().bold(),
        };
        println!(" {mark} {}\n    {}", s.signal, quote(&s.evidence));
    }

    println!("\n{}", style("By the numbers").bold());
    if notes.incomplete {
        println!(" {}", style(METRICS_LEFT_OUT).dim());
    } else {
        for line in metric_lines(m) {
            println!(" {line}");
        }
    }

    if !stored.answer_checks.is_empty() {
        println!("\n{}", style("Answer by answer").bold());
        println!(" {}", answer_check_tally(&stored.answer_checks));
        if full {
            for (start, question, checks) in answers_with_checks(&stored.answer_checks) {
                let cells: Vec<String> = ANSWER_CHECKS
                    .iter()
                    .filter_map(|(id, label)| checks.get(id).map(|c| format!("{label}: {}", check_cell(c))))
                    .collect();
                println!(" {} {}\n    {}", style(fmt_ts(start)).dim(), question, style(cells.join(" · ")).dim());
            }
        }
    }

    println!("\n{}", style("Rubric").bold());
    for (label, score) in a.rubric.items() {
        println!(" {label:<22} {}  {}", style(format!("{:<5}", dots(score.score))).cyan(), style(&score.rationale).dim());
    }

    println!("\n{}", style("Top things to work on").bold());
    for (i, c) in a.coaching.iter().enumerate() {
        println!(
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

    println!("\n{}", style("Question by question").bold());
    for q in &a.questions {
        println!(" {} {} {} {}", style(&q.timestamp).dim(), style(dots(q.score)).cyan(), q.question,
                 style(format!("({})", q.kind)).dim());
        if full {
            println!("    {} {}\n    {} {}\n    {} {}\n    {} {}\n", style("You:").dim(), q.answer_summary,
                     style("Worked:").green(), q.what_worked, style("Missing:").yellow(), q.what_was_missing,
                     style("Stronger:").cyan(), q.stronger_answer);
        }
    }

    if full {
        println!("\n{}", style("Strengths").bold());
        for h in &a.strengths {
            println!(" {} {}\n    {}", style("✓").green(), h.point, quote(&h.evidence));
        }
        if !a.red_flags.is_empty() {
            println!("\n{}", style("Red flags").bold());
            for h in &a.red_flags {
                println!(" {} {}\n    {}", style("!").red().bold(), h.point, quote(&h.evidence));
            }
        }
    } else {
        println!("\n{}", style(format!("More detail: ic report {0} --full · HTML: ic report {0} --open", session.id)).dim());
    }

    if !stored.unverified_quotes.is_empty() {
        println!("\n{} {} quoted line(s) aren't word-for-word in the transcript and may be paraphrased.",
                 style("Note:").yellow(), stored.unverified_quotes.len());
    }
}

// --- HTML -------------------------------------------------------------------------------------

const CSS: &str = r#"
:root { --bg:#fbfaf8; --fg:#1d1d1f; --muted:#6b6b70; --line:#e6e3de; --card:#fff; --accent:#2f5bd3;
  --good:#1f7a4a; --warn:#a86b00; --bad:#b3261e; --chip:#f1efeb; }
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { --bg:#141416; --fg:#ececef;
  --muted:#9a9aa2; --line:#2b2b30; --card:#1c1c20; --accent:#8aa8ff; --good:#5fcf8f; --warn:#f0b54a;
  --bad:#ff8a80; --chip:#26262b; } }
* { box-sizing:border-box } body { margin:0; background:var(--bg); color:var(--fg);
  font:16px/1.55 -apple-system, BlinkMacSystemFont, "Inter", "Segoe UI", sans-serif; }
main { max-width:860px; margin:0 auto; padding:40px 16px 80px }
h1 { font-size:28px; line-height:1.2; margin:0 0 6px } h2 { font-size:15px; text-transform:uppercase;
  letter-spacing:.06em; color:var(--muted); margin:40px 0 12px }
.meta { color:var(--muted) } .verdict { display:inline-block; padding:6px 14px; border-radius:999px;
  font-weight:650; margin:20px 12px 8px 0 }
.v-strong,.v-leaning_positive { background:color-mix(in srgb,var(--good) 18%,transparent); color:var(--good) }
.v-mixed { background:color-mix(in srgb,var(--warn) 18%,transparent); color:var(--warn) }
.v-leaning_negative,.v-weak { background:color-mix(in srgb,var(--bad) 16%,transparent); color:var(--bad) }
.stats { display:grid; grid-template-columns:repeat(auto-fit,minmax(150px,1fr)); gap:10px }
.stat { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:12px 14px }
.stat b { display:block; font-size:22px } .stat span { color:var(--muted); font-size:13px }
.card { background:var(--card); border:1px solid var(--line); border-radius:14px; padding:18px 20px; margin:12px 0 }
.card h3 { margin:0 0 6px; font-size:18px }
.notice { border:1px solid color-mix(in srgb,var(--warn) 45%,transparent); background:color-mix(in srgb,var(--warn) 10%,transparent);
  border-radius:12px; padding:12px 16px; margin:20px 0 4px } .notice b { color:var(--warn) } .notice p { margin:4px 0 } blockquote { margin:10px 0; padding:8px 12px; border-left:3px solid var(--line);
  color:var(--fg); font-style:italic } .ts { font-style:normal; font-size:12px; color:var(--muted);
  background:var(--chip); border-radius:6px; padding:1px 6px; margin-right:6px; font-variant-numeric:tabular-nums }
.label { font-weight:600 } .rubric { display:grid; grid-template-columns:170px 110px 1fr; gap:6px 14px; align-items:baseline }
.dots { color:var(--accent); letter-spacing:2px } .muted { color:var(--muted) }
details { border-top:1px solid var(--line); padding:10px 0 } summary { cursor:pointer; list-style:none }
summary::-webkit-details-marker { display:none } details p { margin:6px 0 }
table.answers { width:100%; border-collapse:collapse; font-size:14px } table.answers th, table.answers td {
  text-align:left; padding:6px 8px; border-bottom:1px solid var(--line); vertical-align:top }
table.answers th { color:var(--muted); font-weight:600; font-size:12px } .pass { color:var(--good) } .fail { color:var(--bad) }
.unclear { color:var(--muted) }
.sig-positive::before { content:"+ "; color:var(--good); font-weight:700 } .sig-negative::before { content:"− ";
  color:var(--bad); font-weight:700 } ul.plain { list-style:none; padding:0 } ul.plain li { margin:0 0 14px }
@media (max-width:600px) { .rubric { grid-template-columns:1fr 90px } .rubric .muted { grid-column:1/-1; margin-bottom:8px } }
"#;

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

fn q_html(ev: &Evidence) -> String {
    format!("<blockquote><span class='ts'>{}</span>“{}”</blockquote>", esc(&ev.timestamp), esc(&ev.quote))
}

pub fn render_html(session: &Session, stored: &StoredAnalysis, outcome: Option<&Outcome>) -> String {
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

    let mut h = String::new();
    let _ = write!(h, "<h1>{}</h1><div class='meta'>{}</div>", esc(&session.title), meta.join(" · "));
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
            let _ = write!(h, "<tr><td><span class='ts'>{}</span>{}</td>", esc(&fmt_ts(start)), esc(question));
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
    h.push_str("</ul><h2>Rubric</h2><div class='rubric'>");
    for (label, s) in a.rubric.items() {
        let _ = write!(h, "<span class='label'>{label}</span><span class='dots'>{}</span><span class='muted'>{}</span>",
                       dots(s.score), esc(&s.rationale));
    }
    h.push_str("</div><h2>Question by question</h2>");
    for q in &a.questions {
        let _ = write!(h, "<details><summary><span class='ts'>{}</span><span class='dots'>{}</span> {}</summary>\
                           <p><span class='label'>You said:</span> {}</p><p><span class='label'>Worked:</span> {}</p>\
                           <p><span class='label'>Missing:</span> {}</p><p><span class='label'>Stronger answer:</span> {}</p></details>",
                       esc(&q.timestamp), dots(q.score), esc(&q.question), esc(&q.answer_summary),
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
    let _ = write!(h, "<p class='muted'>Analysed {} UTC with {} ({}). Transcript: transcript.md in this folder.</p>",
                   esc(&stored.created_at.chars().take(16).collect::<String>().replace('T', " ")),
                   esc(&stored.model), esc(&stored.prompt_version));

    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>\
         <meta name='viewport' content='width=device-width, initial-scale=1'>\
         <title>{} — Interview report</title><style>{CSS}</style></head><body><main>{h}</main></body></html>",
        esc(&session.title)
    )
}

pub fn write_html(session: &Session, stored: &StoredAnalysis, outcome: Option<&Outcome>) -> std::io::Result<PathBuf> {
    let path = Path::new(&session.dir).join("report.html");
    std::fs::write(&path, render_html(session, stored, outcome))?;
    Ok(path)
}

/// Every analysis run gets its own page, so earlier runs stay viewable next to newer ones.
pub fn analysis_html_path(session: &Session, analysis_id: i64) -> PathBuf {
    Path::new(&session.dir).join("reports").join(format!("{analysis_id}.html"))
}

/// Writes the page only when its content changed, so a page open in the app isn't reloaded for nothing.
pub fn write_analysis_html(session: &Session, stored: &StoredAnalysis, outcome: Option<&Outcome>) -> std::io::Result<PathBuf> {
    let path = analysis_html_path(session, stored.id);
    let html = render_html(session, stored, outcome);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(html.as_str()) {
        std::fs::create_dir_all(path.parent().expect("reports dir"))?;
        std::fs::write(&path, html)?;
    }
    Ok(path)
}

pub fn print_next_steps(session: &Session, next: &StoredNextSteps) {
    let plan = &next.plan;
    println!("{}", style(format!("━━ What to do next · {} ━━", session.title)).bold());
    println!("\n{}\n", style(&plan.headline).bold());
    println!("{}", style("Next-round prep").bold());
    for (i, item) in plan.next_round_prep.iter().enumerate() {
        println!("\n {}\n    {}\n    {}\n    {} {}", style(format!("{}. {}", i + 1, item.topic)).bold(), item.why,
                 quote(&item.evidence), style("Prepare:").green(), item.how_to_prepare);
        for q in &item.likely_questions {
            println!("    {} {q}", style("?").cyan());
        }
    }
    println!("\n{}", style("Practice plan").bold());
    for p in &plan.practice_plan {
        let priority = match p.priority {
            Priority::High => style("high").red(),
            Priority::Medium => style("medium").yellow(),
            Priority::Low => style("low").dim(),
        };
        println!(" {} {} {} {}\n    {}", style("□").dim(), style(&p.skill).bold(),
                 style(format!("({} min · {priority} priority)", p.minutes)).dim(), style(format!("— {}", p.from_coaching)).dim(),
                 p.drill);
    }
    if !next.unverified_quotes.is_empty() {
        println!("\n{} {} quoted line(s) aren't word-for-word in the transcript and may be paraphrased.",
                 style("Note:").yellow(), next.unverified_quotes.len());
    }
    println!("\n{}", style(format!("Generated {} with {}", &next.created_at[..16.min(next.created_at.len())], next.model)).dim());
}
