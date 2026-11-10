//! A button that opens a list of options and shows the chosen one.

use egui::{Align2, EventFilter, Id, Key, Modifiers, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::options::OptionList;
use super::popover::Popover;
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

#[derive(Clone, Copy, Debug, Default)]
struct Memory {
    open: bool,
    cursor: usize,
}

/// Choose one of a fixed list of options. `current` is an index into `options`; an index out of
/// range shows the placeholder.
///
/// Keyboard: Space, Enter or the arrows open it; the arrows, Home and End move; Enter or Space
/// chooses; Escape closes without choosing. The focus stays on the button while the list is
/// open. `changed()` on the response reports a choice.
pub struct Select<'a, T: AsRef<str>> {
    id: &'a str,
    options: &'a [T],
    current: &'a mut usize,
    width: Option<f32>,
    label: &'a str,
    placeholder: &'a str,
}

impl<'a, T: AsRef<str>> Select<'a, T> {
    /// `id` is the automation id; options register as `<id>.option.<n>`.
    pub fn new(id: &'a str, options: &'a [T], current: &'a mut usize) -> Self {
        Self { id, options, current, width: None, label: "", placeholder: "Choose…" }
    }

    /// Width of the button in points (default: 12 control heights).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width.is_finite().then_some(width.clamp(40.0, 4000.0));
        self
    }

    /// The accessible name (what the select chooses: "Font size").
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// Shown when `current` is out of range.
    pub fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let id = Id::new(("ashlar_ui::select", self.id));
        let mem_id = id.with("memory");
        let mut mem: Memory = ui.data(|d| d.get_temp(mem_id)).unwrap_or_default();
        let n = self.options.len();
        let label_of = |i: usize| self.options.get(i).map_or("", |o| o.as_ref());
        let current = (*self.current < n).then_some(*self.current);
        let shown = current.map_or(self.placeholder, label_of);

        let mut choose = None;
        let mut moved = false;
        let focused = ui.memory(|m| m.has_focus(id));
        if focused {
            // arrows drive the list instead of moving the focus; Escape closes instead of blurring
            let filter = EventFilter { tab: false, horizontal_arrows: false, vertical_arrows: true, escape: mem.open };
            ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
            let (up, down, home, end, enter, space) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    mem.open && i.consume_key(Modifiers::NONE, Key::Home),
                    mem.open && i.consume_key(Modifiers::NONE, Key::End),
                    i.consume_key(Modifiers::NONE, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::Space),
                )
            });
            let last = n.saturating_sub(1);
            if !mem.open {
                if (up || down || enter || space) && n > 0 {
                    mem.open = true;
                    mem.cursor = current.unwrap_or(0);
                    moved = true;
                }
            } else {
                if up {
                    mem.cursor = mem.cursor.saturating_sub(1);
                }
                if down {
                    mem.cursor = mem.cursor.saturating_add(1).min(last);
                }
                if home {
                    mem.cursor = 0;
                }
                if end {
                    mem.cursor = last;
                }
                moved = up || down || home || end;
                if (enter || space) && mem.cursor < n {
                    choose = Some(mem.cursor);
                }
            }
        }

        let width = self.width.unwrap_or(t.control_height * 12.0).min(ui.available_width().max(40.0));
        let (rect, _) = ui.allocate_exact_size(vec2(width, t.control_height), Sense::hover());
        let mut response = ui.interact(rect, id, Sense::click());
        if response.clicked() {
            // keep the keyboard working after a pointer open
            response.request_focus();
            mem.open = !mem.open && n > 0;
            mem.cursor = current.unwrap_or(0);
            moved = true;
        }
        if mem.open && response.lost_focus() && !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
            // tabbed away
            mem.open = false;
        }
        paint_button(ui, &t, &response, shown, current.is_none(), focused);

        let cursor = mem.cursor.min(n.saturating_sub(1));
        let option_of = |row: usize| (row < n).then_some(row);
        let id_str = self.id;
        let clicked = Popover::attached(id.with("popup")).width(width).show(&response, &mut mem.open, |ui| {
            OptionList {
                id: id_str,
                rows: n,
                option_of: &option_of,
                label: &label_of,
                cursor: Some(cursor),
                chosen: current,
                scroll_to: moved.then_some(cursor),
            }
            .show(ui)
        });
        if let Some(Some(row)) = clicked {
            choose = Some(row);
        }
        if let Some(i) = choose
            && i < n
        {
            mem.open = false;
            mem.cursor = i;
            if *self.current != i {
                *self.current = i;
                response.mark_changed();
            }
        }
        ui.data_mut(|d| d.insert_temp(mem_id, mem));

        let chosen_label = if *self.current < n { label_of(*self.current) } else { "" };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, self.label));
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(egui::accesskit::Role::ComboBox);
            node.set_expanded(mem.open);
            node.set_value(chosen_label);
        });
        automation::register_with(
            ui,
            &response,
            self.id,
            self.label,
            Role::ComboBox,
            Details { value: Some(chosen_label), selected: Some(mem.open) },
        );
        response
    }
}

fn paint_button(ui: &Ui, t: &Tokens, response: &Response, text: &str, placeholder: bool, focused: bool) {
    let rect = response.rect;
    let fill = if response.is_pointer_button_down_on() {
        t.surface.pressed
    } else if response.hovered() {
        t.surface.hover
    } else {
        t.surface.sunken
    };
    let p = ui.painter();
    p.rect(rect, t.radius.md, fill, Stroke::new(t.stroke.hairline, t.border), StrokeKind::Inside);
    if focused {
        p.rect_stroke(rect.expand(1.0), t.radius.md, Stroke::new(t.stroke.focus, t.focus), StrokeKind::Outside);
    }
    let color = if placeholder { t.text.secondary } else { t.text.primary };
    let text_rect = rect.shrink2(vec2(t.space.md, 0.0)).with_max_x(rect.right() - t.space.md * 2.0 - 8.0);
    let clip = p.with_clip_rect(text_rect.intersect(p.clip_rect()));
    clip.text(pos2(text_rect.left(), rect.center().y), Align2::LEFT_CENTER, text, Tokens::font(t.type_scale.body), color);
    // chevron
    let c = pos2(rect.right() - t.space.md - 3.0, rect.center().y);
    let s = Stroke::new(1.4, t.text.secondary);
    p.line_segment([c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0)], s);
    p.line_segment([c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)], s);
}
