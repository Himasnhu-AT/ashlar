//! Cheap rows for plain lists, and a debug check that points long plain lists to
//! [`VirtualList`](crate::virtual_list::VirtualList).
//!
//! A plain `ScrollArea` builds every row every frame, so the per-row cost is what matters. A
//! [`Row`] is one allocated rect, one `interact` and, only when the row is on screen, a cached
//! galley and a fill. A `Button::selectable` row costs about three times as much: it builds a
//! child `Ui`, an atom layout and lays its text out whether or not the row is visible.
//!
//! ```
//! # use ashlar_ui::list::{self, Row};
//! # let ctx = egui::Context::default();
//! # let mut out = ctx.run_ui(Default::default(), |ui| {
//! let names = ["Inbox", "Drafts", "Sent"];
//! let mut selected = 0;
//! egui::ScrollArea::vertical().show(ui, |ui| {
//!     list::checked(ui, |ui| {
//!         for (i, name) in names.iter().enumerate() {
//!             if ui.add(Row::new(i, name).selected(i == selected)).clicked() {
//!                 selected = i;
//!             }
//!         }
//!     });
//! });
//! # });
//! # out.textures_delta.clear();
//! ```

use std::hash::Hash;
use std::panic::Location;
use std::sync::{Mutex, OnceLock, PoisonError};

use egui::accesskit::Role;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, Response, Sense, StrokeKind, TextStyle, Ui, Widget, WidgetInfo, WidgetType, pos2, vec2};

use crate::automation::{self, Details};
use crate::cache;

/// A full-width, single-line, selectable list row: a hover and selection fill, a label truncated
/// with "…" to the row's width, clickable and focusable (Tab reaches it, Space and Enter click it).
/// Screen readers see a list item with its label and selection state.
#[must_use = "add it with `ui.add(row)`"]
pub struct Row<'a> {
    id_salt: u64,
    text: &'a str,
    selected: bool,
    height: Option<f32>,
    automation_id: Option<&'a str>,
}

impl<'a> Row<'a> {
    /// A row whose id is `id_salt` within the parent `Ui` (a row index or a stable item key).
    pub fn new(id_salt: impl Hash, text: &'a str) -> Self {
        Self { id_salt: cache::hash_of(&id_salt), text, selected: false, height: None, automation_id: None }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The row height in points (default: the style's interact height). Non-finite or
    /// non-positive values fall back to the default.
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// Register the row with [`automation`] under `id` (off by default: registering costs a
    /// little per row, and most rows are never driven by an agent).
    pub fn automation_id(mut self, id: &'a str) -> Self {
        self.automation_id = Some(id);
        self
    }
}

impl Widget for Row<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let default_height = ui.spacing().interact_size.y;
        let height = self.height.filter(|h| h.is_finite() && *h > 0.0).unwrap_or(default_height);
        let width = Some(ui.available_width()).filter(|w| w.is_finite()).unwrap_or(0.0).max(0.0);
        let (_, rect) = ui.allocate_space(vec2(width, height));
        let id = ui.id().with(self.id_salt);
        let response = ui.interact(rect, id, Sense::click());

        let enabled = ui.is_enabled();
        let (text, selected) = (self.text, self.selected);
        response.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, text));
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(Role::ListItem);
            node.clear_toggled();
            node.set_selected(selected);
        });
        if let Some(aid) = self.automation_id {
            let details = Details { selected: Some(selected), ..Details::default() };
            automation::register_with(ui, &response, aid, text, automation::Role::Row, details);
        }

        // off-screen rows stop here: no text layout, no shapes
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() || response.highlighted() || response.has_focus() {
            ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, StrokeKind::Inside);
        }
        let pad = ui.spacing().button_padding.x;
        let max_width = (rect.width() - 2.0 * pad).max(0.0);
        let font = TextStyle::Button.resolve(ui.style());
        // the revision covers everything the galley is built from, so a changed label never shows stale
        let revision = cache::hash_of(&(text, &font, max_width.to_bits()));
        let galley = cache::galley(ui, id, revision, || {
            let mut job = LayoutJob::simple_singleline(text.to_owned(), font, Color32::PLACEHOLDER);
            job.wrap = TextWrapping::truncate_at_width(max_width);
            job
        });
        let pos = pos2(rect.left() + pad, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, visuals.text_color());
        response
    }
}

/// How many interactive widgets one [`checked`] block may build in a frame before the debug warning.
pub const MANY_WIDGETS: usize = 2_000;

/// Set this environment variable (to anything) to silence the [`checked`] warning.
pub const QUIET_ENV: &str = "ASHLAR_QUIET_LISTS";

/// Run `add_contents` (usually a `ScrollArea`'s content) and, in debug builds, warn once per call
/// site when it built more than [`MANY_WIDGETS`] interactive widgets in one frame: such a list
/// should be a [`VirtualList`](crate::virtual_list::VirtualList), which builds only the visible
/// rows. Release builds just run `add_contents`.
#[track_caller]
pub fn checked<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    if !cfg!(debug_assertions) {
        return add_contents(ui);
    }
    let site = Location::caller();
    let (out, count) = count_interactive(ui, add_contents);
    if count > MANY_WIDGETS && !quiet() && first_warning(site) {
        log::warn!(
            "{site}: this list built {count} interactive widgets in one frame; every row costs time each frame. \
             Use ashlar_ui::virtual_list::VirtualList to build only the visible rows (set {QUIET_ENV}=1 to silence)."
        );
    }
    out
}

/// `add_contents`'s result and how many interactive widgets it registered on this layer.
pub(crate) fn count_interactive<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> (R, usize) {
    let layer = ui.layer_id();
    let len = |ctx: &egui::Context| ctx.viewport(|vp| vp.this_pass.widgets.get_layer(layer).count());
    let before = len(ui.ctx());
    let out = add_contents(ui);
    // widgets registered again with a known id update in place, so the new ones are the tail
    let count = ui.ctx().viewport(|vp| vp.this_pass.widgets.get_layer(layer).skip(before).filter(|w| w.sense.interactive()).count());
    (out, count)
}

fn quiet() -> bool {
    static QUIET: OnceLock<bool> = OnceLock::new();
    *QUIET.get_or_init(|| std::env::var_os(QUIET_ENV).is_some())
}

/// True the first time `site` asks.
fn first_warning(site: &'static Location<'static>) -> bool {
    static WARNED: Mutex<Vec<&'static Location<'static>>> = Mutex::new(Vec::new());
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if warned.contains(&site) {
        return false;
    }
    warned.push(site);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Key;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};

    #[test]
    fn click_and_keyboard_select_and_the_row_is_a_list_item() {
        let mut h = Harness::builder().with_size(vec2(300.0, 200.0)).build_ui_state(
            |ui, sel: &mut Option<usize>| {
                for (i, name) in ["Alpha", "Beta"].iter().enumerate() {
                    let id = if i == 0 { "rows.alpha" } else { "rows.beta" };
                    if ui.add(Row::new(i, name).selected(*sel == Some(i)).automation_id(id)).clicked() {
                        *sel = Some(i);
                    }
                }
            },
            None,
        );
        h.run();
        let beta = h.get_by_label("Beta");
        assert_eq!(beta.accesskit_node().role(), Role::ListItem);
        beta.click();
        h.run();
        assert_eq!(*h.state(), Some(1));
        assert_eq!(automation::find(&h.ctx, "rows.beta").and_then(|e| e.selected), Some(true));
        assert_eq!(h.get_by_label("Beta").accesskit_node().is_selected(), Some(true));

        // Tab focuses the first row, Enter clicks it
        h.key_press(Key::Tab);
        h.run();
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(*h.state(), Some(0));
    }

    #[test]
    fn odd_sizes_do_not_panic_and_long_text_is_truncated() {
        let long = "x".repeat(10_000);
        let mut h = Harness::builder().with_size(vec2(120.0, 100.0)).build_ui(|ui| {
            let _ = ui.add(Row::new("nan", "nan").height(f32::NAN));
            let _ = ui.add(Row::new("neg", "neg").height(-4.0));
            let _ = ui.add(Row::new("empty", ""));
            let r = ui.add(Row::new("long", &long));
            assert!(r.rect.width() <= 120.0);
        });
        h.run();
    }

    #[test]
    fn checked_counts_interactive_widgets_only() {
        let ctx = egui::Context::default();
        let mut counted = 0;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ((), n) = count_interactive(ui, |ui| {
                for i in 0..30 {
                    let _ = ui.add(Row::new(i, "row"));
                }
                let _ = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            });
            counted = n;
        });
        out.textures_delta.clear();
        assert_eq!(counted, 30);
        let site = Location::caller();
        assert!(first_warning(site));
        assert!(!first_warning(site), "one warning per call site");
    }
}
