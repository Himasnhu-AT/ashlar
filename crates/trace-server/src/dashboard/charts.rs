//! Inline SVG charts. Pure functions: data in, markup out.
//!
//! Every chart is a `<figure>` holding an `<svg role="img">` with a `<title>`, and a visually hidden data
//! table as the text alternative. Colours come from CSS classes (`s1`..`s4`), never from inline styles, so
//! the page can keep a strict Content-Security-Policy and the charts follow the light/dark theme. Series
//! also differ by dash pattern, and every chart has axis labels with units, so colour is never the only cue.
//!
//! All text goes through maud's escaping. Non-finite and negative numbers are treated as zero, and every
//! scale has a positive maximum, so nothing divides by zero or prints `NaN`.

use maud::{Markup, html};

const W: f64 = 640.0;
const H: f64 = 240.0;
const LEFT: f64 = 76.0;
const RIGHT: f64 = 14.0;
const TOP: f64 = 12.0;
const BOTTOM: f64 = 46.0;
/// Most x-axis labels drawn on a time axis.
const MAX_X_LABELS: usize = 6;
/// Longest label drawn beside a ranking bar before it is cut with an ellipsis.
const MAX_BAR_LABEL: usize = 34;

/// Formats a value for an axis tick or a table cell.
pub type ValueFmt = fn(f64) -> String;

/// Compact numbers for axes: `950`, `12.5`, `12k`, `1.3M`.
pub fn compact(value: f64) -> String {
    let v = clean(value);
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else if v.fract() == 0.0 || v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// A non-negative finite number; anything else becomes zero.
pub fn clean(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 { value } else { 0.0 }
}

/// Step between ticks for a scale that reaches `max`, aiming for four intervals. With `integral` the step
/// is at least one, so count axes never show fractions.
pub fn tick_step(max: f64, integral: bool) -> f64 {
    let max = clean(max);
    let raw = if max > 0.0 { max / 4.0 } else { 0.25 };
    let base = 10f64.powf(raw.log10().floor());
    let f = raw / base;
    let nice = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 2.5 {
        2.5
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    let step = nice * base;
    let step = if integral { step.max(1.0) } else { step };
    if step.is_finite() && step > 0.0 { step } else { 1.0 }
}

/// The axis maximum and its ticks (`0`, step, 2 step ... maximum) for data reaching `max`. The maximum is
/// always positive.
pub fn axis(max: f64, integral: bool) -> (f64, Vec<f64>) {
    let step = tick_step(max, integral);
    let intervals = (((clean(max) / step) - 1e-9).ceil().max(1.0) as usize).min(10);
    let ticks: Vec<f64> = (0..=intervals).map(|i| step * i as f64).collect();
    (step * intervals as f64, ticks)
}

/// Horizontal position of point `index` of `count` across `[left, left + width]`. One point sits in the
/// middle; no points is still defined.
pub fn x_at(index: usize, count: usize, left: f64, width: f64) -> f64 {
    if count <= 1 { left + width / 2.0 } else { left + width * (index.min(count - 1) as f64) / ((count - 1) as f64) }
}

/// Vertical position of `value` on a scale `0..=axis_max` drawn in `[top, top + height]`, growing upward.
pub fn y_at(value: f64, axis_max: f64, top: f64, height: f64) -> f64 {
    let max = if axis_max.is_finite() && axis_max > 0.0 { axis_max } else { 1.0 };
    top + height - (clean(value) / max).min(1.0) * height
}

/// Indices of the x labels to draw: all of them when few, otherwise evenly spaced including both ends.
pub fn label_indices(count: usize) -> Vec<usize> {
    if count <= MAX_X_LABELS {
        return (0..count).collect();
    }
    let mut out: Vec<usize> = (0..MAX_X_LABELS).map(|k| k * (count - 1) / (MAX_X_LABELS - 1)).collect();
    out.dedup();
    out
}

fn n(value: f64) -> String {
    if value.is_finite() { format!("{value:.1}") } else { "0.0".to_string() }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
        cut.push('\u{2026}');
        cut
    }
}

fn class_for(index: usize) -> String {
    format!("s{}", index % 4 + 1)
}

// ---------------------------------------------------------------------------------------------------------
// line and area

pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

pub struct LineChart {
    pub title: String,
    pub x_label: String,
    /// Y axis label including the unit, for example `Sessions per day`.
    pub y_label: String,
    /// One label per point, oldest first.
    pub x: Vec<String>,
    pub series: Vec<Series>,
    /// Fill under the line.
    pub area: bool,
    /// Counts never show fractional ticks.
    pub integral: bool,
    pub format: ValueFmt,
}

/// A series with its index and the pixel position of each point.
type Drawn<'a> = (usize, &'a Series, Vec<(f64, f64)>);

pub fn line_chart(chart: &LineChart) -> Markup {
    let count = chart.x.len();
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let max = chart.series.iter().flat_map(|s| s.values.iter().take(count)).fold(0.0f64, |m, v| m.max(clean(*v)));
    let (axis_max, ticks) = axis(max, chart.integral);
    let baseline = TOP + plot_h;
    let labels = label_indices(count);
    let drawn: Vec<Drawn<'_>> = chart
        .series
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let points = (0..count)
                .map(|i| (x_at(i, count, LEFT, plot_w), y_at(series.values.get(i).copied().unwrap_or(0.0), axis_max, TOP, plot_h)))
                .collect();
            (index, series, points)
        })
        .collect();
    let summary = summary_text(&chart.title, count, &chart.series, chart.format);
    html! {
        figure class="chart" {
            div class="chart-scroll" {
                svg class="svg-chart" viewBox=(format!("0 0 {W} {H}")) role="img" {
                    title { (summary) }
                    (frame(&ticks, axis_max, chart.format, plot_w, plot_h))
                    @for i in &labels {
                        text class="tick" x=(n(x_at(*i, count, LEFT, plot_w))) y=(n(baseline + 16.0)) text-anchor=(if count > 1 && *i == count - 1 { "end" } else { "middle" }) {
                            (chart.x.get(*i).map(String::as_str).unwrap_or(""))
                        }
                    }
                    (axis_titles(&chart.x_label, &chart.y_label, plot_w, plot_h))
                    @if count == 0 { (no_data(plot_w, plot_h)) }
                    @for (index, series, points) in &drawn {
                        @if !points.is_empty() {
                            @let line = path(points);
                            @if chart.area && points.len() > 1 {
                                path class=(format!("area {}", class_for(*index))) d=(format!("{line}L{} {}L{} {}Z", n(points[points.len() - 1].0), n(baseline), n(points[0].0), n(baseline))) {}
                            }
                            @if points.len() > 1 {
                                path class=(format!("line {}", class_for(*index))) d=(line) fill="none" {
                                    title { (series.name) }
                                }
                            } @else {
                                circle class=(format!("dot {}", class_for(*index))) cx=(n(points[0].0)) cy=(n(points[0].1)) r="4" {
                                    title { (series.name) }
                                }
                            }
                        }
                    }
                }
            }
            @if chart.series.len() > 1 { (legend(chart.series.iter().map(|s| s.name.as_str()))) }
            (series_table(&chart.title, &chart.x_label, &chart.x, &chart.series, chart.format))
        }
    }
}

fn path(points: &[(f64, f64)]) -> String {
    let mut d = String::new();
    for (i, (x, y)) in points.iter().enumerate() {
        d.push_str(if i == 0 { "M" } else { "L" });
        d.push_str(&n(*x));
        d.push(' ');
        d.push_str(&n(*y));
    }
    d
}

fn summary_text(title: &str, count: usize, series: &[Series], format: ValueFmt) -> String {
    if count == 0 {
        return format!("{title}: no data");
    }
    let parts: Vec<String> = series
        .iter()
        .map(|s| {
            let values: Vec<f64> = s.values.iter().take(count).map(|v| clean(*v)).collect();
            let max = values.iter().copied().fold(0.0, f64::max);
            let last = values.last().copied().unwrap_or(0.0);
            format!("{}: latest {}, peak {}", s.name, format(last), format(max))
        })
        .collect();
    format!("{title}. {count} points. {}", parts.join("; "))
}

fn frame(ticks: &[f64], axis_max: f64, format: ValueFmt, plot_w: f64, plot_h: f64) -> Markup {
    html! {
        @for t in ticks {
            @let y = y_at(*t, axis_max, TOP, plot_h);
            line class="grid" x1=(n(LEFT)) x2=(n(LEFT + plot_w)) y1=(n(y)) y2=(n(y)) {}
            text class="tick" x=(n(LEFT - 6.0)) y=(n(y + 4.0)) text-anchor="end" { (format(*t)) }
        }
    }
}

fn axis_titles(x_label: &str, y_label: &str, plot_w: f64, plot_h: f64) -> Markup {
    let cx = LEFT + plot_w / 2.0;
    let cy = TOP + plot_h / 2.0;
    html! {
        text class="axis-title" x=(n(cx)) y=(n(H - 6.0)) text-anchor="middle" { (x_label) }
        text class="axis-title" x="12" y=(n(cy)) text-anchor="middle" transform=(format!("rotate(-90 12 {})", n(cy))) { (y_label) }
    }
}

fn no_data(plot_w: f64, plot_h: f64) -> Markup {
    html! {
        text class="note" x=(n(LEFT + plot_w / 2.0)) y=(n(TOP + plot_h / 2.0)) text-anchor="middle" { "No data in this range" }
    }
}

fn legend<'a>(names: impl Iterator<Item = &'a str>) -> Markup {
    html! {
        ul class="legend" {
            @for (i, name) in names.enumerate() {
                li { span class=(format!("swatch {}", class_for(i))) {} (name) }
            }
        }
    }
}

fn series_table(title: &str, x_label: &str, x: &[String], series: &[Series], format: ValueFmt) -> Markup {
    html! {
        table class="sr-only" {
            caption { (title) }
            thead { tr { th scope="col" { (x_label) } @for s in series { th scope="col" { (s.name) } } } }
            tbody {
                @for (i, label) in x.iter().enumerate() {
                    tr { th scope="row" { (label) } @for s in series { td { (format(s.values.get(i).copied().unwrap_or(0.0))) } } }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// vertical bars (histograms)

pub struct ColumnChart {
    pub title: String,
    pub x_label: String,
    pub y_label: String,
    pub items: Vec<(String, f64)>,
    pub integral: bool,
    pub format: ValueFmt,
}

pub fn column_chart(chart: &ColumnChart) -> Markup {
    let count = chart.items.len();
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let max = chart.items.iter().fold(0.0f64, |m, (_, v)| m.max(clean(*v)));
    let (axis_max, ticks) = axis(max, chart.integral);
    let slot = if count == 0 { plot_w } else { plot_w / count as f64 };
    let bar_w = (slot * 0.7).max(1.0);
    let baseline = TOP + plot_h;
    let every = if count > 12 { count.div_ceil(12) } else { 1 };
    let total: f64 = chart.items.iter().map(|(_, v)| clean(*v)).sum();
    html! {
        figure class="chart" {
            div class="chart-scroll" {
                svg class="svg-chart" viewBox=(format!("0 0 {W} {H}")) role="img" {
                    title { (format!("{}. {} bars, total {}", chart.title, count, (chart.format)(total))) }
                    (frame(&ticks, axis_max, chart.format, plot_w, plot_h))
                    (axis_titles(&chart.x_label, &chart.y_label, plot_w, plot_h))
                    @if count == 0 { (no_data(plot_w, plot_h)) }
                    @for (i, (label, value)) in chart.items.iter().enumerate() {
                        @let x = LEFT + slot * i as f64 + (slot - bar_w) / 2.0;
                        @let y = y_at(*value, axis_max, TOP, plot_h);
                        rect class="bar s1" x=(n(x)) y=(n(y)) width=(n(bar_w)) height=(n((baseline - y).max(0.0))) {
                            title { (format!("{label}: {}", (chart.format)(clean(*value)))) }
                        }
                        @if i % every == 0 {
                            text class="tick" x=(n(x + bar_w / 2.0)) y=(n(baseline + 16.0)) text-anchor="middle" { (label) }
                        }
                    }
                }
            }
            (item_table(&chart.title, &chart.x_label, &chart.y_label, &chart.items, chart.format))
        }
    }
}

fn item_table(title: &str, key_label: &str, value_label: &str, items: &[(String, f64)], format: ValueFmt) -> Markup {
    html! {
        table class="sr-only" {
            caption { (title) }
            thead { tr { th scope="col" { (key_label) } th scope="col" { (value_label) } } }
            tbody { @for (label, value) in items { tr { th scope="row" { (label) } td { (format(clean(*value))) } } } }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// horizontal bars (rankings)

pub struct BarChart {
    pub title: String,
    /// Unit of the values, shown under the bars.
    pub value_label: String,
    pub items: Vec<(String, f64)>,
    pub format: ValueFmt,
}

const ROW: f64 = 24.0;
const LABEL_W: f64 = 230.0;

pub fn bar_chart(chart: &BarChart) -> Markup {
    let count = chart.items.len();
    let max = chart.items.iter().fold(0.0f64, |m, (_, v)| m.max(clean(*v)));
    let scale = if max > 0.0 { max } else { 1.0 };
    let bars_w = W - LABEL_W - 70.0;
    let height = (count.max(1) as f64) * ROW + 30.0;
    html! {
        figure class="chart" {
            div class="chart-scroll" {
                svg class="svg-chart" viewBox=(format!("0 0 {W} {height}")) role="img" {
                    title { (format!("{}. {} items", chart.title, count)) }
                    @if count == 0 {
                        text class="note" x=(n(W / 2.0)) y="24" text-anchor="middle" { "No data in this range" }
                    }
                    @for (i, (label, value)) in chart.items.iter().enumerate() {
                        @let y = 4.0 + ROW * i as f64;
                        @let w = clean(*value) / scale * bars_w;
                        text class="tick" x=(n(LABEL_W - 8.0)) y=(n(y + 15.0)) text-anchor="end" {
                            title { (label) }
                            (truncate(label, MAX_BAR_LABEL))
                        }
                        rect class="bar s1" x=(n(LABEL_W)) y=(n(y + 3.0)) width=(n(w)) height="14" {
                            title { (format!("{label}: {}", (chart.format)(clean(*value)))) }
                        }
                        text class="tick" x=(n(LABEL_W + w + 6.0)) y=(n(y + 15.0)) { ((chart.format)(clean(*value))) }
                    }
                    text class="axis-title" x=(n(LABEL_W + bars_w / 2.0)) y=(n(height - 6.0)) text-anchor="middle" { (chart.value_label) }
                }
            }
            (item_table(&chart.title, "Item", &chart.value_label, &chart.items, chart.format))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// stacked bar

pub struct Stacked {
    pub title: String,
    /// Unit shown in the legend, for example `hours`.
    pub unit: String,
    pub parts: Vec<(String, f64)>,
}

/// Widths of each part of a bar `width` wide. Sums to `width` unless every part is zero, then all zero.
pub fn stack_widths(parts: &[f64], width: f64) -> Vec<f64> {
    let total: f64 = parts.iter().map(|p| clean(*p)).sum();
    if total <= 0.0 { vec![0.0; parts.len()] } else { parts.iter().map(|p| clean(*p) / total * width).collect() }
}

pub fn stacked_bar(chart: &Stacked) -> Markup {
    let values: Vec<f64> = chart.parts.iter().map(|(_, v)| clean(*v)).collect();
    let total: f64 = values.iter().sum();
    let bar_w = W - 2.0;
    let widths = stack_widths(&values, bar_w);
    let mut offsets = Vec::with_capacity(widths.len());
    let mut at = 1.0;
    for w in &widths {
        offsets.push(at);
        at += w;
    }
    let share = |v: f64| if total > 0.0 { format!("{:.1}%", v / total * 100.0) } else { "0.0%".to_string() };
    let summary = chart.parts.iter().zip(&values).map(|((name, _), v)| format!("{name} {}", share(*v))).collect::<Vec<_>>().join(", ");
    html! {
        figure class="chart" {
            div class="chart-scroll" {
                svg class="svg-chart short" viewBox=(format!("0 0 {W} 44")) role="img" {
                    title { (format!("{}. {}", chart.title, if total > 0.0 { summary.as_str() } else { "no data" })) }
                    rect class="track" x="1" y="6" width=(n(bar_w)) height="30" {}
                    @for (i, w) in widths.iter().enumerate() {
                        @if *w > 0.0 {
                            rect class=(format!("seg {}", class_for(i))) x=(n(offsets[i])) y="6" width=(n(*w)) height="30" {
                                title { (format!("{}: {}", chart.parts[i].0, share(values[i]))) }
                            }
                        }
                    }
                }
            }
            ul class="legend" {
                @for (i, (name, _)) in chart.parts.iter().enumerate() {
                    li { span class=(format!("swatch {}", class_for(i))) {} (name) " " span class="num" { (share(values[i])) } }
                }
            }
            table class="sr-only" {
                caption { (chart.title) }
                thead { tr { th scope="col" { "Part" } th scope="col" { "Share" } th scope="col" { (chart.unit) } } }
                tbody {
                    @for (i, (name, _)) in chart.parts.iter().enumerate() {
                        tr { th scope="row" { (name) } td { (share(values[i])) } td { (compact(values[i])) } }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bad(markup: &Markup) -> bool {
        let s = &markup.0;
        s.contains("NaN") || s.contains("=\"inf") || s.contains("-inf") || s.contains(" inf")
    }

    fn line(values: Vec<f64>, labels: usize) -> LineChart {
        LineChart {
            title: "Sessions".into(),
            x_label: "Day".into(),
            y_label: "Sessions per day".into(),
            x: (0..labels).map(|i| format!("d{i}")).collect(),
            series: vec![Series { name: "Sessions".into(), values }],
            area: true,
            integral: true,
            format: compact,
        }
    }

    #[test]
    fn steps_are_nice_and_positive() {
        assert_eq!(tick_step(100.0, false), 25.0);
        assert_eq!(tick_step(0.0, false), 0.25);
        assert_eq!(tick_step(0.0, true), 1.0);
        assert_eq!(tick_step(3.0, true), 1.0);
        assert_eq!(tick_step(1_000_000.0, true), 250_000.0);
        for bad in [f64::NAN, f64::INFINITY, -5.0] {
            assert!(tick_step(bad, false) > 0.0);
        }
    }

    #[test]
    fn axis_covers_the_data_and_starts_at_zero() {
        for max in [0.0, 0.3, 1.0, 7.0, 99.0, 101.0, 12_345.0, 1e12] {
            let (top, ticks) = axis(max, false);
            assert!(top >= max && top > 0.0, "{max} -> {top}");
            assert_eq!(ticks.first().copied(), Some(0.0));
            assert_eq!(ticks.last().copied(), Some(top));
            assert!(ticks.len() >= 2 && ticks.len() <= 11);
        }
        let (top, ticks) = axis(f64::NAN, true);
        assert_eq!((top, ticks), (1.0, vec![0.0, 1.0]));
    }

    #[test]
    fn positions_handle_zero_one_and_many_points() {
        assert_eq!(x_at(0, 0, 10.0, 100.0), 60.0);
        assert_eq!(x_at(0, 1, 10.0, 100.0), 60.0);
        assert_eq!(x_at(0, 5, 10.0, 100.0), 10.0);
        assert_eq!(x_at(4, 5, 10.0, 100.0), 110.0);
        assert_eq!(x_at(99, 5, 10.0, 100.0), 110.0);
        assert_eq!(y_at(0.0, 10.0, 5.0, 100.0), 105.0);
        assert_eq!(y_at(10.0, 10.0, 5.0, 100.0), 5.0);
        assert_eq!(y_at(50.0, 10.0, 5.0, 100.0), 5.0);
        assert_eq!(y_at(5.0, 0.0, 5.0, 100.0), 5.0);
        assert_eq!(y_at(f64::NAN, 10.0, 5.0, 100.0), 105.0);
        assert_eq!(label_indices(0), Vec::<usize>::new());
        assert_eq!(label_indices(3), vec![0, 1, 2]);
        let ten = label_indices(90);
        assert_eq!((ten.first(), ten.last(), ten.len()), (Some(&0), Some(&89), 6));
    }

    #[test]
    fn empty_single_zero_and_odd_series_render_cleanly() {
        let empty = line_chart(&line(vec![], 0));
        assert!(empty.0.contains("No data in this range"));
        assert!(!bad(&empty));

        let single = line_chart(&line(vec![5.0], 1));
        assert!(single.0.contains("<circle"));
        assert!(!single.0.contains("<path class=\"line"));
        assert!(!bad(&single));

        let zeros = line_chart(&line(vec![0.0; 30], 30));
        assert!(!bad(&zeros));
        assert!(zeros.0.contains("<path class=\"line s1\""));

        let odd = line_chart(&line(vec![f64::NAN, f64::INFINITY, -4.0, 2.0], 4));
        assert!(!bad(&odd));
        assert!(!odd.0.contains("NaN"));

        // Fewer values than labels is fine.
        assert!(!bad(&line_chart(&line(vec![1.0], 5))));
    }

    #[test]
    fn charts_have_titles_and_hidden_tables() {
        let html = line_chart(&line(vec![1.0, 2.0, 3.0], 3)).0;
        assert!(html.contains("<title>Sessions. 3 points."));
        assert!(html.contains("<table class=\"sr-only\">"));
        assert!(html.contains("Sessions per day"));
        assert!(html.contains("role=\"img\""));
    }

    #[test]
    fn text_is_escaped() {
        let mut chart = line(vec![1.0, 2.0], 2);
        chart.title = "<script>alert(1)</script>".into();
        chart.series[0].name = "\"><img src=x>".into();
        chart.x[0] = "<b>".into();
        let html = line_chart(&chart).0;
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<b>"));
        let bars =
            bar_chart(&BarChart { title: "t".into(), value_label: "n".into(), items: vec![("<script>x</script>".into(), 3.0)], format: compact });
        assert!(!bars.0.contains("<script>"));
    }

    #[test]
    fn bars_handle_empty_zero_and_long_labels() {
        let empty = bar_chart(&BarChart { title: "t".into(), value_label: "n".into(), items: vec![], format: compact });
        assert!(empty.0.contains("No data in this range") && !bad(&empty));
        let zero =
            bar_chart(&BarChart { title: "t".into(), value_label: "n".into(), items: vec![("a".into(), 0.0), ("b".into(), 0.0)], format: compact });
        assert!(!bad(&zero));
        let long = bar_chart(&BarChart { title: "t".into(), value_label: "n".into(), items: vec![("x".repeat(100), 9.0)], format: compact });
        assert!(long.0.contains('\u{2026}'));
        let cols = column_chart(&ColumnChart {
            title: "h".into(),
            x_label: "ms".into(),
            y_label: "frames".into(),
            items: (0..16).map(|i| (format!("b{i}"), 0.0)).collect(),
            integral: true,
            format: compact,
        });
        assert!(!bad(&cols));
        let none = column_chart(&ColumnChart {
            title: "h".into(),
            x_label: "x".into(),
            y_label: "y".into(),
            items: vec![],
            integral: true,
            format: compact,
        });
        assert!(none.0.contains("No data"));
    }

    #[test]
    fn stacks_sum_to_the_width() {
        let widths = stack_widths(&[1.0, 2.0, 1.0], 400.0);
        assert_eq!(widths, vec![100.0, 200.0, 100.0]);
        assert_eq!(stack_widths(&[0.0, 0.0], 400.0), vec![0.0, 0.0]);
        assert_eq!(stack_widths(&[f64::NAN, 4.0], 100.0), vec![0.0, 100.0]);
        assert!(stack_widths(&[], 10.0).is_empty());
        let zero = stacked_bar(&Stacked { title: "t".into(), unit: "h".into(), parts: vec![("Focused".into(), 0.0), ("Idle".into(), 0.0)] });
        assert!(zero.0.contains("no data") && !bad(&zero));
        let some = stacked_bar(&Stacked { title: "t".into(), unit: "h".into(), parts: vec![("Active".into(), 3.0), ("Idle".into(), 1.0)] });
        assert!(some.0.contains("75.0%") && some.0.contains("25.0%"));
    }
}
