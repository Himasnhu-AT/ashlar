//! Page bodies: overview, crash list, performance, usage, reports index and settings. Each function turns
//! loaded data into markup; the shell, headers and authentication are added by the handlers.

use maud::{Markup, html};

use super::charts::{self, BarChart, ColumnChart, LineChart, Series, Stacked};
use super::data::{CrashList, Filters, KINDS, Overview, PerfPage, STATUSES, SettingsPage, UsagePage};
use super::fmt;
use super::view::{self, Stat, kind_label};
use crate::store::{AppRecord, VersionInfo};

fn range_text(f: &Filters) -> String {
    match f.version() {
        Some(v) => format!("Last {} days, version {v}", f.days),
        None => format!("Last {} days, all versions", f.days),
    }
}

fn day_labels(from: i64, to: i64) -> Vec<String> {
    (from..=to).map(fmt::short_date).collect()
}

#[allow(clippy::too_many_arguments)]
fn line(title: &str, y_label: &str, from: i64, to: i64, series: Vec<Series>, integral: bool, area: bool, format: charts::ValueFmt) -> Markup {
    charts::line_chart(&LineChart {
        title: title.to_string(),
        x_label: "Day (UTC)".to_string(),
        y_label: y_label.to_string(),
        x: day_labels(from, to),
        series,
        area,
        integral,
        format,
    })
}

fn series(name: &str, values: Vec<f64>) -> Series {
    Series { name: name.to_string(), values }
}

fn us_to_ms(values: impl Iterator<Item = Option<u32>>) -> Vec<f64> {
    values.map(|v| v.map_or(0.0, |us| f64::from(us) / 1000.0)).collect()
}

fn ms_fmt(v: f64) -> String {
    format!("{} ms", charts::compact(v))
}

fn percent_fmt(v: f64) -> String {
    format!("{}%", charts::compact(v))
}

// ---------------------------------------------------------------------------------------------------------

pub fn app_picker(apps: &[(AppRecord, crate::store::AppGlance)]) -> Markup {
    html! {
        (view::heading("Apps", Some("Choose an app to see its crashes, performance and usage.")))
        @if apps.is_empty() {
            section class="empty" {
                h2 { "No apps yet" }
                p { "Create one on the server, then send data to it:" }
                p { code { "tracer-server app create <slug> <name>" } }
                p class="note" { "The command prints the app's first write key." }
            }
        } @else {
            div class="scroll" {
                table class="data" {
                    thead { tr {
                        th scope="col" { "App" } th scope="col" { "Slug" }
                        th scope="col" class="num" { "Opens, last 7 days" } th scope="col" class="num" { "Open issues" }
                    } }
                    tbody {
                        @for (app, glance) in apps {
                            tr {
                                th scope="row" { a href=(format!("/apps/{}", app.slug)) { (app.name) } }
                                td { code { (app.slug) } }
                                td class="num" { (fmt::int(glance.opens)) }
                                td class="num" { (fmt::int(glance.open_issues)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------

pub fn overview(app: &AppRecord, f: &Filters, o: &Overview, ingest_url: &str) -> Markup {
    html! {
        (view::heading(&app.name, Some(&range_text(f))))
        (view::filter_bar(&format!("/apps/{}", app.slug), f, &o.versions, html! {}))
        @if o.versions.is_empty() {
            (view::empty_state(&app.name, &app.slug, ingest_url))
        } @else {
            @let s = &o.summary;
            (view::stats(&[
                view::stat("Crash-free sessions", fmt::pct(s.crash_free_sessions), Some(&format!("{} of {} ended sessions", fmt::int(s.ended - s.crashed - s.abnormal), fmt::int(s.ended)))),
                view::stat("Crash-free installs", fmt::pct(s.crash_free_installs), Some(&format!("install-days with a crash: {}", fmt::int(s.crashed_install_days)))),
                view::stat("Opens today", fmt::int(o.opens_today), Some("GUI sessions started")),
                view::stat("Active installs, today", fmt::int(o.dau_today), Some("daily")),
                view::stat("Active installs, 7 days", fmt::int(o.wau), Some("weekly")),
                view::stat("Active installs, 30 days", fmt::int(o.mau), Some("monthly")),
            ]))
            h2 { "Sessions per day" }
            (line("Sessions and opens per day", "Sessions per day", o.from, o.to,
                vec![series("Sessions", o.sessions.clone()), series("Opens", o.opens.clone())], true, true, charts::compact))
            h2 { "Daily active installs" }
            (line("Active installs per day", "Active installs", o.from, o.to, vec![series("Active installs", o.dau.clone())], true, true, charts::compact))
            h2 { "Newest open issues" }
            @if o.issues.is_empty() {
                p { "No open issues." }
            } @else {
                (view::issue_table(&app.slug, &o.issues, true))
                p { a href=(format!("/apps/{}/crashes?{}", app.slug, view::query(f, true))) { "All issues" } }
            }
            p class="note" {
                "Crash-free sessions = 1 \u{2212} (crashed + abnormal) \u{00f7} sessions that have ended. "
                "Crash-free installs is counted per day over the range. Opens are GUI sessions started that day."
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------

pub fn crashes(app: &AppRecord, f: &Filters, list: &CrashList, ingest_url: &str) -> Markup {
    let extra = html! {
        label { span { "Kind" }
            select name="kind" {
                option value="" selected[f.kind.is_none()] { "All kinds" }
                @for k in KINDS { option value=(k) selected[f.kind.as_deref() == Some(k)] { (kind_label(k)) } }
            }
        }
        label { span { "Status" }
            select name="status" {
                option value="" selected[f.status.is_none()] { "Any status" }
                @for s in STATUSES { option value=(s) selected[f.status.as_deref() == Some(s)] { (s) } }
            }
        }
    };
    html! {
        (view::heading("Crashes", Some(&range_text(f))))
        (view::filter_bar(&format!("/apps/{}/crashes", app.slug), f, &list.versions, extra))
        @if list.versions.is_empty() && list.issues.is_empty() {
            (view::empty_state(&app.name, &app.slug, ingest_url))
        } @else if list.issues.is_empty() {
            p { "No issues match these filters." }
        } @else {
            p class="note" { (list.issues.len()) " issues last seen in this range. Count is all-time occurrences of the issue." }
            (view::issue_table(&app.slug, &list.issues, true))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------

fn rate_stat(label: &str, rate: Option<f64>) -> Stat {
    view::stat(label, fmt::pct(rate), None)
}

pub fn performance(app: &AppRecord, f: &Filters, p: &PerfPage, ingest_url: &str) -> Markup {
    let o = &p.overall;
    html! {
        (view::heading("Performance", Some(&range_text(f))))
        (view::filter_bar(&format!("/apps/{}/performance", app.slug), f, &p.versions, html! {}))
        @if p.versions.is_empty() {
            (view::empty_state(&app.name, &app.slug, ingest_url))
        } @else if o.frames == 0 && p.by_version.is_empty() {
            p { "No performance data in this range." }
        } @else {
            (view::stats(&[
                view::stat("Frame time p50", fmt::us_as_ms(o.p50_us.map(f64::from)), None),
                view::stat("Frame time p90", fmt::us_as_ms(o.p90_us.map(f64::from)), None),
                view::stat("Frame time p99", fmt::us_as_ms(o.p99_us.map(f64::from)), Some(&format!("worst {}", fmt::us_as_ms(Some(f64::from(o.max_us)))))),
                rate_stat("Slow frames", o.slow_rate),
                rate_stat("Frozen frames", o.frozen_rate),
                view::stat("Startup (mean)", o.startup_ms.map_or_else(|| fmt::dec1(None), |ms| format!("{ms:.0} ms")), None),
                view::stat("CPU (mean)", o.cpu_pct.map_or_else(|| fmt::dec1(None), |c| format!("{c:.1}%")), None),
                view::stat("Memory (mean)", o.rss_mb.map_or_else(|| fmt::dec1(None), |m| format!("{m:.0} MB")), Some(&format!("peak {} MB", fmt::int(o.rss_peak_mb)))),
            ]))
            p class="note" {
                "Percentiles come from histograms merged across windows, so they are bucket upper bounds, not exact values. "
                "Slow is over 16.7 ms, frozen over 700 ms. A day without data is drawn as zero."
            }
            h2 { "Frame time over time" }
            (line("Frame time percentiles per day", "Frame time (ms)", p.from, p.to, vec![
                series("p50", us_to_ms(p.daily.iter().map(|d| d.p50_us))),
                series("p90", us_to_ms(p.daily.iter().map(|d| d.p90_us))),
                series("p99", us_to_ms(p.daily.iter().map(|d| d.p99_us))),
            ], false, false, ms_fmt))
            h2 { "Slow and frozen frames" }
            (line("Share of slow and frozen frames per day", "Frames (%)", p.from, p.to, vec![
                series("Slow", p.daily.iter().map(|d| d.slow_rate.unwrap_or(0.0) * 100.0).collect()),
                series("Frozen", p.daily.iter().map(|d| d.frozen_rate.unwrap_or(0.0) * 100.0).collect()),
            ], false, false, percent_fmt))
            h2 { "Frame time distribution" }
            (charts::column_chart(&ColumnChart {
                title: "Frames per frame-time bucket".into(),
                x_label: "Frame time (ms, upper bound of bucket)".into(),
                y_label: "Frames".into(),
                items: p.histogram.clone(),
                integral: true,
                format: charts::compact,
            }))
            h2 { "CPU and memory" }
            (line("Mean CPU use per day", "CPU (% of one core)", p.from, p.to, vec![series("CPU", p.daily.iter().map(|d| d.cpu_pct.unwrap_or(0.0)).collect())], false, true, percent_fmt))
            (line("Mean memory per day", "Resident memory (MB)", p.from, p.to, vec![series("Memory", p.daily.iter().map(|d| d.rss_mb.unwrap_or(0.0)).collect())], false, true, charts::compact))
            h2 { "By version" }
            (view::perf_table("Version", &p.by_version))
            h2 { "By operating system" }
            (view::perf_table("Operating system", &p.by_os))
            @if p.custom.windows > 0 {
                h2 { "Host-defined metrics" }
                p class="note" { "From the " (fmt::int(p.custom.windows as i64)) " most recent reporting windows that carried counters or gauges. Counters are summed, gauges averaged." }
                div class="cols" {
                    @if !p.custom.counters.is_empty() {
                        div { h3 { "Counters" }
                            div class="scroll" { table class="data narrow" {
                                thead { tr { th scope="col" { "Name" } th scope="col" class="num" { "Total" } } }
                                tbody { @for (name, total) in &p.custom.counters { tr { th scope="row" { (name) } td class="num" { (fmt::int(i64::try_from(*total).unwrap_or(i64::MAX))) } } } }
                            } }
                        }
                    }
                    @if !p.custom.gauges.is_empty() {
                        div { h3 { "Gauges" }
                            div class="scroll" { table class="data narrow" {
                                thead { tr { th scope="col" { "Name" } th scope="col" class="num" { "Mean" } } }
                                tbody { @for (name, mean) in &p.custom.gauges { tr { th scope="row" { (name) } td class="num" { (fmt::dec1(Some(*mean))) } } } }
                            } }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------

fn event_bars(title: &str, value_label: &str, rows: Vec<(String, f64)>) -> Markup {
    charts::bar_chart(&BarChart { title: title.to_string(), value_label: value_label.to_string(), items: rows, format: charts::compact })
}

pub fn usage(app: &AppRecord, f: &Filters, u: &UsagePage, ingest_url: &str) -> Markup {
    let s = &u.summary;
    let focused_only = (s.focused_s - s.active_s).max(0) as f64;
    let unfocused = (s.duration_s - s.focused_s).max(0) as f64;
    let hours = |secs: f64| secs / 3600.0;
    let mut by_count = u.events.clone();
    by_count.truncate(15);
    let mut by_installs = u.events.clone();
    by_installs.sort_by(|a, b| b.installs.cmp(&a.installs).then_with(|| b.count.cmp(&a.count)).then_with(|| a.name.cmp(&b.name)));
    by_installs.truncate(15);
    html! {
        (view::heading("Usage", Some(&range_text(f))))
        (view::filter_bar(&format!("/apps/{}/usage", app.slug), f, &u.versions, html! {}))
        @if u.versions.is_empty() {
            (view::empty_state(&app.name, &app.slug, ingest_url))
        } @else {
            (view::stats(&[
                view::stat("Sessions", fmt::int(s.sessions), None),
                view::stat("Average session", fmt::seconds(s.avg_session_s), None),
                view::stat("Total time in app", fmt::seconds(Some(s.duration_s as f64)), None),
                view::stat("Events recorded", fmt::int(u.events.iter().map(|e| e.count).sum()), Some("top 200 event names")),
            ]))
            h2 { "Active installs" }
            (line("Active installs per day", "Active installs", u.from, u.to, vec![series("Active installs", u.dau.clone())], true, true, charts::compact))
            h2 { "Sessions per day" }
            (line("Sessions per day", "Sessions per day", u.from, u.to, vec![series("Sessions", u.sessions.clone())], true, true, charts::compact))
            h2 { "How session time is spent" }
            (charts::stacked_bar(&Stacked {
                title: "Session time by activity".into(),
                unit: "hours".into(),
                parts: vec![
                    ("Active (focused, recent input)".into(), hours(s.active_s as f64)),
                    ("Focused, no recent input".into(), hours(focused_only)),
                    ("Unfocused".into(), hours(unfocused)),
                ],
            }))
            p class="note" { "Idle time is the last two parts together. Totals come from the session heartbeats the SDK sent." }
            h2 { "Events by count" }
            (event_bars("Top events by count", "Events", by_count.iter().map(|e| (e.name.clone(), e.count as f64)).collect()))
            h2 { "Events by installs" }
            (event_bars("Top events by installs", "Install-days (installs per day, summed)", by_installs.iter().map(|e| (e.name.clone(), e.installs as f64)).collect()))
            @if !u.events.is_empty() {
                div class="scroll" {
                    table class="data" {
                        thead { tr { th scope="col" { "Event" } th scope="col" { "Category" } th scope="col" class="num" { "Count" } th scope="col" class="num" { "Install-days" } } }
                        tbody { @for e in &u.events { tr { th scope="row" { (e.name) } td { (e.category) } td class="num" { (fmt::int(e.count)) } td class="num" { (fmt::int(e.installs)) } } } }
                    }
                }
            }
            h2 { "Declared but never fired" }
            @if u.never_fired.is_empty() {
                p { "Every declared event has fired at least once, or no catalog has been received." }
            } @else {
                p class="note" { "Events in the catalog sent by the SDK that have no recorded use on any day." }
                div class="scroll" {
                    table class="data narrow" {
                        thead { tr { th scope="col" { "Event" } th scope="col" { "Category" } } }
                        tbody { @for e in &u.never_fired { tr { th scope="row" { (e.name) } td { (e.category) } } } }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------

pub fn reports_index(app: &AppRecord, versions: &[VersionInfo], ingest_url: &str) -> Markup {
    html! {
        (view::heading("Reports", Some("A stability and usage report for one release.")))
        @if versions.is_empty() {
            (view::empty_state(&app.name, &app.slug, ingest_url))
        } @else {
            form class="filters" method="get" action=(format!("/apps/{}/reports", app.slug)) {
                label { span { "Version" }
                    select name="version" { @for v in versions.iter().rev() { option value=(v.version) { (v.version) } } }
                }
                button type="submit" class="primary" { "Show report" }
            }
            div class="scroll" {
                table class="data" {
                    thead { tr {
                        th scope="col" { "Version" } th scope="col" { "First seen" } th scope="col" { "Last seen" }
                        th scope="col" class="num" { "Sessions" } th scope="col" { "Report" }
                    } }
                    tbody {
                        @for v in versions.iter().rev() {
                            tr {
                                th scope="row" { (v.version) }
                                td { (fmt::date(v.first_day)) } td { (fmt::date(v.last_day)) }
                                td class="num" { (fmt::int(v.sessions)) }
                                td { a href=(format!("/apps/{}/reports?version={}", app.slug, view::enc(&v.version))) { "View" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn report_page(app: &AppRecord, report: &super::report::Report) -> Markup {
    let base = format!("/apps/{}/reports?version={}", app.slug, view::enc(&report.version));
    html! {
        p { a href=(format!("/apps/{}/reports", app.slug)) { "\u{2190} All reports" } }
        p class="actions" {
            "Download: "
            a href=(format!("{base}&format=html")) { "HTML" } " "
            a href=(format!("{base}&format=md")) { "Markdown" } " "
            a href=(format!("{base}&format=json")) { "JSON" }
        }
        (super::report::body(report, true))
    }
}

// ---------------------------------------------------------------------------------------------------------

fn masked(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 12 {
        return "\u{2026}".to_string(); // too short to show any part safely
    }
    format!("{}\u{2026}{}", chars[..7].iter().collect::<String>(), chars[chars.len() - 4..].iter().collect::<String>())
}

/// The settings page. `new_key` is shown once, right after creation.
pub fn settings(app: &AppRecord, s: &SettingsPage, csrf: &str, ingest_url: &str, new_key: Option<&str>) -> Markup {
    html! {
        (view::heading("Settings", Some(&app.name)))
        @if let Some(key) = new_key {
            div class="banner" role="status" {
                strong { "New write key. Copy it now; it is not shown in full again." }
                p { code { (key) } }
            }
        }
        h2 { "App" }
        dl class="kv" {
            dt { "Name" } dd { (app.name) }
            dt { "Slug" } dd { code { (app.slug) } }
            dt { "Created" } dd { (fmt::datetime(app.created_at)) }
            dt { "Ingest URL" } dd { code { (ingest_url) } }
            dt { "Opens, last 7 days" } dd { (fmt::int(s.glance.opens)) }
            dt { "Open issues" } dd { (fmt::int(s.glance.open_issues)) }
        }
        h2 { "Write keys" }
        p class="note" { "A write key only allows sending data. It ships inside the host app, so treat it as public. Revoke a key to stop it being accepted." }
        div class="scroll" {
            table class="data" {
                thead { tr { th scope="col" { "Key" } th scope="col" { "Created" } th scope="col" { "State" } th scope="col" { "Action" } } }
                tbody {
                    @for k in &s.keys {
                        tr {
                            td { code { (masked(&k.key)) } }
                            td { (fmt::datetime(k.created_at)) }
                            td { @if let Some(at) = k.revoked_at { "Revoked " (fmt::datetime(at)) } @else { "Active" } }
                            td {
                                @if k.revoked_at.is_none() {
                                    form method="post" action=(format!("/apps/{}/settings/keys/{}/revoke", app.slug, super::key_id(&k.key))) {
                                        input type="hidden" name="csrf" value=(csrf);
                                        button type="submit" { "Revoke" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        form method="post" action=(format!("/apps/{}/settings/keys", app.slug)) class="actions" {
            input type="hidden" name="csrf" value=(csrf);
            button type="submit" class="primary" { "Create write key" }
        }
        h2 { "Data the SDK dropped" }
        p class="note" { "Counts reported by the SDK for the last " (s.to - s.from + 1) " days (" (fmt::date(s.from)) " to " (fmt::date(s.to)) "). Dropped data never reached this server." }
        @if s.drops.is_empty() {
            p { "No drops reported." }
        } @else {
            (view::count_table("Reason", "Items dropped", &s.drops.iter().map(|d| (d.reason.clone(), d.count)).collect::<Vec<_>>()))
        }
    }
}
