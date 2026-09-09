//! Page shell and the small components shared by the pages and the report.
//!
//! Every string that came from a client goes through maud's escaping. Nothing in the dashboard uses
//! `PreEscaped` on such data.

use maud::{DOCTYPE, Markup, html};

use super::data::{Filters, RANGES};
use super::fmt;
use super::metrics::PerfSummary;
use crate::store::{IssueRow, VersionInfo};

pub const STYLESHEET: &str = include_str!("dashboard.css");
pub const STYLESHEET_PATH: &str = "/static/dashboard.css";

/// Percent-encodes everything except unreserved characters, for query values.
pub fn enc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A query string for the current filters (without a leading `?`), optionally leaving out the version.
pub fn query(f: &Filters, with_version: bool) -> String {
    let mut q = format!("days={}", f.days);
    if let (true, Some(version)) = (with_version, f.version()) {
        q.push_str("&version=");
        q.push_str(&enc(version));
    }
    q
}

pub struct Shell<'a> {
    pub title: &'a str,
    pub user: &'a str,
    pub csrf: &'a str,
    /// `(slug, name)` of the app being viewed.
    pub app: Option<(&'a str, &'a str)>,
    /// Which tab is current: `overview`, `crashes`, `performance`, `usage`, `reports` or `settings`.
    pub active: &'a str,
}

const TABS: [(&str, &str, &str); 6] = [
    ("overview", "Overview", ""),
    ("crashes", "Crashes", "/crashes"),
    ("performance", "Performance", "/performance"),
    ("usage", "Usage", "/usage"),
    ("reports", "Reports", "/reports"),
    ("settings", "Settings", "/settings"),
];

fn head_tags(title: &str) -> Markup {
    html! {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1";
        meta name="color-scheme" content="light dark";
        meta name="referrer" content="no-referrer";
        title { (title) " \u{2013} Tracer" }
        link rel="stylesheet" href=(STYLESHEET_PATH);
    }
}

pub fn shell(s: &Shell<'_>, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head_tags(s.title)) }
            body {
                a class="skip" href="#main" { "Skip to content" }
                header class="top" {
                    div class="top-row" {
                        a class="brand" href="/" { "Tracer" }
                        @if let Some((slug, name)) = s.app {
                            span class="crumb" { a href="/" { "Apps" } " / " a href=(format!("/apps/{slug}")) { (name) } }
                        }
                        form class="logout" method="post" action="/logout" {
                            input type="hidden" name="csrf" value=(s.csrf);
                            span class="who" { (s.user) }
                            button type="submit" class="link" { "Log out" }
                        }
                    }
                    @if let Some((slug, _)) = s.app {
                        nav aria-label="App sections" {
                            ul class="tabs" {
                                @for (key, label, suffix) in TABS {
                                    li { a href=(format!("/apps/{slug}{suffix}")) aria-current=[(key == s.active).then_some("page")] { (label) } }
                                }
                            }
                        }
                    }
                }
                main id="main" { (body) }
                footer { "Tracer " (env!("CARGO_PKG_VERSION")) ". Times are UTC." }
            }
        }
    }
}

/// A bare page for the login screen.
pub fn plain(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head_tags(title)) }
            body class="plain" { main id="main" { (body) } }
        }
    }
}

/// Page heading with an optional line of context.
pub fn heading(title: &str, sub: Option<&str>) -> Markup {
    html! {
        div class="page-head" {
            h1 { (title) }
            @if let Some(sub) = sub { p class="sub" { (sub) } }
        }
    }
}

/// Label, value and an optional note for one headline number.
pub struct Stat {
    pub label: String,
    pub value: String,
    pub note: Option<String>,
}

pub fn stat(label: &str, value: impl Into<String>, note: Option<&str>) -> Stat {
    Stat { label: label.to_string(), value: value.into(), note: note.map(str::to_string) }
}

pub fn stats(items: &[Stat]) -> Markup {
    html! {
        dl class="stats" {
            @for item in items {
                div class="stat" {
                    dt { (item.label) }
                    dd class="num" { (item.value) }
                    @if let Some(note) = &item.note { dd class="note" { (note) } }
                }
            }
        }
    }
}

/// The date range and version selectors, as a plain GET form.
pub fn filter_bar(action: &str, f: &Filters, versions: &[VersionInfo], extra: Markup) -> Markup {
    html! {
        form class="filters" method="get" action=(action) {
            label { span { "Range" }
                select name="days" {
                    @for days in RANGES { option value=(days) selected[days == f.days] { "Last " (days) " days" } }
                }
            }
            label { span { "Version" }
                select name="version" {
                    option value="" selected[f.version.is_none()] { "All versions" }
                    @for v in versions.iter().rev() {
                        option value=(v.version) selected[f.version() == Some(v.version.as_str())] { (v.version) }
                    }
                }
            }
            (extra)
            button type="submit" { "Apply" }
        }
    }
}

/// Shown when an app has no rolled-up data yet.
pub fn empty_state(app_name: &str, slug: &str, ingest_url: &str) -> Markup {
    html! {
        section class="empty" aria-labelledby="empty-title" {
            h2 id="empty-title" { "No data yet for " (app_name) }
            p { "Nothing has been received and rolled up for this app. Point the SDK at this server and run the host app:" }
            dl class="kv" {
                dt { "Ingest URL" } dd { code { (ingest_url) } }
                dt { "Write key" } dd { "Listed under " a href=(format!("/apps/{slug}/settings")) { "Settings" } "." }
            }
            p class="note" { "Rollups are recomputed every five minutes, so new data can take a few minutes to appear." }
        }
    }
}

pub fn status_badge(status: &str) -> Markup {
    let label = match status {
        "open" => "Open",
        "resolved" => "Resolved",
        "ignored" => "Ignored",
        _ => "Unknown",
    };
    html! { span class=(format!("badge {}", if matches!(status, "open" | "resolved" | "ignored") { status } else { "unknown" })) { (label) } }
}

pub fn kind_label(kind: &str) -> String {
    kind.replace('_', " ")
}

/// Lets a wide table scroll sideways inside its own box instead of widening the page.
fn scrolled(table: Markup) -> Markup {
    html! { div class="scroll" { (table) } }
}

pub fn issue_table(slug: &str, issues: &[IssueRow], link: bool) -> Markup {
    scrolled(issue_table_inner(slug, issues, link))
}

pub fn perf_table(group_label: &str, rows: &[(String, PerfSummary)]) -> Markup {
    scrolled(perf_table_inner(group_label, rows))
}

pub fn count_table(name_label: &str, count_label: &str, rows: &[(String, i64)]) -> Markup {
    scrolled(count_table_inner(name_label, count_label, rows))
}

/// Wraps any table markup so it can scroll sideways.
pub fn scroll(table: Markup) -> Markup {
    scrolled(table)
}

/// The issue table used by the overview, the crash list and the report.
fn issue_table_inner(slug: &str, issues: &[IssueRow], link: bool) -> Markup {
    html! {
        table class="data" {
            thead { tr {
                th scope="col" { "Issue" } th scope="col" { "Kind" } th scope="col" class="num" { "Count" }
                th scope="col" class="num" { "Installs" } th scope="col" { "First seen" } th scope="col" { "Last seen" }
                th scope="col" { "Versions" } th scope="col" { "Status" }
            } }
            tbody {
                @for issue in issues {
                    tr {
                        td class="wrap" {
                            @if link { a href=(format!("/apps/{slug}/crashes/{}", issue.id)) { (fmt::head(&issue.title, 140)) } }
                            @else { (fmt::head(&issue.title, 140)) }
                        }
                        td { (kind_label(&issue.kind)) }
                        td class="num" { (fmt::int(issue.count)) }
                        td class="num" { (fmt::int(issue.installs)) }
                        td { (fmt::datetime(issue.first_seen)) }
                        td { (fmt::datetime(issue.last_seen)) }
                        td class="wrap" { (issue.versions.join(", ")) }
                        td { (status_badge(&issue.status)) }
                    }
                }
            }
        }
    }
}

/// A table of performance summaries, one row per group.
fn perf_table_inner(group_label: &str, rows: &[(String, PerfSummary)]) -> Markup {
    html! {
        table class="data" {
            thead { tr {
                th scope="col" { (group_label) } th scope="col" class="num" { "Frames" }
                th scope="col" class="num" { "p50" } th scope="col" class="num" { "p90" } th scope="col" class="num" { "p99" }
                th scope="col" class="num" { "Slow" } th scope="col" class="num" { "Frozen" }
                th scope="col" class="num" { "Startup" } th scope="col" class="num" { "CPU" } th scope="col" class="num" { "Memory" }
            } }
            tbody {
                @for (name, p) in rows {
                    tr {
                        th scope="row" { (name) }
                        td class="num" { (fmt::int(i64::try_from(p.frames).unwrap_or(i64::MAX))) }
                        td class="num" { (fmt::us_as_ms(p.p50_us.map(f64::from))) }
                        td class="num" { (fmt::us_as_ms(p.p90_us.map(f64::from))) }
                        td class="num" { (fmt::us_as_ms(p.p99_us.map(f64::from))) }
                        td class="num" { (fmt::pct(p.slow_rate)) }
                        td class="num" { (fmt::pct(p.frozen_rate)) }
                        td class="num" { (p.startup_ms.map_or_else(|| fmt::dec1(None), |ms| format!("{ms:.0} ms"))) }
                        td class="num" { (p.cpu_pct.map_or_else(|| fmt::dec1(None), |c| format!("{c:.1}%"))) }
                        td class="num" { (p.rss_mb.map_or_else(|| fmt::dec1(None), |m| format!("{m:.0} MB"))) }
                    }
                }
            }
        }
    }
}

/// A two-column table of `(name, count)`.
fn count_table_inner(name_label: &str, count_label: &str, rows: &[(String, i64)]) -> Markup {
    html! {
        table class="data narrow" {
            thead { tr { th scope="col" { (name_label) } th scope="col" class="num" { (count_label) } } }
            tbody { @for (name, count) in rows { tr { th scope="row" { (name) } td class="num" { (fmt::int(*count)) } } } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(enc("1.0.0-beta+1 &x=<"), "1.0.0-beta%2B1%20%26x%3D%3C");
        assert_eq!(enc("é"), "%C3%A9");
        let f = Filters { days: 7, version: Some("a&b".into()), kind: None, status: None };
        assert_eq!(query(&f, true), "days=7&version=a%26b");
        assert_eq!(query(&f, false), "days=7");
    }

    #[test]
    fn client_text_is_escaped_in_tables() {
        let issue = IssueRow {
            id: 1,
            kind: "panic".into(),
            title: "<script>alert(1)</script>".into(),
            first_seen: 0,
            last_seen: 0,
            first_version: "<b>".into(),
            last_version: "<b>".into(),
            count: 1,
            status: "open\"><i>".into(),
            installs: 1,
            versions: vec!["<img src=x>".into()],
        };
        let html = issue_table("demo", &[issue], true).into_string();
        assert!(!html.contains("<script>") && !html.contains("<img") && !html.contains("<i>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
