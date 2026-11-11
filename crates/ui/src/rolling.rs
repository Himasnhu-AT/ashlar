//! Rolling text: counters, timers and live readouts that change without jumping.
//!
//! [`RollingText`] matches the old and new text cluster by cluster (common prefix and suffix,
//! then the longest common subsequence for short middles). Unchanged clusters stay put; changed
//! ones roll up when the value rises and down when it falls, and the width follows. Under reduced
//! motion the changed clusters crossfade in place. The accessible name is always the whole new
//! text, from the first frame of a change. Once a roll settles nothing is animating and no frames
//! are asked for; showing the same text again is a no-op.
//!
//! ```no_run
//! # fn f(ui: &mut egui::Ui, cost: f64) {
//! let text = format!("${cost:.2}");
//! ashlar_ui::rolling::RollingText::new("usage.cost", &text).value(cost).tabular(true).show(ui);
//! # }
//! ```

use std::sync::Arc;

use ashlar_motion::{Class, tokens as mtokens};
use egui::{Color32, FontId, Galley, Id, Pos2, Rect, Response, Sense, Ui, Vec2, WidgetInfo, WidgetType, text::LayoutJob};

use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};
use crate::{cache, motion};

/// Middles longer than this (in clusters, either side) are not aligned by LCS: every cluster in
/// them is treated as changed.
const LCS_MAX: usize = 32;
/// The progress value starts over at 0 past this many changes.
const RESTART: f32 = 1.0e6;

/// A line of text whose changes roll.
#[must_use = "call .show(ui)"]
pub struct RollingText<'a> {
    id: &'a str,
    text: &'a str,
    value: Option<f64>,
    tabular: bool,
    font: Option<FontId>,
    color: Option<Color32>,
}

impl<'a> RollingText<'a> {
    /// `id` names the widget for automation and keeps its state; `text` is what to show now.
    pub fn new(id: &'a str, text: &'a str) -> Self {
        RollingText { id, text, value: None, tabular: false, font: None, color: None }
    }

    /// The number the text shows: the roll goes up when it rises and down when it falls (without
    /// one, or when it is not finite, changes roll up).
    pub fn value(mut self, value: f64) -> Self {
        self.value = Some(value);
        self
    }

    /// Give every digit the same width, so a counter does not wobble as it changes.
    pub fn tabular(mut self, tabular: bool) -> Self {
        self.tabular = tabular;
        self
    }

    /// The font (default: the body size).
    pub fn font(mut self, font: FontId) -> Self {
        self.font = Some(font);
        self
    }

    /// The text colour (default: primary text).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let font = self.font.clone().unwrap_or_else(|| Tokens::font(t.type_scale.body));
        let color = self.color.unwrap_or(t.text.primary);
        let id = ui.make_persistent_id(self.id);
        let progress_id = id.with("roll");
        let ctx = ui.ctx().clone();

        // shared, so an idle frame clones a pointer, not the columns
        let mut state = ui.data(|d| d.get_temp::<Arc<Roll>>(id)).unwrap_or_else(|| Arc::new(Roll::settled(self.text, self.value)));
        if state.text != self.text {
            let dir = match (state.value, self.value) {
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() && b < a => -1.0,
                _ => 1.0,
            };
            // the progress value only ever climbs, one unit per change: a change during a roll
            // carries on from where the progress is (no jump, no reversal)
            let (base, target) = if state.rolling {
                (state.last, state.target + 1.0)
            } else {
                // start from where the value rests (it may have been forgotten while idle); a
                // very long-lived counter starts over before f32 steps get coarse
                let at = if state.target > RESTART { 0.0 } else { state.target };
                motion::set(&ctx, progress_id, at);
                (at, at + 1.0)
            };
            state = Arc::new(Roll {
                cols: diff(&state.text, self.text),
                text: self.text.to_owned(),
                value: self.value,
                dir,
                base,
                target,
                last: base,
                rolling: true,
            });
        } else if state.value != self.value {
            Arc::make_mut(&mut state).value = self.value;
        }

        let mut p = 1.0_f32;
        if state.rolling {
            let roll = Arc::make_mut(&mut state);
            let v = motion::value(&ctx, progress_id, roll.target, Class::Opacity, mtokens::SETTLE.enter);
            roll.last = v;
            if v >= roll.target {
                roll.rolling = false;
                roll.cols = diff(self.text, self.text);
            } else {
                p = ((v - roll.base) / (roll.target - roll.base).max(f32::EPSILON)).clamp(0.0, 1.0);
            }
        }

        // lay out every cluster once (cached across frames)
        let revision = cache::hash_of(&font);
        let cell = if self.tabular { digit_width(ui, id, &font, revision) } else { 0.0 };
        let glyph = |s: &str| -> Arc<Galley> {
            cache::galley(ui, (id, "glyph", s), revision, || LayoutJob::simple_singleline(s.to_owned(), font.clone(), Color32::WHITE))
        };
        let width_of = |g: &Galley, s: &str| if cell > 0.0 && is_digit(s) { cell.max(g.size().x) } else { g.size().x };
        let mut height = glyph("0").size().y;
        let mut cols: Vec<Drawn> = Vec::with_capacity(state.cols.len());
        let mut final_width = 0.0;
        for c in &state.cols {
            let old = c.old.as_deref().map(|s| (glyph(s), s));
            let new = c.new.as_deref().map(|s| (glyph(s), s));
            let w_old = old.as_ref().map_or(0.0, |(g, s)| width_of(g, s));
            let w_new = new.as_ref().map_or(0.0, |(g, s)| width_of(g, s));
            for (g, _) in old.iter().chain(new.iter()) {
                height = height.max(g.size().y);
            }
            final_width += w_new;
            let w = if c.old == c.new { w_new } else { w_old + (w_new - w_old) * p };
            cols.push(Drawn { same: c.old == c.new, old: old.map(|(g, _)| g), new: new.map(|(g, _)| g), w });
        }
        let width = motion::value(&ctx, id.with("width"), final_width, Class::Spatial, mtokens::SETTLE.enter);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width.max(0.0), height), Sense::hover());

        if ui.is_rect_visible(rect) {
            let drawn: f32 = cols.iter().map(|c| c.w).sum();
            let clip = Rect::from_min_size(rect.min, Vec2::new(width.max(drawn), height)).intersect(ui.clip_rect());
            let painter = ui.painter().with_clip_rect(clip);
            let reduced = motion::prefs(&ctx).reduced;
            let mut x = rect.min.x;
            for c in &cols {
                if c.same {
                    if let Some(g) = &c.new {
                        let pos = Pos2::new(x + (c.w - g.size().x) * 0.5, rect.min.y);
                        painter.galley_with_override_text_color(pos, g.clone(), color);
                    }
                } else {
                    let (old_dy, new_dy) = offsets(p, state.dir, reduced, height);
                    for (g, dy, alpha) in [(&c.old, old_dy, 1.0 - p), (&c.new, new_dy, p)] {
                        if let Some(g) = g {
                            let pos = Pos2::new(x + (c.w - g.size().x) * 0.5, rect.min.y + dy);
                            painter.galley_with_override_text_color(pos, g.clone(), color.gamma_multiply(alpha));
                        }
                    }
                }
                x += c.w;
            }
        }

        let text = self.text;
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
        automation::register_with(ui, &response, self.id, text, Role::Label, Details { value: Some(text), selected: None });
        ui.data_mut(|d| d.insert_temp(id, state));
        response
    }
}

/// The vertical offsets of the leaving and arriving glyph at progress `p` for a roll in `dir`
/// (+1 up, -1 down) through a row `height` tall: the arriving glyph comes from below when rolling
/// up. Reduced motion has no travel (a crossfade).
fn offsets(p: f32, dir: f32, reduced: bool, height: f32) -> (f32, f32) {
    if reduced {
        return (0.0, 0.0);
    }
    let p = p.clamp(0.0, 1.0);
    (-dir * height * p, dir * height * (1.0 - p))
}

/// The widest decimal digit in `font`.
fn digit_width(ui: &Ui, id: Id, font: &FontId, revision: u64) -> f32 {
    (0..10)
        .map(|d| {
            let s = char::from(b'0' + d).to_string();
            cache::galley(ui, (id, "glyph", s.as_str()), revision, || LayoutJob::simple_singleline(s.clone(), font.clone(), Color32::WHITE)).size().x
        })
        .fold(0.0, f32::max)
}

fn is_digit(s: &str) -> bool {
    let mut c = s.chars();
    matches!((c.next(), c.next()), (Some(d), None) if d.is_ascii_digit())
}

struct Drawn {
    same: bool,
    old: Option<Arc<Galley>>,
    new: Option<Arc<Galley>>,
    /// The column's width this frame.
    w: f32,
}

/// What a widget remembers between frames.
#[derive(Clone, Debug)]
struct Roll {
    /// The text being shown (the target of a running roll).
    text: String,
    value: Option<f64>,
    cols: Vec<Col>,
    /// +1 rolls up, -1 down.
    dir: f32,
    /// Where the progress value started this roll, and where it rests when the roll ends.
    base: f32,
    target: f32,
    /// The progress value last frame.
    last: f32,
    rolling: bool,
}

impl Roll {
    fn settled(text: &str, value: Option<f64>) -> Self {
        Roll { text: text.to_owned(), value, cols: diff(text, text), dir: 1.0, base: 0.0, target: 0.0, last: 0.0, rolling: false }
    }
}

/// One column of a roll: the cluster leaving and the cluster arriving (equal when unchanged).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Col {
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Splits `s` into user-perceived characters: a base character with the combining marks,
/// variation selectors, emoji modifiers and tags that follow it, emoji joined by a zero-width
/// joiner, regional-indicator pairs (flags), and CR LF.
pub(crate) fn clusters(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut prev: Option<char> = None;
    let mut ri_run = 0usize;
    for (i, c) in s.char_indices() {
        let joins = match prev {
            None => false,
            Some(p) => extends(c) || p == '\u{200D}' || (p == '\r' && c == '\n') || (is_regional(p) && is_regional(c) && ri_run % 2 == 1),
        };
        if !joins && i > start {
            if let Some(cl) = s.get(start..i) {
                out.push(cl);
            }
            start = i;
        }
        ri_run = if is_regional(c) { ri_run + 1 } else { 0 };
        prev = Some(c);
    }
    if start < s.len()
        && let Some(cl) = s.get(start..)
    {
        out.push(cl);
    }
    out
}

fn extends(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
        | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
}

fn is_regional(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// Aligns `old` with `new` cluster by cluster. Unchanged clusters keep their column; between
/// them, leaving and arriving clusters pair up from the right (so 99 → 100 rolls both nines and
/// slides a 1 in at the left).
pub(crate) fn diff(old: &str, new: &str) -> Vec<Col> {
    let a = clusters(old);
    let b = clusters(new);
    let pre = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let max_suf = a.len().min(b.len()) - pre;
    let suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    let am = a.get(pre..a.len() - suf).unwrap_or_default();
    let bm = b.get(pre..b.len() - suf).unwrap_or_default();

    let keep = |s: &str| Col { old: Some(s.to_owned()), new: Some(s.to_owned()) };
    let mut out: Vec<Col> = Vec::with_capacity(a.len().max(b.len()));
    out.extend(a.iter().take(pre).map(|s| keep(s)));
    let anchors = if am.len() <= LCS_MAX && bm.len() <= LCS_MAX { lcs(am, bm) } else { Vec::new() };
    let (mut i, mut j) = (0, 0);
    for (ai, bj) in anchors.into_iter().chain(std::iter::once((am.len(), bm.len()))) {
        let dels = am.get(i..ai).unwrap_or_default();
        let ins = bm.get(j..bj).unwrap_or_default();
        let pairs = dels.len().min(ins.len());
        let (lone_d, paired_d) = dels.split_at(dels.len() - pairs);
        let (lone_i, paired_i) = ins.split_at(ins.len() - pairs);
        out.extend(lone_d.iter().map(|s| Col { old: Some((*s).to_owned()), new: None }));
        out.extend(lone_i.iter().map(|s| Col { old: None, new: Some((*s).to_owned()) }));
        out.extend(paired_d.iter().zip(paired_i).map(|(d, n)| Col { old: Some((*d).to_owned()), new: Some((*n).to_owned()) }));
        if let Some(s) = am.get(ai) {
            out.push(keep(s));
        }
        i = ai + 1;
        j = bj + 1;
    }
    out.extend(a.iter().skip(a.len() - suf).map(|s| keep(s)));
    out
}

/// Index pairs of a longest common subsequence of `a` and `b`, in order.
fn lcs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    // table[i][j]: LCS length of a[i..] and b[j..]
    let mut table = vec![0u16; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let v = if a.get(i) == b.get(j) {
                table.get(at(i + 1, j + 1)).copied().unwrap_or(0) + 1
            } else {
                table.get(at(i + 1, j)).copied().unwrap_or(0).max(table.get(at(i, j + 1)).copied().unwrap_or(0))
            };
            if let Some(cell) = table.get_mut(at(i, j)) {
                *cell = v;
            }
        }
    }
    let get = |i: usize, j: usize| table.get(at(i, j)).copied().unwrap_or(0);
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a.get(i) == b.get(j) {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if get(i + 1, j) >= get(i, j + 1) {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(old: &str, new: &str) -> Vec<(Option<String>, Option<String>)> {
        diff(old, new).into_iter().map(|c| (c.old, c.new)).collect()
    }

    fn col(old: Option<&str>, new: Option<&str>) -> (Option<String>, Option<String>) {
        (old.map(str::to_owned), new.map(str::to_owned))
    }

    #[test]
    fn clusters_keep_marks_flags_and_joined_emoji_together() {
        assert_eq!(clusters("e\u{301}x"), vec!["e\u{301}", "x"]);
        assert_eq!(clusters("🇬🇧🇫🇷"), vec!["🇬🇧", "🇫🇷"]);
        assert_eq!(clusters("👩\u{200D}💻!"), vec!["👩\u{200D}💻", "!"]);
        assert_eq!(clusters("👍🏽"), vec!["👍🏽"]);
        assert_eq!(clusters("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert!(clusters("").is_empty());
    }

    #[test]
    fn unchanged_text_keeps_every_column() {
        assert!(diff("$1.25", "$1.25").iter().all(|c| c.old == c.new));
    }

    #[test]
    fn only_changed_digits_roll() {
        assert_eq!(
            cols("$1.25", "$1.26"),
            vec![
                col(Some("$"), Some("$")),
                col(Some("1"), Some("1")),
                col(Some("."), Some(".")),
                col(Some("2"), Some("2")),
                col(Some("5"), Some("6")),
            ]
        );
    }

    #[test]
    fn a_new_digit_slides_in_at_the_left() {
        assert_eq!(cols("99", "100"), vec![col(None, Some("1")), col(Some("9"), Some("0")), col(Some("9"), Some("0"))]);
        assert_eq!(cols("100", "99"), vec![col(Some("1"), None), col(Some("0"), Some("9")), col(Some("0"), Some("9"))]);
    }

    #[test]
    fn lcs_keeps_common_clusters_in_the_middle() {
        // the comma stays put
        let c = cols("9,999", "10,000");
        assert!(c.contains(&col(Some(","), Some(","))), "{c:?}");
        assert_eq!(c.iter().filter_map(|c| c.1.clone()).collect::<String>(), "10,000");
        assert_eq!(c.iter().filter_map(|c| c.0.clone()).collect::<String>(), "9,999");
    }

    #[test]
    fn long_middles_skip_lcs_but_keep_both_texts() {
        let a = "a".repeat(40) + &"x".repeat(50);
        let b = "a".repeat(40) + &"y".repeat(60);
        let c = diff(&a, &b);
        assert_eq!(c.iter().filter_map(|c| c.old.clone()).collect::<String>(), a);
        assert_eq!(c.iter().filter_map(|c| c.new.clone()).collect::<String>(), b);
    }

    #[test]
    fn rolls_up_from_below_and_crossfade_has_no_travel() {
        let (o, n) = offsets(0.0, 1.0, false, 10.0);
        assert_eq!((o, n), (0.0, 10.0), "rising: the new glyph starts below");
        let (o, n) = offsets(0.0, -1.0, false, 10.0);
        assert_eq!((o, n), (0.0, -10.0), "falling: the new glyph starts above");
        assert_eq!(offsets(1.0, 1.0, false, 10.0), (-10.0, 0.0));
        for p in [0.0, 0.3, 1.0] {
            assert_eq!(offsets(p, 1.0, true, 10.0), (0.0, 0.0));
        }
    }
}
