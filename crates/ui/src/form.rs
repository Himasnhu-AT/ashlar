//! The settings form pattern: sections of rows (label, description, control), a "modified"
//! marker with a reset button, a filter that hides rows which do not match a query, and
//! [`Field`] for a labelled input with a required marker and an announced error.
//!
//! Every row's control gets the automation id `<section>.<row>` (passed to the control closure),
//! so an agent can find and set any preference; its reset button is `<section>.<row>.reset`.
//! The control's AccessKit node is named by the row label and described by the row description.
//!
//! ```no_run
//! # use ashlar_ui::{controls::Switch, form};
//! # fn demo(ui: &mut egui::Ui, query: &str, launch: &mut bool) {
//! form::filter(ui, "settings", query, |f| {
//!     f.section("general", "General", |s| {
//!         s.row("launch", "Launch at login")
//!             .description("Starts with your session")
//!             .keywords(&["startup", "boot"])
//!             .bind(launch, false)
//!             .control(|ui, id, on| Switch::new(id, "", on).show(ui));
//!     });
//! });
//! # }
//! ```
//!
//! Rows render as they are declared, so a row whose control and reset both need the same value
//! uses [`Row::bind`]; with [`Row::modified`] the caller resets when [`RowOutput::reset`] is set.

use std::ops::Range;
use std::sync::OnceLock;

use egui::accesskit::{Invalid, Live};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, CornerRadius, Layout, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2};

use crate::automation::{self, Details, Role};
use crate::content::EmptyState;
use crate::controls::{Button, Size, Variant};
use crate::tokens::{self, Tokens};

/// Longest query (in characters) the filter compares; the rest is ignored.
pub const MAX_QUERY_CHARS: usize = 128;

/// Words the form shows (localized by the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormLabels {
    /// Title of the empty state when the filter hides every row.
    pub no_match: String,
    /// Body of that empty state.
    pub no_match_body: String,
    /// The reset button on a modified row.
    pub reset: String,
    /// Spoken name of the required marker on a [`Field`].
    pub required: String,
}

impl Default for FormLabels {
    fn default() -> Self {
        FormLabels {
            no_match: "No settings match".into(),
            no_match_body: "Try a different search.".into(),
            reset: "Reset".into(),
            required: "Required".into(),
        }
    }
}

fn default_labels() -> &'static FormLabels {
    static LABELS: OnceLock<FormLabels> = OnceLock::new();
    LABELS.get_or_init(FormLabels::default)
}

/// What a form showed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormOutput {
    /// Rows declared.
    pub rows: usize,
    /// Rows shown (all of them when there is no query).
    pub visible: usize,
}

impl FormOutput {
    /// True when the filter hid every row (the empty state was shown).
    pub fn is_empty(&self) -> bool {
        self.visible == 0 && self.rows > 0
    }
}

/// A form: one or more sections, optionally filtered by a query.
pub struct Form<'a> {
    id: &'a str,
    query: &'a str,
    labels: &'a FormLabels,
}

impl<'a> Form<'a> {
    /// `id` names the form for automation (its empty state is `<id>.empty`).
    pub fn new(id: &'a str) -> Self {
        Form { id, query: "", labels: default_labels() }
    }

    /// Show only rows whose label, description or keywords contain `query` (case-insensitive),
    /// and every row of a section whose title matches.
    pub fn query(mut self, query: &'a str) -> Self {
        self.query = query;
        self
    }

    pub fn labels(mut self, labels: &'a FormLabels) -> Self {
        self.labels = labels;
        self
    }

    pub fn show(self, ui: &mut Ui, add: impl FnOnce(&mut FormUi<'_, 'a>)) -> FormOutput {
        let needle: Vec<char> = self.query.trim().chars().take(MAX_QUERY_CHARS).map(lower).collect();
        let labels = self.labels;
        let form_id = self.id;
        ui.vertical(|ui| {
            let mut f = FormUi { ui, st: State { needle, labels, rows: 0, visible: 0, id_buf: String::new() } };
            add(&mut f);
            let FormUi { ui, st } = f;
            let out = FormOutput { rows: st.rows, visible: st.visible };
            if out.is_empty() {
                let mut id = st.id_buf;
                id.clear();
                id.push_str(form_id);
                id.push_str(".empty");
                EmptyState::new(&id, &labels.no_match).body(&labels.no_match_body).show(ui);
            }
            out
        })
        .inner
    }
}

/// Sections of rows without a filter: shorthand for `Form::new(id).show(..)` with one section.
pub fn section(ui: &mut Ui, id: &str, title: &str, add: impl FnOnce(&mut Section<'_, '_>)) -> FormOutput {
    Form::new(id).show(ui, |f| f.section(id, title, add))
}

/// A form filtered by `query`: hides non-matching rows and sections with no visible rows,
/// highlights matches and shows "No settings match" when nothing is left.
pub fn filter<'a>(ui: &mut Ui, id: &'a str, query: &'a str, add: impl FnOnce(&mut FormUi<'_, 'a>)) -> FormOutput {
    Form::new(id).query(query).show(ui, add)
}

struct State<'l> {
    /// Lower-cased query; empty means no filter.
    needle: Vec<char>,
    labels: &'l FormLabels,
    rows: usize,
    visible: usize,
    /// Reused buffer for `<section>.<row>` ids.
    id_buf: String,
}

/// The inside of a [`Form`]: add sections with [`FormUi::section`].
pub struct FormUi<'u, 'l> {
    ui: &'u mut Ui,
    st: State<'l>,
}

impl<'l> FormUi<'_, 'l> {
    /// A titled group of rows. It is not shown at all when the filter hides all its rows.
    pub fn section(&mut self, id: &str, title: &str, add: impl FnOnce(&mut Section<'_, 'l>)) {
        let title_hit = !self.st.needle.is_empty() && find_ci(title, &self.st.needle).is_some();
        let mut s = Section { ui: self.ui, st: &mut self.st, id, title, title_hit, frame: None };
        add(&mut s);
        s.finish();
    }

    /// The form's `Ui`, for content between sections.
    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }
}

struct Frame {
    bg: egui::layers::ShapeIdx,
    rect: Rect,
    rows: usize,
}

/// A section of a form: add rows with [`Section::row`].
pub struct Section<'s, 'l> {
    ui: &'s mut Ui,
    st: &'s mut State<'l>,
    id: &'s str,
    title: &'s str,
    title_hit: bool,
    frame: Option<Frame>,
}

impl<'s, 'l> Section<'s, 'l> {
    /// A row with automation id `<section>.<id>`.
    pub fn row<'r>(&'r mut self, id: &'r str, label: &'r str) -> Row<'r, 's, 'l> {
        Row { section: self, id, label, description: None, keywords: &[], modified: false, enabled: true }
    }

    /// Paints the header and reserves the background the first time a row is shown.
    fn begin(&mut self) -> &mut Frame {
        let ui = &mut *self.ui;
        let (id, title) = (self.id, self.title);
        self.frame.get_or_insert_with(|| {
            let t = tokens::current(ui.ctx());
            let header = ui.label(RichText::new(title).size(t.type_scale.heading).strong().color(t.text.primary));
            ui.ctx().accesskit_node_builder(header.id, |n| {
                n.set_role(egui::accesskit::Role::Heading);
                n.set_label(title);
            });
            automation::register(ui, &header, id, title, Role::Group);
            ui.add_space(t.space.xs);
            let bg = ui.painter().add(Shape::Noop);
            let top = ui.cursor().top();
            let full = ui.available_rect_before_wrap();
            Frame { bg, rect: Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), top)), rows: 0 }
        })
    }

    fn finish(self) {
        let Some(frame) = self.frame else { return };
        let t = tokens::current(self.ui.ctx());
        let radius = CornerRadius::same(t.radius.md);
        if frame.rect.is_finite() {
            let bg = egui::epaint::RectShape::new(frame.rect, radius, t.surface.raised, Stroke::new(1.0, t.border), StrokeKind::Inside);
            self.ui.painter().set(frame.bg, bg);
        }
        self.ui.add_space(t.space.lg);
    }
}

/// One row of a section: label and description on the left, the control on the right.
#[must_use = "a row is shown by `control`"]
pub struct Row<'r, 's, 'l> {
    section: &'r mut Section<'s, 'l>,
    id: &'r str,
    label: &'r str,
    description: Option<&'r str>,
    keywords: &'r [&'r str],
    modified: bool,
    enabled: bool,
}

/// What a row did this frame.
#[derive(Clone, Debug, Default)]
pub struct RowOutput {
    /// The control's response; `None` when the filter hid the row.
    pub response: Option<Response>,
    /// The reset button was clicked (already applied by [`Row::bind`]).
    pub reset: bool,
}

impl RowOutput {
    pub fn visible(&self) -> bool {
        self.response.is_some()
    }

    /// The control changed the value, or reset did.
    pub fn changed(&self) -> bool {
        self.reset || self.response.as_ref().is_some_and(Response::changed)
    }
}

/// Where the filter matched a row.
#[derive(Default)]
struct Hits {
    label: Option<Range<usize>>,
    description: Option<Range<usize>>,
}

impl<'r, 's, 'l> Row<'r, 's, 'l> {
    /// Secondary text under the label; also the control's AccessKit description.
    pub fn description(mut self, text: &'r str) -> Self {
        self.description = Some(text);
        self
    }

    /// Extra words the filter matches (not shown).
    pub fn keywords(mut self, words: &'r [&'r str]) -> Self {
        self.keywords = words;
        self
    }

    /// Shows the modified marker and the reset button.
    pub fn modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }

    /// Disables the control (and reset).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Ties the row to `value`: modified while it differs from `default`, and reset restores it.
    pub fn bind<T: PartialEq>(self, value: &'r mut T, default: T) -> Bound<'r, 's, 'l, T> {
        Bound { row: self, value, default }
    }

    fn hits(&self) -> Option<Hits> {
        let needle = &self.section.st.needle;
        if needle.is_empty() {
            return Some(Hits::default());
        }
        let hits = Hits { label: find_ci(self.label, needle), description: self.description.and_then(|d| find_ci(d, needle)) };
        let keyword = || self.keywords.iter().any(|k| find_ci(k, needle).is_some());
        (self.section.title_hit || hits.label.is_some() || hits.description.is_some() || keyword()).then_some(hits)
    }

    /// Shows the row. `add` draws the control and gets the row's automation id `<section>.<row>`.
    pub fn control(self, add: impl FnOnce(&mut Ui, &str) -> Response) -> RowOutput {
        self.section.st.rows = self.section.st.rows.saturating_add(1);
        let Some(hits) = self.hits() else { return RowOutput::default() };
        self.section.st.visible = self.section.st.visible.saturating_add(1);
        let Row { section, id, label, description, modified, enabled, .. } = self;
        let first = section.begin().rows == 0;
        let st = &mut *section.st;
        st.id_buf.clear();
        st.id_buf.push_str(section.id);
        st.id_buf.push('.');
        st.id_buf.push_str(id);
        let full_id = st.id_buf.as_str();
        let labels = st.labels;
        let ui = &mut *section.ui;

        let t = tokens::current(ui.ctx());
        let spacing = ui.spacing().item_spacing;
        let full = ui.available_rect_before_wrap();
        let (left, right, top) = (full.left(), full.right().max(full.left()), ui.cursor().top());
        let width = right - left;
        let (pad_x, pad_y) = (t.space.md, t.space.sm);
        let inner_top = top + pad_y;
        let bottom_limit = full.bottom().max(inner_top + t.control_height);
        let salt = ("ashlar_ui::form::row", section.id, id);

        // control (and reset) on the right, vertically centred on the label line
        let ctl_max = Rect::from_min_max(pos2(left + width * 0.4, inner_top), pos2((right - pad_x).max(left), inner_top + t.control_height));
        let mut cui = ui.new_child(UiBuilder::new().max_rect(ctl_max).layout(Layout::right_to_left(Align::Center)).id_salt(salt));
        cui.spacing_mut().item_spacing = spacing;
        if !enabled {
            cui.disable();
        }
        let response = add(&mut cui, full_id);
        ui.ctx().accesskit_node_builder(response.id, |n| {
            n.set_label(label);
            if let Some(d) = description {
                n.set_description(d);
            }
        });
        let mut reset = false;
        if modified {
            cui.add_space(t.space.sm);
            let r = Button::new(full_id, &labels.reset).part("reset").variant(Variant::Ghost).size(Size::Sm).show(&mut cui);
            ui.ctx().accesskit_node_builder(r.id, |n| n.set_description(label));
            reset = r.clicked();
        }
        let ctl_rect = cui.min_rect();

        // label and description on the left
        let text_left = left + pad_x;
        let text_right = (ctl_rect.left() - t.space.lg).max(text_left + t.control_height);
        let text_max = Rect::from_min_max(pos2(text_left, inner_top), pos2(text_right, bottom_limit));
        let mut tui = ui.new_child(UiBuilder::new().max_rect(text_max).layout(Layout::top_down(Align::Min)).id_salt((salt, "text")));
        tui.spacing_mut().item_spacing.y = t.space.xxs;
        let wrap = text_max.width();
        let hl = (t.selection, t.selection_text);
        let job = |text: &str, range: Option<Range<usize>>, size: f32, color: Color32| text_job(text, range, size, color, hl, wrap);
        let rev = egui::util::hash((label, &hits.label, t.type_scale.body.to_bits(), t.text.primary, hl, wrap.to_bits()));
        let galley = crate::cache::galley(&tui, (salt, "label"), rev, || job(label, hits.label.clone(), t.type_scale.body, t.text.primary));
        let label_rect = tui.add(egui::Label::new(galley).selectable(false)).rect;
        if let Some(d) = description {
            let rev = egui::util::hash((d, &hits.description, t.type_scale.small.to_bits(), t.text.secondary, hl, wrap.to_bits()));
            let galley =
                crate::cache::galley(&tui, (salt, "description"), rev, || job(d, hits.description.clone(), t.type_scale.small, t.text.secondary));
            tui.add(egui::Label::new(galley).selectable(false));
        }
        let text_rect = tui.min_rect();

        let bottom = text_rect.bottom().max(ctl_rect.bottom()) + pad_y;
        let row_rect = Rect::from_min_max(pos2(left, top), pos2(right, bottom.max(top)));
        if ui.is_rect_visible(row_rect) {
            let painter = ui.painter();
            if !first {
                painter.hline((left + pad_x)..=(right - pad_x).max(left + pad_x), top, Stroke::new(1.0, t.separator));
            }
            if modified {
                let r = (pad_x * 0.2).clamp(2.0, 4.0);
                painter.circle_filled(pos2(left + pad_x * 0.5, label_rect.center().y), r, t.accent);
            }
        }
        // rows sit flush; the separator divides them
        let saved = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.allocate_rect(row_rect, Sense::hover());
        ui.spacing_mut().item_spacing.y = saved;
        if let Some(frame) = section.frame.as_mut() {
            frame.rows += 1;
            frame.rect.max.y = row_rect.bottom();
        }
        RowOutput { response: Some(response), reset }
    }
}

/// A row tied to a value (see [`Row::bind`]).
#[must_use = "a row is shown by `control`"]
pub struct Bound<'r, 's, 'l, T> {
    row: Row<'r, 's, 'l>,
    value: &'r mut T,
    default: T,
}

impl<T: PartialEq> Bound<'_, '_, '_, T> {
    /// Shows the row; `add` draws the control for the value and gets the row's automation id.
    pub fn control(self, add: impl FnOnce(&mut Ui, &str, &mut T) -> Response) -> RowOutput {
        let Bound { row, value, default } = self;
        let modified = *value != default;
        let mut out = row.modified(modified).control(|ui, id| add(ui, id, &mut *value));
        if out.reset {
            *value = default;
            if let Some(r) = out.response.as_mut() {
                r.mark_changed();
            }
        }
        out
    }
}

/// A labelled input: a required marker, a description and an error that is announced.
pub struct Field<'a> {
    id: &'a str,
    label: &'a str,
    description: Option<&'a str>,
    required: bool,
    error: Option<&'a str>,
    labels: &'a FormLabels,
}

impl<'a> Field<'a> {
    /// `id` is passed to the control; the error text is `<id>.error`.
    pub fn new(id: &'a str, label: &'a str) -> Self {
        Field { id, label, description: None, required: false, error: None, labels: default_labels() }
    }

    pub fn description(mut self, text: &'a str) -> Self {
        self.description = Some(text);
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// The validation error, shown under the control (replacing the description) and announced.
    pub fn error(mut self, error: Option<&'a str>) -> Self {
        self.error = error;
        self
    }

    pub fn labels(mut self, labels: &'a FormLabels) -> Self {
        self.labels = labels;
        self
    }

    /// Shows the field; `add` draws the control and gets the field id. Returns its response.
    pub fn show(self, ui: &mut Ui, add: impl FnOnce(&mut Ui, &str) -> Response) -> Response {
        let t = tokens::current(ui.ctx());
        let Field { id, label, description, required, error, labels } = self;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = t.space.xxs;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = t.space.xxs;
                ui.label(RichText::new(label).size(t.type_scale.body).color(t.text.primary));
                if required {
                    let r = ui.label(RichText::new("*").size(t.type_scale.body).color(t.status.danger));
                    ui.ctx().accesskit_node_builder(r.id, |n| n.set_label(labels.required.as_str()));
                }
            });
            let response = add(ui, id);
            let help = error.or(description);
            ui.ctx().accesskit_node_builder(response.id, |n| {
                n.set_label(label);
                if let Some(h) = help {
                    n.set_description(h);
                }
                if required {
                    n.set_required();
                }
                if error.is_some() {
                    n.set_invalid(Invalid::True);
                }
            });
            if let Some(e) = error {
                let r = ui.label(RichText::new(e).size(t.type_scale.small).color(t.status.danger));
                ui.ctx().accesskit_node_builder(r.id, |n| n.set_live(Live::Polite));
                automation::register_fmt(ui, r.rect, format_args!("{id}.error"), e, Role::Alert, true, Details { value: Some(e), selected: None });
            } else if let Some(d) = description {
                ui.label(RichText::new(d).size(t.type_scale.small).color(t.text.secondary));
            }
            response
        })
        .inner
    }
}

/// `text` with `range` drawn on the highlight colours.
fn text_job(text: &str, range: Option<Range<usize>>, size: f32, color: Color32, (hl_bg, hl_fg): (Color32, Color32), wrap: f32) -> LayoutJob {
    let plain = TextFormat::simple(Tokens::font(size), color);
    let mut job = LayoutJob::default();
    job.wrap.max_width = if wrap.is_finite() && wrap > 0.0 { wrap } else { f32::INFINITY };
    let parts = range.and_then(|r| Some((text.get(..r.start)?, text.get(r.clone())?, text.get(r.end..)?)));
    match parts {
        Some((before, hit, after)) => {
            job.append(before, 0.0, plain.clone());
            job.append(hit, 0.0, TextFormat { background: hl_bg, color: hl_fg, ..plain.clone() });
            job.append(after, 0.0, plain);
        }
        None => job.append(text, 0.0, plain),
    }
    job
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Byte range of the first case-insensitive occurrence of `needle` (already lower-cased).
fn find_ci(hay: &str, needle: &[char]) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    hay.char_indices().find_map(|(start, _)| {
        let mut rest = hay.get(start..)?.char_indices();
        let mut end = start;
        for &n in needle {
            let (off, c) = rest.next()?;
            if lower(c) != n {
                return None;
            }
            end = start + off + c.len_utf8();
        }
        Some(start..end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn needle(q: &str) -> Vec<char> {
        q.chars().map(lower).collect()
    }

    #[test]
    fn find_is_case_insensitive_and_returns_byte_ranges() {
        assert_eq!(find_ci("Launch at login", &needle("LOG")), Some(10..13));
        assert_eq!(find_ci("Größe ändern", &needle("ÄND")), Some(8..12));
        assert_eq!(find_ci("abc", &needle("abcd")), None);
        assert_eq!(find_ci("abc", &[]), None);
    }

    #[test]
    fn highlight_splits_the_label() {
        let job = text_job("Launch at login", Some(10..13), 13.0, Color32::WHITE, (Color32::BLUE, Color32::BLACK), 200.0);
        assert_eq!(job.sections.len(), 3);
        let bad = text_job("é", Some(0..1), 13.0, Color32::WHITE, (Color32::BLUE, Color32::BLACK), f32::NAN);
        assert_eq!(bad.sections.len(), 1);
    }
}
