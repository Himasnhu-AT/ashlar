//! A section that opens and closes under its header.

use egui::{CornerRadius, Key, Modifiers, Pos2, Response, Sense, Shape, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::controls::{FOCUS_WIDTH, text_galley};
use crate::tokens::{self, Tokens};

/// The chevron's three points in a square of side `side` centred at `c`: pointing right when
/// closed, down when open.
pub(crate) fn chevron(c: Pos2, side: f32, open: bool) -> Vec<Pos2> {
    let h = side / 2.0;
    if open {
        vec![pos2(c.x - h, c.y - h / 2.0), pos2(c.x + h, c.y - h / 2.0), pos2(c.x, c.y + h / 2.0 + h / 4.0)]
    } else {
        vec![pos2(c.x - h / 2.0, c.y - h), pos2(c.x + h / 2.0 + h / 4.0, c.y), pos2(c.x - h / 2.0, c.y + h)]
    }
}

/// Where the chevron sits in a header row starting at `left`, centred on `y`.
pub(crate) fn chevron_center(t: &Tokens, left: f32, y: f32) -> (Pos2, f32) {
    let side = (t.type_scale.body * 0.6).round().max(4.0);
    (pos2(left + t.space.sm + side / 2.0, y), side)
}

/// A header row (chevron and title) that shows or hides the content under it. A click, Space or
/// Enter toggles `open`; with the header focused, the right arrow opens and the left arrow closes.
/// Several in a column make an accordion; [`Accordion`] keeps at most one of them open.
pub struct Collapsible<'a> {
    id: &'a str,
    title: &'a str,
    open: &'a mut bool,
}

/// What [`Collapsible::show`] reports.
pub struct CollapsibleOutput<R> {
    /// The header: `changed()` when it was opened or closed this frame.
    pub header: Response,
    /// What the content closure returned, when open.
    pub body: Option<R>,
}

impl<'a> Collapsible<'a> {
    pub fn new(id: &'a str, title: &'a str, open: &'a mut bool) -> Self {
        Collapsible { id, title, open }
    }

    pub fn show<R>(self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> CollapsibleOutput<R> {
        let t = tokens::current(ui.ctx());
        let galley = text_galley(ui, ("ashlar_ui::content::collapsible", self.id), self.title, t.type_scale.body);
        let width = ui.available_width().clamp(0.0, 8000.0);
        let (auto_id, rect) = ui.allocate_space(vec2(width, t.control_height.max(galley.size().y)));
        let id = if self.id.is_empty() { auto_id } else { ui.make_persistent_id(("ashlar_ui::content::collapsible", self.id)) };
        let mut header = ui.interact(rect, id, Sense::click());
        let mut toggled = header.clicked();
        if header.has_focus() {
            let open = *self.open;
            let (right, left) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowRight), i.consume_key(Modifiers::NONE, Key::ArrowLeft)));
            toggled |= (right && !open) || (left && open);
        }
        if toggled {
            *self.open = !*self.open;
            header.mark_changed();
        }
        let open = *self.open;
        let enabled = ui.is_enabled();
        header.widget_info(|| WidgetInfo::labeled(WidgetType::CollapsingHeader, enabled, self.title));
        ui.ctx().accesskit_node_builder(header.id, |node| node.set_expanded(open));

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let radius = CornerRadius::same(t.radius.sm);
            if enabled && header.is_pointer_button_down_on() {
                painter.rect_filled(rect, radius, t.surface.pressed);
            } else if enabled && header.hovered() {
                painter.rect_filled(rect, radius, t.surface.hover);
            }
            let (c, side) = chevron_center(&t, rect.left(), rect.center().y);
            painter.add(Shape::convex_polygon(chevron(c, side, open), t.text.secondary, Stroke::NONE));
            let x = c.x + side / 2.0 + t.space.sm;
            painter.galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, t.text.primary);
            if header.has_focus() {
                painter.rect_stroke(rect, radius, Stroke::new(FOCUS_WIDTH, t.focus), egui::StrokeKind::Inside);
            }
        }
        automation::register_with(ui, &header, self.id, self.title, Role::Disclosure, Details { value: None, selected: Some(open) });
        // indented under the title, without a guide line
        let indent = t.space.xl;
        let body = open.then(|| {
            ui.horizontal_top(|ui| {
                ui.add_space(indent);
                ui.vertical(add).inner
            })
            .inner
        });
        CollapsibleOutput { header, body }
    }
}

/// Collapsible sections of which at most one is open: opening one closes the one open before.
/// `open` is the open section's index (`None`: all closed). Each section's automation id is
/// `<id>.<index>`.
///
/// ```
/// # use ashlar_ui::content::Accordion;
/// # fn ui(ui: &mut egui::Ui, open: &mut Option<usize>) {
/// let mut acc = Accordion::new("help.faq", open);
/// acc.section(ui, 0, "Shipping", |ui| { ui.label("…"); });
/// acc.section(ui, 1, "Returns", |ui| { ui.label("…"); });
/// # }
/// ```
pub struct Accordion<'a> {
    id: &'a str,
    open: &'a mut Option<usize>,
}

impl<'a> Accordion<'a> {
    pub fn new(id: &'a str, open: &'a mut Option<usize>) -> Self {
        Accordion { id, open }
    }

    /// The section at `index`, shown open when it is the open one.
    pub fn section<R>(&mut self, ui: &mut Ui, index: usize, title: &str, add: impl FnOnce(&mut Ui) -> R) -> CollapsibleOutput<R> {
        let mut open = *self.open == Some(index);
        let id = if self.id.is_empty() { String::new() } else { format!("{}.{index}", self.id) };
        let out = Collapsible::new(&id, title, &mut open).show(ui, add);
        if out.header.changed() {
            if open {
                // a section above this one may have drawn open this frame: draw once more
                if self.open.is_some_and(|o| o < index) {
                    crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
                }
                *self.open = Some(index);
            } else if *self.open == Some(index) {
                *self.open = None;
            }
        }
        out
    }
}
