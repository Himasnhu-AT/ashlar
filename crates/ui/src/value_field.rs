//! Numeric drag/scrub field with an explicit edit phase.
//!
//! Drag horizontally to scrub (Shift ×10, Alt or ⌘/Ctrl ×0.1), click or press Enter to type a value, use
//! the arrow keys to step. Every change is reported as an [`Edit`] so the app maps one drag or one
//! typed entry to one undo step (see [`crate::edit`]). Escape cancels a drag or a typed entry and
//! restores the value it started from.
//!
//! [`ValueField::hot_text`] draws the value as bare scrubbable text (the "hot text" of inspectors
//! in compositing and editing apps) instead of a boxed field.
//!
//! The text comes from a [`ValueFormat`]: [`Decimal`] for plain numbers, or the app's own (a
//! timecode, a frequency, a percentage) through [`FnFormat`] — the "formatted value field".
//!
//! ```
//! # use ashlar_ui::value_field::{Decimal, ValueField};
//! # use ashlar_ui::edit::EditPhase;
//! # fn ui(ui: &mut egui::Ui, opacity: &mut f64) {
//! let fmt = Decimal { decimals: 0, suffix: " %" };
//! let out = ValueField::new("inspector.opacity", "Opacity", opacity, &fmt).range(0.0..=100.0).show(ui);
//! if let Some(edit) = out.edit {
//!     match edit.phase {
//!         EditPhase::Begin => { /* open a merge group */ }
//!         EditPhase::Update => { /* apply edit.value with the group's merge key */ }
//!         EditPhase::Commit => { /* apply and close the group (or one step if none was open) */ }
//!         EditPhase::Cancel => { /* roll the group back */ }
//!     }
//! }
//! # }
//! ```

use std::fmt::Write as _;
use std::ops::RangeInclusive;

use egui::{Align2, Color32, CursorIcon, Key, Response, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, text::LayoutJob};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::edit::{Bounds, DragMode, Edit, drive_drag, has_keyboard, snap};
use crate::tokens::{self, Tokens};

/// Turns a value into text and back.
pub trait ValueFormat {
    /// Append the text for `value` to `out`.
    fn format(&self, value: f64, out: &mut String);
    /// The value typed as `text`, or `None` when it is not a value.
    fn parse(&self, text: &str) -> Option<f64>;
    /// Changes when the format's own settings change (caches key on it).
    fn tag(&self) -> u64 {
        0
    }
}

/// A plain number with a fixed number of decimals and an optional suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    pub decimals: usize,
    pub suffix: &'static str,
}

impl Default for Decimal {
    fn default() -> Self {
        Decimal { decimals: 2, suffix: "" }
    }
}

impl ValueFormat for Decimal {
    fn format(&self, value: f64, out: &mut String) {
        let decimals = self.decimals.min(12);
        let _ = write!(out, "{value:.decimals$}{}", self.suffix);
    }
    fn parse(&self, text: &str) -> Option<f64> {
        parse_number(text.trim().trim_end_matches(self.suffix.trim()))
    }
    fn tag(&self) -> u64 {
        cache::hash_of(self)
    }
}

/// Parse a typed number leniently: surrounding spaces, a decimal comma, a leading `+`.
pub fn parse_number(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() || t.len() > 64 {
        return None;
    }
    let normalized: String = t.chars().map(|c| if c == ',' { '.' } else { c }).filter(|c| !c.is_whitespace()).collect();
    normalized.trim_start_matches('+').parse::<f64>().ok().filter(|v| v.is_finite())
}

/// An app-supplied format from two closures (format, parse) and a tag.
pub struct FnFormat<F, P> {
    pub format: F,
    pub parse: P,
    pub tag: u64,
}

impl<F: Fn(f64, &mut String), P: Fn(&str) -> Option<f64>> ValueFormat for FnFormat<F, P> {
    fn format(&self, value: f64, out: &mut String) {
        (self.format)(value, out);
    }
    fn parse(&self, text: &str) -> Option<f64> {
        (self.parse)(text)
    }
    fn tag(&self) -> u64 {
        self.tag
    }
}

/// What a value field reports.
pub struct ValueResponse {
    pub response: Response,
    pub edit: Option<Edit>,
}

/// The field. See the [module docs](self).
pub struct ValueField<'a> {
    id: &'a str,
    label: &'a str,
    value: &'a mut f64,
    format: &'a dyn ValueFormat,
    range: RangeInclusive<f64>,
    speed: Option<f64>,
    step: f64,
    key_step: Option<f64>,
    width: Option<f32>,
    hot: Option<Color32>,
}

impl<'a> ValueField<'a> {
    /// `id` is the automation id (also scopes egui state); `label` is the accessible name.
    pub fn new(id: &'a str, label: &'a str, value: &'a mut f64, format: &'a dyn ValueFormat) -> Self {
        ValueField { id, label, value, format, range: f64::MIN..=f64::MAX, speed: None, step: 0.0, key_step: None, width: None, hot: None }
    }
    pub fn range(mut self, range: RangeInclusive<f64>) -> Self {
        self.range = range;
        self
    }
    /// Value change per point of horizontal drag (default: 1/300 of a finite range, else 0.1).
    pub fn speed(mut self, speed: f64) -> Self {
        self.speed = Some(speed);
        self
    }
    /// Snap values to multiples of `step` (0 = no snapping).
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }
    /// Arrow-key step (default: `step`, else 1/100 of a finite range, else 1).
    pub fn key_step(mut self, step: f64) -> Self {
        self.key_step = Some(step);
        self
    }
    /// Draw the value as bare text in `color` (no box), sized to the text, brighter and
    /// underlined while hovered or dragged. The editor a click opens is `width` wide (default
    /// three control heights).
    pub fn hot_text(mut self, color: Color32) -> Self {
        self.hot = Some(color);
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn show(self, ui: &mut Ui) -> ValueResponse {
        let t = tokens::current(ui.ctx());
        let bounds = Bounds::new(&self.range);
        let span = bounds.max - bounds.min;
        let speed = valid_positive(self.speed).unwrap_or(if bounds.finite() && span > 0.0 { span / 300.0 } else { 0.1 });
        let key_step = valid_positive(self.key_step).or(valid_positive(Some(self.step))).unwrap_or(if bounds.finite() && span > 0.0 {
            span / 100.0
        } else {
            1.0
        });
        let id = ui.make_persistent_id(self.id);
        let editing_id = id.with("editing");
        let width = self.width.filter(|w| w.is_finite() && *w > 0.0).unwrap_or(t.control_height * 3.0).min(4096.0);
        let editing = ui.data(|d| d.get_temp::<String>(editing_id));
        let font = Tokens::font(t.type_scale.body);
        // hot text is laid out first: its size is the text's
        let hot = match (self.hot, &editing) {
            (Some(color), None) => {
                let shown = *self.value;
                let revision = cache::hash_of(&(shown.to_bits(), self.format.tag(), font.size.to_bits()));
                let format = self.format;
                let galley = cache::galley(ui, (id, "hot"), revision, || {
                    let mut text = String::new();
                    format.format(shown, &mut text);
                    LayoutJob::simple_singleline(text, font.clone(), color)
                });
                Some((color, galley))
            }
            _ => None,
        };
        let size = hot.as_ref().map_or(Vec2::new(width, t.control_height), |(_, g)| g.size() + Vec2::splat(4.0));
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());

        if let Some(buffer) = editing {
            return self.show_editor(ui, rect, id, editing_id, buffer, bounds, &t);
        }

        let response = ui.interact(rect, id, Sense::click_and_drag());
        let value = self.value;
        let enabled = response.enabled();
        let mut edit = drive_drag(ui, &response, value, bounds, self.step, DragMode::Relative(|d: Vec2, mult| d.x as f64 * speed * mult));

        if edit.is_none() && has_keyboard(&response) && enabled {
            let (up, down, enter, shift) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::Enter),
                    i.modifiers.shift,
                )
            });
            let dir = (up as i8 - down as i8) as f64;
            if dir != 0.0 {
                let start = bounds.clamp(*value);
                let next = bounds.clamp(snap(start + dir * key_step * if shift { 10.0 } else { 1.0 }, self.step));
                if next != start {
                    *value = next;
                    edit = Some(Edit::commit(start, next));
                }
            } else if enter {
                edit = Some(open_editor(ui, editing_id, self.format, *value));
            }
        }
        if edit.is_none() && response.clicked() && enabled {
            edit = Some(open_editor(ui, editing_id, self.format, *value));
        }

        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        let lit = response.hovered() || response.dragged();
        if let Some((color, galley)) = hot {
            let color = if !enabled {
                t.text.disabled
            } else if lit {
                t.accent_hover
            } else {
                color
            };
            let painter = ui.painter();
            if lit && enabled {
                let y = rect.max.y - 1.0;
                painter.line_segment([egui::pos2(rect.min.x + 2.0, y), egui::pos2(rect.max.x - 2.0, y)], Stroke::new(1.0, color.gamma_multiply(0.6)));
            }
            let text = galley.text().to_owned();
            painter.galley_with_override_text_color(rect.min + Vec2::splat(2.0), galley, color);
            let shown = *value;
            response.widget_info(|| WidgetInfo::slider(enabled, shown, self.label));
            automation::register_with(ui, &response, self.id, self.label, Role::Field, Details { value: Some(&text), selected: None });
            return ValueResponse { response, edit };
        }
        let text_color = if !enabled {
            t.text.disabled
        } else if lit {
            t.accent_hover
        } else {
            t.text.primary
        };
        let painter = ui.painter_at(rect.expand(2.0));
        let radius = t.radius.md;
        painter.rect_filled(rect, radius, t.surface.sunken);
        let border = if response.has_focus() {
            Stroke::new(1.5, t.focus)
        } else if lit {
            Stroke::new(1.0, t.border_strong)
        } else {
            Stroke::new(1.0, t.border)
        };
        painter.rect_stroke(rect, radius, border, StrokeKind::Inside);
        let shown = *value;
        let format = self.format;
        let revision = cache::hash_of(&(shown.to_bits(), format.tag(), text_color, font.size.to_bits()));
        let galley = cache::galley(ui, (id, "value"), revision, || {
            let mut text = String::new();
            format.format(shown, &mut text);
            LayoutJob::simple_singleline(text, font, text_color)
        });
        let pos = Align2::CENTER_CENTER.align_size_within_rect(galley.size(), rect.shrink2(Vec2::new(t.space.sm, 0.0))).min;
        painter.galley(pos, galley.clone(), text_color);

        response.widget_info(|| WidgetInfo::slider(enabled, shown, self.label));
        automation::register_with(ui, &response, self.id, self.label, Role::Field, Details { value: Some(galley.text()), selected: None });
        ValueResponse { response, edit }
    }

    fn show_editor(
        self,
        ui: &mut Ui,
        rect: egui::Rect,
        id: egui::Id,
        editing_id: egui::Id,
        mut buffer: String,
        bounds: Bounds,
        t: &Tokens,
    ) -> ValueResponse {
        let start = bounds.clamp(*self.value);
        let text_id = id.with("text");
        let focused_id = focused_flag(editing_id);
        // The frame the editor opens it takes the keyboard before it is drawn, so keys of this very
        // frame (a value typed straight after the click) reach it; its text is selected, so typing
        // replaces the value.
        let opening = !ui.data(|d| d.get_temp::<bool>(focused_id).unwrap_or(false));
        if opening {
            select_all(ui.ctx(), text_id, &buffer);
            ui.memory_mut(|m| m.request_focus(text_id));
            ui.data_mut(|d| d.insert_temp(focused_id, true));
        }
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("ashlar_value_editor"));
        let response = child.add(
            egui::TextEdit::singleline(&mut buffer)
                .id(text_id)
                .desired_width(rect.width())
                .font(Tokens::font(t.type_scale.body))
                .margin(egui::Margin::symmetric(t.space.sm as i8, 3)),
        );
        let mut edit = None;
        // the keyboard went elsewhere (Enter, Escape, a click outside), or another widget took it
        // the frame the editor opened
        let lost = response.lost_focus();
        if lost || !has_keyboard(&response) {
            ui.data_mut(|d| {
                d.remove::<String>(editing_id);
                d.remove::<bool>(focused_id);
            });
            let escaped = ui.input(|i| i.key_pressed(Key::Escape));
            edit = Some(match self.format.parse(&buffer) {
                Some(v) if lost && !escaped => {
                    let v = bounds.clamp(snap(v, self.step));
                    *self.value = v;
                    Edit::commit(start, v)
                }
                _ => Edit::cancel(start),
            });
        } else {
            ui.data_mut(|d| d.insert_temp(editing_id, buffer));
        }
        automation::register(ui, &response, self.id, self.label, Role::Field);
        ValueResponse { response, edit }
    }
}

fn valid_positive(v: Option<f64>) -> Option<f64> {
    v.filter(|s| s.is_finite() && *s > 0.0)
}

/// Set once an open editor has been given the keyboard.
fn focused_flag(editing_id: egui::Id) -> egui::Id {
    editing_id.with("focused")
}

fn open_editor(ui: &Ui, editing_id: egui::Id, format: &dyn ValueFormat, value: f64) -> Edit {
    let mut text = String::new();
    format.format(value, &mut text);
    ui.data_mut(|d| {
        d.insert_temp(editing_id, text);
        d.remove::<bool>(focused_flag(editing_id));
    });
    crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
    Edit::begin(value)
}

/// Select all of a text field's text so typing replaces it.
pub(crate) fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let all = egui::text::CCursorRange::two(egui::text::CCursor::default(), egui::text::CCursor::new(text.chars().count()));
    state.cursor.set_char_range(Some(all));
    state.store(ctx, id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::EditPhase;
    use egui::Pos2;
    use egui_kittest::Harness;

    struct State {
        value: f64,
        edits: Vec<Edit>,
        rect: egui::Rect,
    }

    fn harness(value: f64, range: RangeInclusive<f64>) -> Harness<'static, State> {
        Harness::builder().with_size(Vec2::new(300.0, 80.0)).build_ui_state(
            move |ui, s: &mut State| {
                let fmt = Decimal { decimals: 1, suffix: " px" };
                let out = ValueField::new("test.value", "Width", &mut s.value, &fmt).range(range.clone()).speed(1.0).step(0.5).width(120.0).show(ui);
                s.rect = out.response.rect;
                s.edits.extend(out.edit);
            },
            State { value, edits: Vec::new(), rect: egui::Rect::NOTHING },
        )
    }

    fn phases(s: &State) -> Vec<EditPhase> {
        s.edits.iter().map(|e| e.phase).collect()
    }

    fn drag(h: &mut Harness<'static, State>, from: Pos2, to: &[Pos2]) {
        h.hover_at(from);
        h.step();
        h.drag_at(from);
        h.step();
        for p in to {
            h.hover_at(*p);
            h.step();
        }
    }

    fn hot_harness(value: f64) -> Harness<'static, State> {
        Harness::builder().with_size(Vec2::new(300.0, 80.0)).build_ui_state(
            move |ui, s: &mut State| {
                let fmt = Decimal { decimals: 1, suffix: " px" };
                let out = ValueField::new("test.hot", "Width", &mut s.value, &fmt)
                    .range(0.0..=100.0)
                    .speed(1.0)
                    .hot_text(Color32::LIGHT_BLUE)
                    .width(64.0)
                    .show(ui);
                s.rect = out.response.rect;
                s.edits.extend(out.edit);
            },
            State { value, edits: Vec::new(), rect: egui::Rect::NOTHING },
        )
    }

    #[test]
    fn hot_text_is_sized_to_its_text_scrubs_and_types() {
        let mut h = hot_harness(10.0);
        h.step();
        let r = h.state().rect;
        assert!(r.width() < 60.0 && r.height() < Tokens::dark().control_height, "{r:?}");
        let c = r.center();
        // ⌘/Ctrl is fine (×0.1), like Alt
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
        drag(&mut h, c, &[c + egui::vec2(10.0, 0.0), c + egui::vec2(20.0, 0.0)]);
        h.drop_at(c + egui::vec2(20.0, 0.0));
        h.step();
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        let v = h.state().value;
        assert!(v > 10.0 && v <= 12.5, "{v}");
        assert_eq!(h.state().edits.last().map(|e| e.phase), Some(EditPhase::Commit));
        // a click opens the editor (the field's width) and Enter commits the typed value
        let c = h.state().rect.center();
        crate::test_support::click(&mut h, c);
        h.run_steps(2);
        assert_eq!(h.state().rect.width(), 64.0);
        h.event(egui::Event::Text("42".into()));
        h.step();
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().value, 42.0);
        assert!(h.state().rect.width() < 60.0, "back to hot text");
    }

    #[test]
    fn a_drag_is_one_begin_some_updates_and_one_commit() {
        let mut h = harness(10.0, 0.0..=100.0);
        h.step();
        let c = h.state().rect.center();
        drag(&mut h, c, &[c + egui::vec2(10.0, 0.0), c + egui::vec2(20.0, 0.0), c + egui::vec2(25.0, 0.0)]);
        h.drop_at(c + egui::vec2(25.0, 0.0));
        h.step();
        let p = phases(h.state());
        assert_eq!(p.first(), Some(&EditPhase::Begin), "{p:?}");
        assert_eq!(p.last(), Some(&EditPhase::Commit), "{p:?}");
        assert!(p[1..p.len() - 1].iter().all(|x| *x == EditPhase::Update), "{p:?}");
        let commit = h.state().edits.last().copied().unwrap();
        assert_eq!(commit.start, 10.0);
        assert!(commit.value > 20.0 && commit.value <= 35.0, "{commit:?}");
        assert_eq!(commit.value % 0.5, 0.0, "snapped to the step");
        assert_eq!(h.state().value, commit.value);
    }

    #[test]
    fn escape_cancels_a_drag_and_restores_the_start() {
        let mut h = harness(10.0, 0.0..=100.0);
        h.step();
        let c = h.state().rect.center();
        drag(&mut h, c, &[c + egui::vec2(30.0, 0.0), c + egui::vec2(40.0, 0.0)]);
        assert!(h.state().value > 10.0);
        h.key_press(Key::Escape);
        h.step();
        assert_eq!(h.state().value, 10.0);
        assert_eq!(h.state().edits.last().map(|e| e.phase), Some(EditPhase::Cancel));
        h.hover_at(c + egui::vec2(60.0, 0.0));
        h.step();
        h.drop_at(c + egui::vec2(60.0, 0.0));
        h.step();
        assert_eq!(h.state().value, 10.0, "the rest of a cancelled drag is ignored");
        assert_eq!(h.state().edits.last().map(|e| e.phase), Some(EditPhase::Cancel));
    }

    #[test]
    fn click_types_a_value_and_enter_commits_it_clamped() {
        let mut h = harness(10.0, 0.0..=100.0);
        h.step();
        let c = h.state().rect.center();
        h.hover_at(c);
        h.step();
        h.drag_at(c);
        h.step();
        h.drop_at(c);
        h.run_steps(3);
        assert_eq!(phases(h.state()), [EditPhase::Begin]);
        h.key_combination(&[Key::A]); // replaced by the select-all on open
        h.event(egui::Event::Text("250,5".into()));
        h.step();
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(phases(h.state()), [EditPhase::Begin, EditPhase::Commit]);
        assert_eq!(h.state().value, 100.0);
    }

    /// A window driven from outside without being activated (an agent, a script): a value typed
    /// in the frame straight after the click lands, and each entry adds to the text instead of
    /// replacing it.
    #[test]
    fn typing_straight_after_the_click_lands_and_adds_up_in_a_background_window() {
        let mut h = harness(10.0, 0.0..=1000.0);
        h.input_mut().focused = false;
        h.step();
        let c = h.state().rect.center();
        h.hover_at(c);
        h.step();
        h.drag_at(c);
        h.step();
        h.drop_at(c);
        h.step();
        assert_eq!(phases(h.state()), [EditPhase::Begin]);
        for typed in ["1", "2", "0"] {
            h.event(egui::Event::Text(typed.into()));
            h.step();
        }
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(phases(h.state()), [EditPhase::Begin, EditPhase::Commit]);
        assert_eq!(h.state().value, 120.0);
    }

    #[test]
    fn escape_or_garbage_cancels_a_typed_entry() {
        for (typed, key) in [("42", Key::Escape), ("forty-two", Key::Enter)] {
            let mut h = harness(10.0, 0.0..=100.0);
            h.step();
            let c = h.state().rect.center();
            h.hover_at(c);
            h.step();
            h.drag_at(c);
            h.step();
            h.drop_at(c);
            h.run_steps(3);
            h.event(egui::Event::Text(typed.into()));
            h.step();
            h.key_press(key);
            h.run_steps(2);
            assert_eq!(phases(h.state()), [EditPhase::Begin, EditPhase::Cancel], "{typed}");
            assert_eq!(h.state().value, 10.0);
        }
    }

    #[test]
    fn arrow_keys_step_as_one_shot_commits_and_are_registered() {
        let mut h = harness(10.0, 0.0..=100.0);
        h.step();
        // Tab focuses the field
        h.key_press(Key::Tab);
        h.step();
        h.key_press(Key::ArrowUp);
        h.step();
        assert_eq!(phases(h.state()), [EditPhase::Commit]);
        assert_eq!(h.state().edits[0].start, 10.0);
        assert_eq!(h.state().value, 10.5, "key step defaults to the snap step");
        h.key_press_modifiers(egui::Modifiers::SHIFT, Key::ArrowDown);
        h.step();
        assert_eq!(h.state().value, 5.5);
        let e = crate::automation::find(&h.ctx, "test.value").unwrap();
        assert_eq!(e.value, "5.5 px");
        assert_eq!(e.role, Role::Field);
    }

    #[test]
    fn hostile_values_and_ranges_do_not_panic() {
        for (v, r) in [(f64::NAN, 0.0..=1.0), (5.0, 10.0..=-10.0), (f64::INFINITY, f64::NEG_INFINITY..=f64::INFINITY)] {
            let mut h = harness(v, r);
            h.step();
            let c = h.state().rect.center();
            drag(&mut h, c, &[c + egui::vec2(1e6, 0.0)]);
            h.drop_at(c);
            h.step();
            assert!(h.state().edits.iter().all(|e| e.value.is_finite() && e.start.is_finite()));
        }
    }

    #[test]
    fn formatted_field_uses_the_apps_format_and_parse() {
        // seconds shown as m:ss
        let fmt = FnFormat {
            format: |v: f64, out: &mut String| {
                let s = v.max(0.0).round() as u64;
                let _ = write!(out, "{}:{:02}", s / 60, s % 60);
            },
            parse: |t: &str| {
                let (m, s) = t.split_once(':')?;
                Some(m.trim().parse::<f64>().ok()? * 60.0 + s.trim().parse::<f64>().ok()?)
            },
            tag: 1,
        };
        let mut buf = String::new();
        fmt.format(125.0, &mut buf);
        assert_eq!(buf, "2:05");
        assert_eq!(fmt.parse("1:30"), Some(90.0));
        assert_eq!(fmt.parse("90"), None);
        assert_eq!(parse_number(" +1,5 "), Some(1.5));
        assert_eq!(parse_number("inf"), None);
        assert_eq!(Decimal { decimals: 0, suffix: " %" }.parse("40 %"), Some(40.0));
    }
}
