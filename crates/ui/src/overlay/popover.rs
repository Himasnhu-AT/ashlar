//! An anchored floating surface.

use egui::accesskit::Role as NodeRole;
use egui::{Align2, Area, Id, Key, LayerId, Margin, Modifiers, Order, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2};

use crate::automation::{self, Details, Role};
use crate::motion;
use crate::tokens;

/// Where a surface of `size` goes next to `anchor` inside `screen`: below the anchor, or above it
/// when it does not fit below and there is more room above. Returns the top-left corner and
/// whether it flipped above. The result always lies inside `screen` when the surface fits.
pub fn place(anchor: Rect, size: Vec2, screen: Rect, gap: f32) -> (Pos2, bool) {
    let sane = |v: f32| if v.is_finite() { v } else { 0.0 };
    let size = Vec2::new(sane(size.x).max(0.0), sane(size.y).max(0.0));
    let gap = sane(gap).max(0.0);
    if !anchor.is_finite() || !screen.is_finite() || screen.is_negative() {
        return (Pos2::ZERO, false);
    }
    let below = screen.bottom() - (anchor.bottom() + gap);
    let above = (anchor.top() - gap) - screen.top();
    let flipped = size.y > below && above > below;
    let y = if flipped { anchor.top() - gap - size.y } else { anchor.bottom() + gap };
    let y = y.clamp(screen.top(), (screen.bottom() - size.y).max(screen.top()));
    let x = anchor.left().clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
    (pos2(x, y), flipped)
}

#[derive(Clone, Copy, Debug)]
struct State {
    was_open: bool,
    rect: Rect,
}

impl Default for State {
    fn default() -> Self {
        Self { was_open: false, rect: Rect::NOTHING }
    }
}

/// A surface anchored to a response. The caller owns `open`; the popover sets it to `false` on
/// Escape or a click outside both the surface and the anchor (clicks on the anchor are the
/// caller's, usually a toggle).
///
/// Opening moves the focus to the surface (Tab then walks into its widgets); closing returns the
/// focus to the anchor unless the user clicked somewhere else.
///
/// ```
/// # use ashlar_ui::overlay::Popover;
/// # fn frame(ui: &mut egui::Ui, open: &mut bool) {
/// let button = ui.button("Filters");
/// if button.clicked() {
///     *open = !*open;
/// }
/// Popover::new("filters").show(&button, open, |ui| {
///     ui.label("Only show errors");
/// });
/// # }
/// ```
pub struct Popover<'a> {
    id: &'a str,
    egui_id: Id,
    width: Option<f32>,
    /// Public popovers take the focus and register themselves; the select and the combobox keep
    /// the focus on their anchor and register their own list.
    standalone: bool,
}

impl<'a> Popover<'a> {
    /// `id` is the automation id of the surface.
    pub fn new(id: &'a str) -> Self {
        Self { id, egui_id: Id::new(("ashlar_ui::popover", id)), width: None, standalone: true }
    }

    /// A popover that leaves the focus on its anchor and registers nothing itself.
    pub(crate) fn attached(egui_id: Id) -> Self {
        Self { id: "", egui_id, width: None, standalone: false }
    }

    /// Fixed content width in points (default: as wide as the content).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width.is_finite().then_some(width.clamp(40.0, 4000.0));
        self
    }

    /// Draw the surface while it is open or fading out. Returns what `add` returned, `None`
    /// while hidden.
    pub fn show<R>(self, anchor: &Response, open: &mut bool, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        let ctx = anchor.ctx.clone();
        let t = tokens::current(&ctx);
        let state_id = self.egui_id.with("state");
        let fade_id = self.egui_id.with("fade");
        let layer = LayerId::new(Order::Foreground, self.egui_id);
        let mut st: State = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();

        let mut clicked_outside = false;
        let mut escaped = false;
        if *open && st.was_open {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                *open = false;
                escaped = true;
            } else {
                clicked_outside = ctx
                    .input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !st.rect.contains(p) && !anchor.rect.contains(p)));
                if clicked_outside {
                    *open = false;
                }
            }
        }
        let just_opened = *open && !st.was_open;
        if just_opened {
            // animate the arrival even the first time (presence would otherwise start shown)
            motion::set(&ctx, fade_id, 0.0_f32);
        }
        if st.was_open && !*open && !clicked_outside {
            let focused = ctx.memory(|m| m.focused());
            let inside = focused.and_then(|f| ctx.read_response(f)).is_some_and(|r| r.layer_id == layer);
            if escaped || inside || focused.is_none() {
                ctx.memory_mut(|m| m.request_focus(anchor.id));
            }
        }
        st.was_open = *open;

        let Some(alpha) = motion::presence(&ctx, fade_id, *open, t.motion.enter_exit) else {
            ctx.data_mut(|d| d.insert_temp(state_id, st));
            return None;
        };
        let screen = ctx.content_rect().shrink(t.space.xs);
        let gap = t.space.xxs;
        let last_size = egui::AreaState::load(&ctx, self.egui_id).and_then(|a| a.size).unwrap_or(Vec2::ZERO);
        let (pos, flipped) = place(anchor.rect, last_size, screen, gap);
        let room = if flipped { anchor.rect.top() - gap - screen.top() } else { screen.bottom() - anchor.rect.bottom() - gap };
        // pinned by its bottom edge when above, so a shrinking list stays attached to the anchor
        let (pivot, at) = if flipped { (Align2::LEFT_BOTTOM, pos2(pos.x, pos.y + last_size.y)) } else { (Align2::LEFT_TOP, pos) };
        let frame = egui::Frame::new()
            .fill(t.surface.raised)
            .stroke(Stroke::new(t.stroke.hairline, t.border))
            .corner_radius(t.radius.md)
            .inner_margin(Margin::same(t.space.xs as i8))
            .shadow(t.elevation.popup);
        let width = self.width;
        let out =
            Area::new(self.egui_id).order(Order::Foreground).pivot(pivot).fixed_pos(at).constrain(false).fade_in(false).interactable(*open).show(
                &ctx,
                |ui| {
                    ui.multiply_opacity(alpha);
                    let focus_target = self.standalone.then(|| ui.interact(st.rect, self.egui_id.with("focus"), Sense::focusable_noninteractive()));
                    if let Some(r) = &focus_target {
                        if just_opened {
                            r.request_focus();
                        }
                        ui.ctx().accesskit_node_builder(r.id, |n| n.set_role(NodeRole::Dialog));
                    }
                    let inner = frame
                        .show(ui, |ui| {
                            if let Some(w) = width {
                                ui.set_width(w - t.space.xs * 2.0);
                            }
                            ui.set_max_height((room - t.space.xs * 2.0).max(t.control_height));
                            add(ui)
                        })
                        .inner;
                    if self.standalone {
                        automation::register_fmt(ui, ui.min_rect(), format_args!("{}", self.id), "", Role::Group, *open, Details::default());
                    }
                    inner
                },
            );
        st.rect = out.response.rect;
        ctx.data_mut(|d| d.insert_temp(state_id, st));
        Some(out.inner)
    }
}
