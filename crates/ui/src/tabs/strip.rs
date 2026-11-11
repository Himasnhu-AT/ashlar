//! A tab strip: [`TabStrip`] for apps, and the engine it shares with the docking surface.
//!
//! The strip fits tabs with [`fit`], senses each with [`Tab`], and adds what a strip needs on
//! top: one Tab stop with arrow keys (the selected tab; arrows, Home and End move the selection),
//! ⌘W, middle-click and a close button to close, an overflow menu for tabs that do not fit,
//! drag (or ⌘⇧ + arrow) reordering, the animated selection [`indicator`], an AccessKit tab list
//! and dotted automation ids. It takes the selection by reference and reports everything else.

use egui::accesskit::Role as AccessRole;
use egui::{
    Align2, Color32, FocusDirection, FontId, Id, Key, Modifiers, Rect, Response, ScrollArea, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetInfo,
    WidgetType, pos2, vec2,
};

use super::{Tab, fit, indicator};
use crate::automation::{self, Details, Role};
use crate::layout::{EXTENT, usable};
use crate::list;

/// Tabs beyond this many are ignored (not drawn, not reachable).
pub const MAX_TABS: usize = 4096;

/// Height of the overflow menu before it scrolls.
const MENU_HEIGHT: f32 = 360.0;

/// How a [`TabStrip`] looks. Dimensions are sanitized when shown (non-finite → the default,
/// clamped to sane ranges), so any value is safe.
#[derive(Clone, Debug, PartialEq)]
pub struct TabStripStyle {
    /// Strip height.
    pub height: f32,
    /// Horizontal padding on each side of a label.
    pub padding: f32,
    /// Narrowest a tab is squeezed to before tabs overflow.
    pub min_width: f32,
    /// Width of the overflow button.
    pub overflow_width: f32,
    /// Side of the close button's square.
    pub close_size: f32,
    pub font: FontId,
    pub background: Color32,
    pub selected_background: Color32,
    pub hover_background: Color32,
    pub text: Color32,
    pub inactive_text: Color32,
    /// Selection indicator colour; transparent turns it off.
    pub indicator: Color32,
    pub indicator_height: f32,
    /// Keyboard focus outline of a tab.
    pub focus: Stroke,
    /// Line under the strip.
    pub border: Stroke,
    /// Accessible name of the overflow button.
    pub overflow_label: String,
    /// Accessible name of a tab's close button.
    pub close_label: String,
}

impl TabStripStyle {
    /// The style from the current theme tokens.
    pub fn from_ui(ui: &Ui) -> Self {
        let t = crate::tokens::current(ui.ctx());
        let labels = crate::labels::current(ui.ctx());
        Self {
            height: t.control_height + t.space.sm,
            padding: t.space.lg,
            min_width: 48.0,
            overflow_width: 24.0,
            close_size: 16.0,
            font: FontId::proportional(t.type_scale.button),
            background: t.surface.panel,
            selected_background: t.surface.raised,
            hover_background: t.surface.hover,
            text: t.text.primary,
            inactive_text: t.text.secondary,
            indicator: t.accent,
            indicator_height: 2.0,
            focus: Stroke::new(1.5, t.focus),
            border: Stroke::new(1.0, t.border),
            overflow_label: labels.tabs.overflow.clone(),
            close_label: labels.tabs.close.clone(),
        }
    }

    fn sanitized(mut self) -> Self {
        let dim = |v: f32, lo: f32, hi: f32, default: f32| if v.is_finite() { v.clamp(lo, hi) } else { default };
        self.height = dim(self.height, 1.0, 1024.0, 28.0);
        self.padding = dim(self.padding, 0.0, 256.0, 12.0);
        self.min_width = dim(self.min_width, 0.0, EXTENT, 48.0);
        self.overflow_width = dim(self.overflow_width, 0.0, 1024.0, 24.0);
        self.close_size = dim(self.close_size, 0.0, 256.0, 16.0);
        self.indicator_height = dim(self.indicator_height, 0.0, 64.0, 2.0);
        self.focus.width = dim(self.focus.width, 0.0, 16.0, 1.0);
        self.border.width = dim(self.border.width, 0.0, 16.0, 1.0);
        let size = self.font.size;
        self.font.size = dim(size, 1.0, 256.0, 13.0);
        self
    }
}

/// What a [`TabStrip`] reports for one frame. It never edits your tabs: apply `closed` and
/// `moved` yourself ([`reorder`] applies a move and keeps the selection on the same tab).
#[derive(Clone, Debug)]
pub struct TabStripResponse {
    /// The strip's area.
    pub response: Response,
    /// A tab was activated (click, Enter, arrows, the overflow menu); `*selected` already holds it.
    pub selected: Option<usize>,
    /// A tab asked to close (close button, middle-click, ⌘W while it has focus).
    pub closed: Option<usize>,
    /// A tab was dragged (or moved with ⌘⇧ + arrow) from the first index to the second, in
    /// `Vec::remove` then `Vec::insert` terms.
    pub moved: Option<(usize, usize)>,
    /// The overflow menu is open.
    pub overflow_open: bool,
}

/// Apply a [`TabStripResponse::moved`] to `items`, keeping `selected` on the same tab. Out of
/// range moves are ignored.
pub fn reorder<T>(items: &mut Vec<T>, selected: &mut usize, (from, to): (usize, usize)) {
    if from >= items.len() || to >= items.len() || from == to {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
    let s = *selected;
    *selected = if s == from {
        to
    } else if from < s && s <= to {
        s - 1
    } else if to <= s && s < from {
        s + 1
    } else {
        s
    };
}

/// A row of tabs with one Tab stop, close, overflow and reorder.
///
/// ```no_run
/// # fn f(ui: &mut egui::Ui, tabs: &mut Vec<String>, selected: &mut usize) {
/// use ashlar_ui::tabs::{TabStrip, reorder};
/// let out = TabStrip::new("editor.tabs", tabs, selected).closable(true).reorderable(true).show(ui);
/// if let Some(m) = out.moved { reorder(tabs, selected, m); }
/// if let Some(i) = out.closed { tabs.remove(i); }
/// # }
/// ```
///
/// Automation: the strip is `<id>`, each tab `<id>.tab.<n>`, its close button
/// `<id>.tab.<n>.close`, the overflow button `<id>.overflow` and its rows `<id>.overflow.<n>`
/// (`n` is the tab's index). Ids must be stable: an empty id gets an automatic egui id and no
/// automation.
pub struct TabStrip<'a, S> {
    id: &'a str,
    tabs: &'a [S],
    selected: &'a mut usize,
    closable: bool,
    reorderable: bool,
    label: &'a str,
    style: Option<TabStripStyle>,
}

impl<'a, S: AsRef<str>> TabStrip<'a, S> {
    /// Tabs labelled by `tabs`; `selected` is clamped into range.
    pub fn new(id: &'a str, tabs: &'a [S], selected: &'a mut usize) -> Self {
        Self { id, tabs, selected, closable: false, reorderable: false, label: "", style: None }
    }

    /// Show close buttons and accept middle-click and ⌘W (default: off).
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// Accept drags and ⌘⇧ + arrow to reorder (default: off).
    pub fn reorderable(mut self, reorderable: bool) -> Self {
        self.reorderable = reorderable;
        self
    }

    /// The tab list's accessible name.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// Override the theme's style.
    pub fn style(mut self, style: TabStripStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// Lay out a strip across the available width.
    pub fn show(self, ui: &mut Ui) -> TabStripResponse {
        let style = self.style.clone().unwrap_or_else(|| TabStripStyle::from_ui(ui)).sanitized();
        let width = ui.available_width();
        let width = if width.is_finite() { width.clamp(0.0, EXTENT) } else { 0.0 };
        let (auto_id, rect) = ui.allocate_space(vec2(width, style.height));
        let id = if self.id.is_empty() { auto_id } else { ui.make_persistent_id(("ashlar_ui::tabs::strip", self.id)) };
        let count = self.tabs.len().min(MAX_TABS);
        if count > 0 && *self.selected >= count {
            *self.selected = count - 1;
        }

        if ui.is_rect_visible(rect) {
            ui.painter().rect_filled(rect, 0.0, style.background);
        }
        let close_room = if self.closable { style.close_size + style.padding / 2.0 } else { 0.0 };
        let galleys: Vec<_> = self
            .tabs
            .iter()
            .take(count)
            .enumerate()
            .map(|(i, s)| {
                let color = if i == *self.selected { style.text } else { style.inactive_text };
                ui.painter().layout_no_wrap(s.as_ref().to_owned(), style.font.clone(), color)
            })
            .collect();
        let tabs: Vec<StripTab<'_>> = self
            .tabs
            .iter()
            .zip(&galleys)
            .enumerate()
            .map(|(i, (s, g))| StripTab {
                id: id.with(("tab", i)),
                label: s.as_ref(),
                width: g.size().x + 2.0 * style.padding + close_room,
                sense: Sense::click(),
                closable: self.closable,
            })
            .collect();
        let engine = Strip {
            id,
            automation: (!self.id.is_empty()).then_some(self.id),
            label: self.label,
            style: &style,
            rect,
            selected: *self.selected,
            reorderable: self.reorderable,
            overflow_id: id.with("overflow"),
        };
        let enabled = ui.is_enabled();
        let out = engine.show(
            ui,
            &tabs,
            &mut |ui, i, r| {
                let Some(galley) = galleys.get(i) else { return };
                let selected = i == engine.selected;
                let fill = if selected {
                    style.selected_background
                } else if enabled && r.hovered() {
                    style.hover_background
                } else {
                    Color32::TRANSPARENT
                };
                if fill.a() > 0 {
                    ui.painter().rect_filled(r.rect, 0.0, fill);
                }
                let text = Rect::from_min_max(r.rect.min, pos2((r.rect.right() - close_room).max(r.rect.left()), r.rect.bottom()));
                let pos = pos2(text.left() + style.padding, text.center().y - galley.size().y / 2.0);
                ui.painter().with_clip_rect(text.intersect(ui.clip_rect())).galley(pos, galley.clone(), style.text);
            },
            &mut |ui, i| ui.button(tabs.get(i).map_or("", |t| t.label)),
        );
        if ui.is_rect_visible(rect) && style.border.width > 0.0 {
            ui.painter().hline(rect.x_range(), rect.bottom() - style.border.width / 2.0, style.border);
        }
        if let Some(i) = out.selected {
            *self.selected = i;
        }
        TabStripResponse { response: out.list, selected: out.selected, closed: out.closed, moved: out.moved, overflow_open: out.overflow_open }
    }
}

/// One tab as the engine sees it.
pub(crate) struct StripTab<'a> {
    /// Stable egui id (focus follows it).
    pub id: Id,
    /// Accessible name.
    pub label: &'a str,
    /// Natural width.
    pub width: f32,
    /// What the tab senses when the strip is not reorderable.
    pub sense: Sense,
    pub closable: bool,
}

/// The shared engine: fits, senses, focuses and reports; the caller paints each tab and each
/// overflow menu row.
pub(crate) struct Strip<'a> {
    pub id: Id,
    /// Dotted automation id of the strip; `None` registers nothing.
    pub automation: Option<&'a str>,
    pub label: &'a str,
    pub style: &'a TabStripStyle,
    pub rect: Rect,
    pub selected: usize,
    pub reorderable: bool,
    pub overflow_id: Id,
}

/// What the engine reports.
pub(crate) struct StripOut {
    pub list: Response,
    /// Each tab's rect this frame (`Rect::NOTHING` when it is in the overflow menu).
    pub rects: Vec<Rect>,
    /// Each shown tab's response.
    pub responses: Vec<Option<Response>>,
    /// Activated this frame (also when it was already selected).
    pub selected: Option<usize>,
    pub closed: Option<usize>,
    pub moved: Option<(usize, usize)>,
    pub overflow_open: bool,
}

/// Where a tab dragged from `from` lands when dropped at `x`, given the shown tabs in order.
fn drop_index(shown: &[(usize, Rect)], from: usize, x: f32) -> (usize, f32) {
    let others = || shown.iter().filter(|(j, _)| *j != from);
    if let Some(&(j, r)) = others().find(|(_, r)| x < r.center().x) {
        return (if j > from { j - 1 } else { j }, r.left());
    }
    match others().next_back() {
        Some(&(j, r)) => (if j > from { j } else { j + 1 }, r.right()),
        None => (from, x),
    }
}

impl Strip<'_> {
    pub(crate) fn show(
        &self,
        ui: &mut Ui,
        tabs: &[StripTab<'_>],
        paint: &mut dyn FnMut(&Ui, usize, &Response),
        menu_item: &mut dyn FnMut(&mut Ui, usize) -> Response,
    ) -> StripOut {
        let tabs = tabs.get(..MAX_TABS).unwrap_or(tabs);
        let count = tabs.len();
        let s = self.style;
        let rect = if usable(self.rect) {
            self.rect
        } else {
            Rect::from_min_size(self.rect.min.clamp(pos2(-EXTENT, -EXTENT), pos2(EXTENT, EXTENT)), Vec2::ZERO)
        };
        let list = ui.interact(rect, self.id, Sense::hover());
        ui.ctx().accesskit_node_builder(list.id, |node| {
            node.set_role(AccessRole::TabList);
            if !self.label.is_empty() {
                node.set_label(self.label);
            }
        });
        if let Some(a) = self.automation {
            automation::register_with(ui, &list, a, self.label, Role::Group, Details::default());
        }
        let mut out = StripOut {
            list,
            rects: vec![Rect::NOTHING; count],
            responses: vec![None; count],
            selected: None,
            closed: None,
            moved: None,
            overflow_open: false,
        };
        if count == 0 || !usable(self.rect) {
            return out;
        }
        let selected = self.selected.min(count - 1);
        let widths: Vec<f32> = tabs.iter().map(|t| t.width).collect();
        let fitted = fit(&widths, selected, rect.width(), s.min_width, s.overflow_width);

        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).accessibility_parent(out.list.id));
        let focused = ui.memory(|m| m.focused());
        let mut shown: Vec<(usize, Rect)> = Vec::with_capacity(fitted.shown.len());
        let mut x = rect.left();
        for &(i, w) in &fitted.shown {
            let Some(tab) = tabs.get(i) else { continue };
            let r = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
            x += w;
            let mut sense = if self.reorderable { Sense::click_and_drag() } else { tab.sense };
            // one Tab stop: only the selected tab (or one focused some other way) is focusable
            if i != selected && focused != Some(tab.id) {
                sense = sense.difference(Sense::FOCUSABLE);
            }
            let response =
                Tab::new(tab.id, tab.label, i == selected).sense(sense).focus_stroke(s.focus).show_at(&mut child, r, |ui, resp| paint(ui, i, resp));
            ui.ctx().accesskit_node_builder(response.id, |node| {
                node.set_position_in_set(i + 1);
                node.set_size_of_set(count);
            });
            if let Some(a) = self.automation {
                let details = Details { value: None, selected: Some(i == selected) };
                automation::register_fmt(ui, r, format_args!("{a}.tab.{i}"), tab.label, Role::Tab, response.enabled(), details);
            }
            if response.clicked() {
                out.selected = Some(i);
            }
            if tab.closable {
                if response.middle_clicked() {
                    out.closed = Some(i);
                }
                if self.close_button(&mut child, tab, i, r, i == selected || response.hovered()) {
                    out.closed = Some(i);
                }
            }
            if let Some(slot) = out.rects.get_mut(i) {
                *slot = r;
            }
            if let Some(slot) = out.responses.get_mut(i) {
                *slot = Some(response);
            }
            shown.push((i, r));
        }

        self.keys(ui, tabs, selected, &mut out);
        if self.reorderable {
            self.drag(ui, &shown, &mut out);
        }

        if !fitted.overflow.is_empty() && fitted.overflow_width > 0.0 {
            let button = Rect::from_min_size(pos2(x, rect.top()), vec2(fitted.overflow_width, rect.height()));
            self.overflow(ui, tabs, button, &fitted.overflow, menu_item, &mut out);
        }

        if s.indicator.a() > 0
            && s.indicator_height > 0.0
            && let Some(&sel) = out.rects.get(selected)
            && usable(sel)
        {
            // animate relative to the strip so moving or resizing the strip does not slide it
            let h = s.indicator_height.min(rect.height());
            let target = Rect::from_min_max(pos2(sel.left(), sel.bottom() - h), sel.max).translate(-rect.min.to_vec2());
            let at = indicator(ui.ctx(), self.id, target).translate(rect.min.to_vec2());
            ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).rect_filled(at, 0.0, s.indicator);
        }
        out
    }

    /// A tab's close button; true when clicked. It is not a Tab stop (⌘W closes from the keyboard).
    fn close_button(&self, ui: &mut Ui, tab: &StripTab<'_>, i: usize, r: Rect, visible: bool) -> bool {
        let s = self.style;
        let side = s.close_size.min(r.height()).min(r.width() / 2.0);
        if side < 6.0 {
            return false;
        }
        let at = Rect::from_center_size(pos2(r.right() - s.padding / 2.0 - side / 2.0, r.center().y), Vec2::splat(side));
        let response = ui.interact(at, tab.id.with("ashlar_ui::tab_close"), Sense::CLICK);
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &s.close_label));
        if let Some(a) = self.automation {
            automation::register_fmt(ui, at, format_args!("{a}.tab.{i}.close"), &s.close_label, Role::Button, enabled, Details::default());
        }
        if visible || response.hovered() {
            let painter = ui.painter();
            if enabled && response.hovered() {
                painter.circle_filled(at.center(), side / 2.0, s.hover_background);
            }
            let k = side * 0.22;
            let c = at.center();
            let stroke = Stroke::new(1.25, s.text);
            painter.line_segment([c + vec2(-k, -k), c + vec2(k, k)], stroke);
            painter.line_segment([c + vec2(-k, k), c + vec2(k, -k)], stroke);
        }
        response.clicked()
    }

    /// Arrows, Home and End move the selection (and focus); ⌘W closes; ⌘⇧ + arrow reorders.
    fn keys(&self, ui: &Ui, tabs: &[StripTab<'_>], selected: usize, out: &mut StripOut) {
        let count = tabs.len();
        let Some(f) = out.responses.iter().position(|r| r.as_ref().is_some_and(Response::has_focus)) else { return };
        let closable = tabs.get(f).is_some_and(|t| t.closable);
        let reorderable = self.reorderable;
        enum Step {
            Focus(usize),
            Close,
            Move(usize),
        }
        let step = ui.input_mut(|inp| {
            let mut key = |m, k| inp.consume_key(m, k);
            let shifted = Modifiers::COMMAND | Modifiers::SHIFT;
            if closable && key(Modifiers::COMMAND, Key::W) {
                Some(Step::Close)
            } else if reorderable && key(shifted, Key::ArrowRight) {
                Some(Step::Move((f + 1).min(count - 1)))
            } else if reorderable && key(shifted, Key::ArrowLeft) {
                Some(Step::Move(f.saturating_sub(1)))
            } else if key(Modifiers::NONE, Key::ArrowRight) {
                Some(Step::Focus((f + 1) % count))
            } else if key(Modifiers::NONE, Key::ArrowLeft) {
                Some(Step::Focus((f + count - 1) % count))
            } else if key(Modifiers::NONE, Key::Home) {
                Some(Step::Focus(0))
            } else if key(Modifiers::NONE, Key::End) {
                Some(Step::Focus(count - 1))
            } else {
                None
            }
        });
        match step {
            Some(Step::Close) => out.closed = Some(f),
            Some(Step::Move(to)) => {
                if to != f {
                    out.moved = Some((f, to));
                    // the caller applies the move, so the tab is at `to` next frame
                    if let Some(t) = tabs.get(to) {
                        ui.memory_mut(|m| m.request_focus(t.id));
                    }
                }
            }
            Some(Step::Focus(n)) => {
                if let Some(t) = tabs.get(n) {
                    // the arrow already asked egui to move the focus spatially: keep it in the strip
                    ui.memory_mut(|m| {
                        m.move_focus(FocusDirection::None);
                        m.request_focus(t.id);
                    });
                }
                if n != selected || out.selected.is_none() {
                    out.selected = Some(n);
                }
            }
            None => {}
        }
    }

    /// Drag a tab along the strip; a line marks where it lands, release reports the move.
    fn drag(&self, ui: &Ui, shown: &[(usize, Rect)], out: &mut StripOut) {
        let Some((from, r)) =
            out.responses.iter().enumerate().find_map(|(i, r)| r.as_ref().filter(|r| r.dragged() || r.drag_stopped()).map(|r| (i, r)))
        else {
            return;
        };
        let Some(pointer) = ui.input(|i| i.pointer.latest_pos()).filter(|p| p.is_finite()) else { return };
        let (to, line_x) = drop_index(shown, from, pointer.x);
        if r.drag_stopped() {
            if to != from {
                out.moved = Some((from, to));
            }
        } else if to != from {
            let y = self.rect.y_range();
            let stroke = Stroke::new(2.0, if self.style.indicator.a() > 0 { self.style.indicator } else { self.style.focus.color });
            ui.painter().with_clip_rect(self.rect.intersect(ui.clip_rect())).vline(line_x, y, stroke);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }

    /// The overflow button and its menu of hidden tabs.
    fn overflow(
        &self,
        ui: &mut Ui,
        tabs: &[StripTab<'_>],
        button: Rect,
        hidden: &[usize],
        menu_item: &mut dyn FnMut(&mut Ui, usize) -> Response,
        out: &mut StripOut,
    ) {
        let s = self.style;
        let r = ui.interact(button, self.overflow_id, Sense::click());
        let enabled = ui.is_enabled();
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &s.overflow_label));
        if let Some(a) = self.automation {
            automation::register_fmt(ui, button, format_args!("{a}.overflow"), &s.overflow_label, Role::Button, enabled, Details::default());
        }
        if enabled && r.hovered() {
            ui.painter().rect_filled(button, 0.0, s.hover_background);
        }
        ui.painter().with_clip_rect(button.intersect(ui.clip_rect())).text(
            button.center(),
            Align2::CENTER_CENTER,
            "\u{2026}",
            s.font.clone(),
            s.text,
        );
        let automation = self.automation;
        let mut picked = None;
        let popup = egui::Popup::menu(&r).show(|ui| {
            ScrollArea::vertical().max_height(MENU_HEIGHT).show(ui, |ui| {
                // the hidden tabs are unbounded
                list::checked(ui, |ui| {
                    for &i in hidden {
                        let row = menu_item(ui, i);
                        if let Some(a) = automation {
                            let label = tabs.get(i).map_or("", |t| t.label);
                            automation::register_fmt(
                                ui,
                                row.rect,
                                format_args!("{a}.overflow.{i}"),
                                label,
                                Role::MenuItem,
                                row.enabled(),
                                Details::default(),
                            );
                        }
                        if row.clicked() {
                            picked = Some(i);
                            ui.close();
                        }
                    }
                });
            });
        });
        out.overflow_open = popup.is_some();
        if picked.is_some() {
            out.selected = picked;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_keeps_the_selection_on_its_tab() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        let mut s = 1;
        reorder(&mut v, &mut s, (0, 3));
        assert_eq!((v.clone(), s), (vec!['b', 'c', 'd', 'a'], 0));
        reorder(&mut v, &mut s, (3, 0));
        assert_eq!((v.clone(), s), (vec!['a', 'b', 'c', 'd'], 1));
        reorder(&mut v, &mut s, (1, 2));
        assert_eq!((v.clone(), s), (vec!['a', 'c', 'b', 'd'], 2));
        reorder(&mut v, &mut s, (9, 0));
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn drop_index_inserts_between_centres() {
        let r = |l: f32| Rect::from_min_size(pos2(l, 0.0), vec2(10.0, 10.0));
        let shown = [(0, r(0.0)), (1, r(10.0)), (2, r(20.0))];
        assert_eq!(drop_index(&shown, 0, 100.0).0, 2);
        assert_eq!(drop_index(&shown, 2, -5.0).0, 0);
        assert_eq!(drop_index(&shown, 0, 17.0).0, 1);
        assert_eq!(drop_index(&shown, 2, 100.0).0, 2);
        assert_eq!(drop_index(&[(0, r(0.0))], 0, 50.0).0, 0);
    }
}
