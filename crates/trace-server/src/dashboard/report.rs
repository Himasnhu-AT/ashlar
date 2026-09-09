//! Release reports: one model, four renderings (page, standalone HTML, Markdown, JSON).
//!
//! The numbers are computed once in [`load_report`] from the rollups, so every format shows the same
//! values. Client-controlled text (versions, issue titles, event names) is escaped for each format.

use maud::{DOCTYPE, Markup, html};
use serde::Serialize;

use super::charts::{self, LineChart, Series};
use super::fmt;
use super::metrics::{self, PerfSummary, SessionSummary};
use super::view::{self, Stat};
use crate::ingest::DAY_MS;
use crate::store::{AppRecord, DayTotals, EventRow, IssueFilter, IssueRow, IssueSort, Store, StoreResult, VersionInfo};

/// Longest span, in days, a report reads from the rollups.
const MAX_SPAN_DAYS: i64 = 365;
const TOP_ISSUES: i64 = 10;
const TOP_FEATURES: usize = 15;

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseStats {
    pub version: String,
    pub first_day: String,
    pub last_day: String,
    /// Distinct installs with a session on this version in the span.
    pub active_installs: i64,
    pub session: SessionSummary,
    pub perf: PerfSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub app: String,
    pub app_name: String,
    pub version: String,
    pub generated_at: String,
    pub release: ReleaseStats,
    pub previous: Option<ReleaseStats>,
    pub top_issues: Vec<IssueRow>,
    pub features: Vec<EventRow>,
    pub daily: Vec<DayTotals>,
    #[serde(skip)]
    first_day: i64,
    #[serde(skip)]
    last_day: i64,
}

fn release_stats(store: &dyn Store, app_id: i64, info: &VersionInfo) -> StoreResult<(ReleaseStats, i64, i64, Vec<DayTotals>)> {
    let to = info.last_day;
    let from = info.first_day.max(to - MAX_SPAN_DAYS + 1);
    let version = Some(info.version.as_str());
    let days = store.day_totals(app_id, from, to, version)?;
    let active_by_day = store.active_installs_by_day(app_id, from, to, version)?;
    let (_, perf) = metrics::summarize_perf(store.perf_rows(app_id, from, to, version)?.iter());
    let stats = ReleaseStats {
        version: info.version.clone(),
        first_day: fmt::date(from),
        last_day: fmt::date(to),
        active_installs: store.active_installs_between(app_id, from, to, version)?,
        session: SessionSummary::from_days(&days, &active_by_day),
        perf,
    };
    Ok((stats, from, to, days))
}

/// Builds the report for `version`, or `None` when the app has no data for it.
pub fn load_report(store: &dyn Store, app: &AppRecord, version: &str, now: i64) -> StoreResult<Option<Report>> {
    let versions = store.versions(app.id)?;
    let Some(position) = versions.iter().position(|v| v.version == version) else { return Ok(None) };
    let (release, from, to, daily) = release_stats(store, app.id, &versions[position])?;
    let previous = match position.checked_sub(1) {
        Some(index) => Some(release_stats(store, app.id, &versions[index])?.0),
        None => None,
    };
    let top_issues = store.list_issues(
        app.id,
        &IssueFilter { version: Some(version), since_ms: from.saturating_mul(DAY_MS), sort: IssueSort::Count, ..IssueFilter::default() },
        TOP_ISSUES,
    )?;
    let mut features = store.events_ranked(app.id, from, to, Some(version), true)?;
    features.truncate(TOP_FEATURES);
    Ok(Some(Report {
        app: app.slug.clone(),
        app_name: app.name.clone(),
        version: version.to_string(),
        generated_at: fmt::datetime(now),
        release,
        previous,
        top_issues,
        features,
        daily,
        first_day: from,
        last_day: to,
    }))
}

// ---------------------------------------------------------------------------------------------------------
// shared rows

fn stat_rows(r: &ReleaseStats) -> Vec<(&'static str, String)> {
    let s = &r.session;
    vec![
        ("Sessions", fmt::int(s.sessions)),
        ("Active installs", fmt::int(r.active_installs)),
        ("Daily opens (total)", fmt::int(s.opens)),
        ("Crash-free sessions", fmt::pct(s.crash_free_sessions)),
        ("Crash-free installs", fmt::pct(s.crash_free_installs)),
        ("Average session", fmt::seconds(s.avg_session_s)),
        ("Frame time p50", fmt::us_as_ms(r.perf.p50_us.map(f64::from))),
        ("Frame time p90", fmt::us_as_ms(r.perf.p90_us.map(f64::from))),
        ("Frame time p99", fmt::us_as_ms(r.perf.p99_us.map(f64::from))),
        ("Slow frames", fmt::pct(r.perf.slow_rate)),
        ("Frozen frames", fmt::pct(r.perf.frozen_rate)),
        ("Startup (mean)", r.perf.startup_ms.map_or_else(|| fmt::dec1(None), |ms| format!("{ms:.0} ms"))),
    ]
}

/// Change in a ratio, in percentage points.
fn points(now: Option<f64>, before: Option<f64>) -> String {
    match (now, before) {
        (Some(a), Some(b)) => format!("{:+.2} pts", (a - b) * 100.0),
        _ => "\u{2013}".to_string(),
    }
}

fn count_delta(now: i64, before: i64) -> String {
    format!("{:+}", now - before)
}

fn ms_delta(now: Option<u32>, before: Option<u32>) -> String {
    match (now, before) {
        (Some(a), Some(b)) => format!("{:+.1} ms", (f64::from(a) - f64::from(b)) / 1000.0),
        _ => "\u{2013}".to_string(),
    }
}

/// `(metric, this release, previous release, change)`.
fn compare_rows(cur: &ReleaseStats, prev: &ReleaseStats) -> Vec<(&'static str, String, String, String)> {
    let (a, b) = (&cur.session, &prev.session);
    let ms = |v: Option<u32>| fmt::us_as_ms(v.map(f64::from));
    vec![
        ("Sessions", fmt::int(a.sessions), fmt::int(b.sessions), count_delta(a.sessions, b.sessions)),
        ("Active installs", fmt::int(cur.active_installs), fmt::int(prev.active_installs), count_delta(cur.active_installs, prev.active_installs)),
        (
            "Crash-free sessions",
            fmt::pct(a.crash_free_sessions),
            fmt::pct(b.crash_free_sessions),
            points(a.crash_free_sessions, b.crash_free_sessions),
        ),
        (
            "Crash-free installs",
            fmt::pct(a.crash_free_installs),
            fmt::pct(b.crash_free_installs),
            points(a.crash_free_installs, b.crash_free_installs),
        ),
        ("Frame time p90", ms(cur.perf.p90_us), ms(prev.perf.p90_us), ms_delta(cur.perf.p90_us, prev.perf.p90_us)),
        ("Frame time p99", ms(cur.perf.p99_us), ms(prev.perf.p99_us), ms_delta(cur.perf.p99_us, prev.perf.p99_us)),
        ("Slow frames", fmt::pct(cur.perf.slow_rate), fmt::pct(prev.perf.slow_rate), points(cur.perf.slow_rate, prev.perf.slow_rate)),
    ]
}

// ---------------------------------------------------------------------------------------------------------
// HTML

/// The report content, for the page and for the standalone file. `link_issues` makes issue titles links to
/// the dashboard, which a downloaded file cannot use.
pub fn body(r: &Report, link_issues: bool) -> Markup {
    let days: Vec<i64> = (r.first_day..=r.last_day).collect();
    let sessions = metrics::fill_days(r.first_day, r.last_day, r.daily.iter().map(|d| (d.day, d.sessions as f64)));
    let chart = LineChart {
        title: format!("Sessions per day on {}", r.version),
        x_label: "Day (UTC)".into(),
        y_label: "Sessions per day".into(),
        x: days.iter().map(|d| fmt::short_date(*d)).collect(),
        series: vec![Series { name: "Sessions".into(), values: sessions }],
        area: true,
        integral: true,
        format: charts::compact,
    };
    let stats: Vec<Stat> = stat_rows(&r.release).into_iter().map(|(label, value)| view::stat(label, value, None)).collect();
    let perf_row = [(r.version.clone(), r.release.perf.clone())];
    html! {
        h1 { "Release report: " (r.app_name) " " (r.version) }
        p class="sub" {
            "Generated " (r.generated_at) ". Covers " (r.release.first_day) " to " (r.release.last_day) " (UTC), from daily rollups."
        }
        h2 { "Summary" }
        (view::stats(&stats))
        (charts::line_chart(&chart))

        h2 { "Stability" }
        @if r.top_issues.is_empty() {
            p { "No crash issues were recorded for this release." }
        } @else {
            p class="note" { "Top issues by occurrence count, across all releases that share the fingerprint." }
            (view::issue_table(&r.app, &r.top_issues, link_issues))
        }

        h2 { "Performance" }
        @if r.release.perf.frames == 0 {
            p { "No frame-time data was recorded for this release." }
        } @else {
            (view::perf_table("Release", &perf_row))
        }

        h2 { "Feature use" }
        @if r.features.is_empty() {
            p { "No usage events were recorded for this release." }
        } @else {
            div class="scroll" {
                table class="data" {
                    thead { tr {
                        th scope="col" { "Event" } th scope="col" { "Category" }
                        th scope="col" class="num" { "Count" } th scope="col" class="num" { "Install-days" }
                    } }
                    tbody {
                        @for e in &r.features {
                            tr { th scope="row" { (e.name) } td { (e.category) } td class="num" { (fmt::int(e.count)) } td class="num" { (fmt::int(e.installs)) } }
                        }
                    }
                }
            }
        }

        @if let Some(prev) = &r.previous {
            h2 { "Compared with " (prev.version) }
            div class="scroll" {
                table class="data" {
                    thead { tr {
                        th scope="col" { "Metric" } th scope="col" class="num" { (r.version) }
                        th scope="col" class="num" { (prev.version) } th scope="col" class="num" { "Change" }
                    } }
                    tbody {
                        @for (metric, now, before, change) in compare_rows(&r.release, prev) {
                            tr { th scope="row" { (metric) } td class="num" { (now) } td class="num" { (before) } td class="num" { (change) } }
                        }
                    }
                }
            }
        } @else {
            h2 { "Comparison" }
            p { "There is no earlier release to compare with." }
        }
    }
}

/// A complete HTML file with the stylesheet inlined, readable on its own.
pub fn standalone_html(r: &Report) -> String {
    let page = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                title { "Release report " (r.app_name) " " (r.version) }
                style { (maud::PreEscaped(view::STYLESHEET)) }
            }
            body { main { (body(r, false)) } }
        }
    };
    page.into_string()
}

// ---------------------------------------------------------------------------------------------------------
// Markdown

/// Escapes text for a Markdown table cell or heading: backslash-escapes Markdown punctuation, turns
/// HTML-significant characters into entities and flattens line breaks.
pub fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() => {}
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '!' | '|' | '~' => {
                out.push('\\');
                out.push(ch);
            }
            c => out.push(c),
        }
    }
    neutralise_links(&out)
}

/// Client text must not turn into live links or notifications when the Markdown is pasted somewhere that
/// renders it. A zero-width space breaks the pattern that autolinking and `@mention` detection look for
/// (`scheme://`, `www.`, `@name`) without changing how the text reads.
fn neutralise_links(text: &str) -> String {
    const ZWSP: &str = "\u{200B}";
    let mut out = text.replace("://", &format!(":{ZWSP}//")).replace('@', &format!("@{ZWSP}"));
    for www in ["www.", "WWW.", "Www."] {
        out = out.replace(www, &format!("{}{ZWSP}.", &www[..3]));
    }
    out
}

pub fn markdown(r: &Report) -> String {
    let mut m = String::new();
    let e = md_escape;
    m.push_str(&format!("# Release report: {} {}\n\n", e(&r.app_name), e(&r.version)));
    m.push_str(&format!("Generated {}. Covers {} to {} (UTC), from daily rollups.\n\n", r.generated_at, r.release.first_day, r.release.last_day));
    m.push_str("## Summary\n\n| Metric | Value |\n|---|---:|\n");
    for (label, value) in stat_rows(&r.release) {
        m.push_str(&format!("| {} | {} |\n", label, value));
    }
    m.push_str("\n## Stability\n\n");
    if r.top_issues.is_empty() {
        m.push_str("No crash issues were recorded for this release.\n");
    } else {
        m.push_str("| Issue | Kind | Count | Installs | Last seen | Status |\n|---|---|---:|---:|---|---|\n");
        for i in &r.top_issues {
            m.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                e(&fmt::head(&i.title, 140)),
                e(&view::kind_label(&i.kind)),
                i.count,
                i.installs,
                fmt::datetime(i.last_seen),
                e(&i.status)
            ));
        }
    }
    m.push_str("\n## Performance\n\n");
    if r.release.perf.frames == 0 {
        m.push_str("No frame-time data was recorded for this release.\n");
    } else {
        let p = &r.release.perf;
        let ms = |v: Option<u32>| fmt::us_as_ms(v.map(f64::from));
        m.push_str("| Frames | p50 | p90 | p99 | Slow | Frozen |\n|---:|---:|---:|---:|---:|---:|\n");
        m.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            p.frames,
            ms(p.p50_us),
            ms(p.p90_us),
            ms(p.p99_us),
            e(&fmt::pct(p.slow_rate)),
            e(&fmt::pct(p.frozen_rate))
        ));
    }
    m.push_str("\n## Feature use\n\n");
    if r.features.is_empty() {
        m.push_str("No usage events were recorded for this release.\n");
    } else {
        m.push_str("| Event | Category | Count | Install-days |\n|---|---|---:|---:|\n");
        for f in &r.features {
            m.push_str(&format!("| {} | {} | {} | {} |\n", e(&f.name), e(&f.category), f.count, f.installs));
        }
    }
    match &r.previous {
        Some(prev) => {
            m.push_str(&format!(
                "\n## Compared with {}\n\n| Metric | {} | {} | Change |\n|---|---:|---:|---:|\n",
                e(&prev.version),
                e(&r.version),
                e(&prev.version)
            ));
            for (metric, now, before, change) in compare_rows(&r.release, prev) {
                m.push_str(&format!("| {} | {} | {} | {} |\n", metric, now, before, change));
            }
        }
        None => m.push_str("\n## Comparison\n\nThere is no earlier release to compare with.\n"),
    }
    m
}

// ---------------------------------------------------------------------------------------------------------
// JSON

/// Pretty JSON. `<`, `>` and `&` are written as `\u` escapes so the text is inert if a browser sniffs it
/// as HTML; the result is still the same JSON value.
pub fn json(r: &Report) -> String {
    let text = serde_json::to_string_pretty(r).unwrap_or_else(|_| "{}".to_string());
    text.replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(version: &str) -> ReleaseStats {
        ReleaseStats {
            version: version.into(),
            first_day: "2026-01-01".into(),
            last_day: "2026-01-02".into(),
            active_installs: 3,
            session: SessionSummary::default(),
            perf: PerfSummary::default(),
        }
    }

    fn report(title: &str) -> Report {
        Report {
            app: "demo".into(),
            app_name: "Demo".into(),
            version: "1.0.0".into(),
            generated_at: "now".into(),
            release: stats("1.0.0"),
            previous: Some(stats("0.9.0")),
            top_issues: vec![IssueRow {
                id: 1,
                kind: "panic".into(),
                title: title.into(),
                first_seen: 0,
                last_seen: 0,
                first_version: "1.0.0".into(),
                last_version: "1.0.0".into(),
                count: 2,
                status: "open".into(),
                installs: 1,
                versions: vec!["1.0.0".into()],
            }],
            features: vec![],
            daily: vec![],
            first_day: 20_000,
            last_day: 20_001,
        }
    }

    #[test]
    fn markdown_escapes_markup_and_table_breakers() {
        assert_eq!(md_escape("<script>alert(1)</script>"), "&lt;script&gt;alert\\(1\\)&lt;/script&gt;");
        assert_eq!(md_escape("a|b\nc"), "a\\|b c");
        assert_eq!(md_escape("[x](http://e)"), "\\[x\\]\\(http:\u{200B}//e\\)");
    }

    #[test]
    fn markdown_does_not_leave_bare_urls_or_mentions_in_client_text() {
        let escaped = md_escape("see https://evil.example/x, www.evil.example and @octocat or me@evil.example");
        assert!(!escaped.contains("://") && !escaped.contains("www.") && !escaped.contains("@o") && !escaped.contains("@e"), "{escaped}");
        // It still reads the same once the invisible characters are ignored.
        assert_eq!(escaped.replace('\u{200B}', ""), "see https://evil.example/x, www.evil.example and @octocat or me@evil.example");
        let r = report("click https://evil.example @everyone");
        let md = markdown(&r);
        assert!(!md.contains("https://evil") && !md.contains("@everyone"), "{md}");
    }

    #[test]
    fn every_format_neutralises_script_tags() {
        let r = report("<script>alert(1)</script>");
        assert!(!standalone_html(&r).contains("<script>"));
        assert!(!markdown(&r).contains("<script>"));
        let json_text = json(&r);
        assert!(!json_text.contains("<script>"));
        let parsed: serde_json::Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(parsed["top_issues"][0]["title"], "<script>alert(1)</script>");
    }

    #[test]
    fn deltas_are_signed() {
        assert_eq!(points(Some(0.99), Some(0.98)), "+1.00 pts");
        assert_eq!(points(None, Some(0.9)), "\u{2013}");
        assert_eq!(count_delta(3, 5), "-2");
        assert_eq!(ms_delta(Some(20_000), Some(10_000)), "+10.0 ms");
    }

    #[test]
    fn a_report_without_a_previous_release_says_so() {
        let mut r = report("x");
        r.previous = None;
        assert!(markdown(&r).contains("no earlier release"));
        assert!(standalone_html(&r).contains("no earlier release"));
    }
}
