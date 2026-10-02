//! Render a stored analysis for the terminal and as a self-contained HTML page.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use console::style;

use crate::db::{Outcome, StoredAnalysis};
use crate::metrics::TalkMetrics;
use crate::models::{Direction, Evidence, Session, Verdict, fmt_ts};

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
    for line in metric_lines(m) {
        println!(" {line}");
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
        println!(" {} {} {} {}", style(&q.timestamp).dim(), style(dots(Some(q.score))).cyan(), q.question,
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
.card h3 { margin:0 0 6px; font-size:18px } blockquote { margin:10px 0; padding:8px 12px; border-left:3px solid var(--line);
  color:var(--fg); font-style:italic } .ts { font-style:normal; font-size:12px; color:var(--muted);
  background:var(--chip); border-radius:6px; padding:1px 6px; margin-right:6px; font-variant-numeric:tabular-nums }
.label { font-weight:600 } .rubric { display:grid; grid-template-columns:170px 110px 1fr; gap:6px 14px; align-items:baseline }
.dots { color:var(--accent); letter-spacing:2px } .muted { color:var(--muted) }
details { border-top:1px solid var(--line); padding:10px 0 } summary { cursor:pointer; list-style:none }
summary::-webkit-details-marker { display:none } details p { margin:6px 0 }
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

    let mut h = String::new();
    let _ = write!(h, "<h1>{}</h1><div class='meta'>{}</div>", esc(&session.title), meta.join(" · "));
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

    h.push_str("<h2>By the numbers</h2><div class='stats'>");
    for (value, label) in &stats {
        let _ = write!(h, "<div class='stat'><b>{}</b><span>{}</span></div>", esc(value), esc(label));
    }
    h.push_str("</div><h2>Interviewer signals</h2><ul class='plain'>");
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
                       esc(&q.timestamp), dots(Some(q.score)), esc(&q.question), esc(&q.answer_summary),
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
