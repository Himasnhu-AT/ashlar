//! The page for one crash issue. Everything in a crash record came from a client, so all of it is shown as
//! escaped text.

use maud::{Markup, html};
use tracer_proto::{Breadcrumb, Device, Perf};

use super::data::IssueDetail;
use super::fmt;
use super::view::{self, kind_label};
use crate::store::{AppRecord, CrashRow};

/// Longest backtrace or message shown, in characters. The SDK already truncates, but the server does not
/// trust that.
const MAX_TEXT: usize = 64 * 1024;
const MAX_CRUMBS: usize = 200;

fn clip(text: &str) -> String {
    fmt::head(text, MAX_TEXT)
}

fn status_form(app: &AppRecord, id: i64, csrf: &str, status: &str, label: &str) -> Markup {
    html! {
        form method="post" action=(format!("/apps/{}/crashes/{id}/status", app.slug)) {
            input type="hidden" name="csrf" value=(csrf);
            input type="hidden" name="status" value=(status);
            button type="submit" { (label) }
        }
    }
}

fn device_rows(crash: &CrashRow) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    let os = [crash.os_name.as_deref(), crash.os_version.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
    if !os.is_empty() {
        rows.push(("Operating system", os));
    }
    if let Some(arch) = &crash.arch {
        rows.push(("Architecture", arch.clone()));
    }
    if let Some(device) = crash.device.as_deref().and_then(|text| serde_json::from_str::<Device>(text).ok()) {
        if let Some(cpu) = device.cpu_brand {
            let cores = device.cpu_cores.map_or(String::new(), |n| format!(" ({n} cores)"));
            rows.push(("CPU", format!("{cpu}{cores}")));
        }
        if let Some(ram) = device.ram_mb {
            rows.push(("Memory", format!("{} MB", fmt::int(i64::try_from(ram).unwrap_or(i64::MAX)))));
        }
        if let Some(gpu) = device.gpu {
            let backend = gpu.backend.map_or(String::new(), |b| format!(" via {b}"));
            rows.push(("GPU", format!("{}{backend}", gpu.name)));
        }
        if let Some(locale) = device.locale {
            rows.push(("Locale", locale));
        }
    }
    rows
}

fn perf_rows(perf: &Perf) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Window", format!("{} s", perf.window_s))];
    if let Some(frames) = &perf.frames {
        rows.push(("Frames", fmt::int(i64::try_from(frames.count).unwrap_or(i64::MAX))));
        for (label, q) in [("Frame time p50", 0.50), ("Frame time p90", 0.90), ("Frame time p99", 0.99)] {
            rows.push((label, fmt::us_as_ms(frames.quantile_us(q).map(f64::from))));
        }
        rows.push(("Slow frames", fmt::int(i64::try_from(frames.slow).unwrap_or(i64::MAX))));
        rows.push(("Frozen frames", fmt::int(i64::try_from(frames.frozen).unwrap_or(i64::MAX))));
    }
    if let Some(cpu) = perf.cpu_pct {
        rows.push(("CPU", format!("{cpu:.1}%")));
    }
    if let Some(rss) = perf.rss_mb {
        rows.push(("Memory", format!("{rss} MB")));
    }
    if let Some(peak) = perf.rss_peak_mb {
        rows.push(("Peak memory", format!("{peak} MB")));
    }
    rows
}

fn kv(rows: &[(&str, String)]) -> Markup {
    html! { dl class="kv" { @for (k, v) in rows { dt { (k) } dd { (v) } } } }
}

fn crash_details(crash: &CrashRow) -> Markup {
    let breadcrumbs: Vec<Breadcrumb> = crash.breadcrumbs.as_deref().and_then(|text| serde_json::from_str(text).ok()).unwrap_or_default();
    let perf: Option<Perf> = crash.perf.as_deref().and_then(|text| serde_json::from_str(text).ok());
    let mut facts: Vec<(&str, String)> = vec![("When", fmt::datetime(crash.ts)), ("Version", crash.version.clone())];
    if let Some(build) = &crash.build {
        facts.push(("Build", build.clone()));
    }
    facts.push(("Fatal", if crash.fatal { "yes" } else { "no" }.to_string()));
    if let Some(thread) = &crash.thread {
        facts.push(("Thread", thread.clone()));
    }
    if let Some(location) = &crash.location {
        facts.push(("Location", location.clone()));
    }
    if let Some(ms) = crash.blocked_ms {
        facts.push(("Blocked for", format!("{} ms", fmt::int(ms))));
    }
    if let Some(code) = crash.exit_code {
        facts.push(("Exit code", code.to_string()));
    }
    if let Some(signal) = crash.signal {
        facts.push(("Signal", signal.to_string()));
    }
    if let Some(context) = &crash.context {
        facts.push(("Context", context.clone()));
    }
    let device = device_rows(crash);
    html! {
        h2 { "Example occurrence" }
        (kv(&facts))
        @if let Some(message) = crash.message.as_deref().filter(|m| !m.is_empty()) {
            h3 { "Message" }
            pre class="trace wrapped" { (clip(message)) }
        }
        h3 { "Backtrace" }
        @if let Some(trace) = crash.backtrace.as_deref().filter(|t| !t.trim().is_empty()) {
            pre class="trace" { (clip(trace)) }
        } @else {
            p { "No backtrace was captured for this occurrence." }
        }
        div class="cols" {
            div {
                h3 { "Device" }
                @if device.is_empty() { p { "No device details." } } @else { (kv(&device)) }
            }
            div {
                h3 { "Last performance window" }
                @match &perf {
                    Some(perf) => { (kv(&perf_rows(perf))) }
                    None => { p { "No performance window was attached." } }
                }
            }
        }
        h3 { "Breadcrumbs" }
        @if breadcrumbs.is_empty() {
            p { "No log lines were attached." }
        } @else {
            p class="note" { "Recent log lines before the event, oldest first." }
            ol class="crumbs" {
                @for b in breadcrumbs.iter().rev().take(MAX_CRUMBS).rev() {
                    li {
                        span class="note" { (fmt::datetime(i64::try_from(b.ts).unwrap_or(0))) } " "
                        span class=(format!("lvl {}", if matches!(b.level.as_str(), "error" | "warn") { b.level.as_str() } else { "other" })) { (fmt::head(&b.level, 8)) }
                        " " span class="note" { (fmt::head(&b.target, 80)) } " " (fmt::head(&b.message, 512))
                    }
                }
            }
        }
    }
}

pub fn issue_page(app: &AppRecord, d: &IssueDetail, csrf: &str) -> Markup {
    let i = &d.issue;
    html! {
        p { a href=(format!("/apps/{}/crashes", app.slug)) { "\u{2190} All issues" } }
        div class="page-head" {
            h1 { (fmt::head(&i.title, 200)) }
            p class="sub" { (kind_label(&i.kind)) " " (view::status_badge(&i.status)) }
        }
        div class="actions" {
            @match i.status.as_str() {
                "open" => {
                    (status_form(app, i.id, csrf, "resolved", "Resolve"))
                    (status_form(app, i.id, csrf, "ignored", "Ignore"))
                }
                "resolved" => {
                    (status_form(app, i.id, csrf, "open", "Reopen"))
                    (status_form(app, i.id, csrf, "ignored", "Ignore"))
                }
                _ => { (status_form(app, i.id, csrf, "open", "Reopen")) }
            }
        }
        (view::stats(&[
            view::stat("Occurrences", fmt::int(i.count), None),
            view::stat("Affected installs", fmt::int(i.installs), None),
            view::stat("First seen", fmt::datetime(i.first_seen), Some(&format!("version {}", i.first_version))),
            view::stat("Last seen", fmt::datetime(i.last_seen), Some(&format!("version {}", i.last_version))),
        ]))
        @match &d.crash {
            Some(crash) => { (crash_details(crash)) }
            None => {
                h2 { "Example occurrence" }
                p { "The raw occurrences of this issue are no longer stored (they were removed by retention)." }
            }
        }
        h2 { "Breakdown" }
        div class="cols" {
            div { h3 { "By version" } (view::count_table("Version", "Crashes", &d.breakdown.versions)) }
            div { h3 { "By operating system" } (view::count_table("Operating system", "Crashes", &d.breakdown.os)) }
        }
        h2 { "Recent occurrences" }
        @if d.recent.is_empty() {
            p { "None stored." }
        } @else {
            div class="scroll" {
                table class="data" {
                    thead { tr {
                        th scope="col" { "When" } th scope="col" { "Version" } th scope="col" { "Operating system" }
                        th scope="col" { "Install" } th scope="col" { "Fatal" }
                    } }
                    tbody {
                        @for c in &d.recent {
                            tr {
                                td { (fmt::datetime(c.ts)) }
                                td { (c.version) }
                                td { (c.os_name.as_deref().unwrap_or("unknown")) @if let Some(v) = &c.os_version { " " (v) } }
                                td { code { (fmt::head(&c.install_id, 8)) } }
                                td { (if c.fatal { "Yes" } else { "No" }) }
                            }
                        }
                    }
                }
            }
        }
    }
}
