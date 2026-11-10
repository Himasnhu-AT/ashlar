//! A sidebar (source list): titled sections that fold, each with a count, holding rows with an
//! icon, a label, a weak detail and a badge.
//!
//! The sidebar is one Tab stop. With it focused, Up and Down move through section headers and
//! rows (the selection follows onto rows), Home and End jump to the ends, Left folds a section
//! (or moves from a row to its header), Right unfolds it, Enter or Space on a header toggles it,
//! Enter on a row activates it, F2 renames it in place, and typing jumps to the next row whose
//! label starts with the typed text. A click selects a row, a double click activates it, a
//! secondary click reports it for a context menu. Only the visible rows are built, so a section
//! may hold any number of rows.
//!
//! The app owns the sections and keeps a [`SidebarState`] (selection, folded sections, the
//! rename in progress); the response says what the user asked for.
//!
//! ```
//! # use ashlar_ui::sidebar::{Sidebar, SidebarItem, SidebarSection, SidebarState};
//! # fn ui(ui: &mut egui::Ui, state: &mut SidebarState, names: &mut Vec<String>) {
//! let items: Vec<SidebarItem> = names.iter().map(|n| SidebarItem::new(n)).collect();
//! let sections = [SidebarSection::new("Projects", &items)];
//! let out = Sidebar::new("nav", "Navigation", state, &sections).show(ui);
//! if let Some((at, name)) = out.renamed {
//!     names[at.item] = name;
//! }
//! # }
//! ```
//!
//! Automation: `<id>` (the list, its value the selected row's label), `<id>.section.<s>` (a
//! header, `selected` when open, its value the count), `<id>.section.<s>.item.<i>` (a row) and
//! `<id>.rename` (the rename field). AccessKit: rows are list items with their selection state,
//! headers buttons with their expanded state and the count as a description.

use std::collections::HashSet;

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{FocusDirection, Key, Modifiers, Rect, Response, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::automation::{self, Details, Role};
use crate::controls::IconPainter;
use crate::list::Row;
use crate::tokens;

/// Sections beyond this many are not shown.
pub const MAX_SECTIONS: usize = 256;
/// Rows of one section beyond this many are not shown.
pub const MAX_ITEMS: usize = 1_000_000;
/// The longest name the rename field accepts, in characters.
pub const MAX_NAME: usize = 1_000;

/// One row.
#[derive(Clone, Copy)]
pub struct SidebarItem<'a> {
    pub label: &'a str,
    pub icon: Option<IconPainter<'a>>,
    /// Weak text at the right (a count, a date).
    pub detail: Option<&'a str>,
    /// A pill at the right (an unread count).
    pub badge: Option<&'a str>,
}

impl<'a> SidebarItem<'a> {
    pub fn new(label: &'a str) -> Self {
        SidebarItem { label, icon: None, detail: None, badge: None }
    }
    pub fn icon(mut self, paint: IconPainter<'a>) -> Self {
        self.icon = Some(paint);
        self
    }
    pub fn detail(mut self, text: &'a str) -> Self {
        self.detail = Some(text);
        self
    }
    pub fn badge(mut self, text: &'a str) -> Self {
        self.badge = Some(text);
        self
    }
}

/// A titled group of rows.
#[derive(Clone, Copy)]
pub struct SidebarSection<'a> {
    pub title: &'a str,
    pub items: &'a [SidebarItem<'a>],
    /// The number shown next to the title (default: the number of rows).
    pub count: Option<usize>,
}

impl<'a> SidebarSection<'a> {
    pub fn new(title: &'a str, items: &'a [SidebarItem<'a>]) -> Self {
        SidebarSection { title, items, count: None }
    }
    /// Show this number next to the title instead of the number of rows (unread items, say).
    pub fn count(mut self, n: usize) -> Self {
        self.count = Some(n);
        self
    }
    fn len(&self) -> usize {
        self.items.len().min(MAX_ITEMS)
    }
}

/// A row: its section and its index within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ItemRef {
    pub section: usize,
    pub item: usize,
}

/// Where the keyboard is: a section header or a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cursor {
    Header(usize),
    Item(ItemRef),
}

/// What the sidebar keeps between frames. Sections are folded by index.
#[derive(Clone, Debug, Default)]
pub struct SidebarState {
    /// The selected row.
    pub selected: Option<ItemRef>,
    collapsed: HashSet<usize>,
    cursor: Option<Cursor>,
    /// The row being renamed, the text typed so far, and whether the field took the focus.
    rename: Option<(ItemRef, String, bool)>,
}

impl SidebarState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_collapsed(&self, section: usize) -> bool {
        self.collapsed.contains(&section)
    }
    pub fn set_collapsed(&mut self, section: usize, collapsed: bool) {
        if collapsed {
            self.collapsed.insert(section);
        } else {
            self.collapsed.remove(&section);
        }
    }
    /// Start renaming `item`, its field filled with `current`.
    pub fn start_rename(&mut self, item: ItemRef, current: &str) {
        self.rename = Some((item, current.chars().take(MAX_NAME).collect(), false));
        self.cursor = Some(Cursor::Item(item));
    }
    /// The row being renamed.
    pub fn renaming(&self) -> Option<ItemRef> {
        self.rename.as_ref().map(|r| r.0)
    }
    pub fn cancel_rename(&mut self) {
        self.rename = None;
    }
}

/// Words the sidebar shows or speaks (localized by the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarLabels {
    /// The rename field's accessible name.
    pub rename: String,
    /// A header's count for screen readers; `{n}` is the number.
    pub items: String,
}

impl Default for SidebarLabels {
    fn default() -> Self {
        SidebarLabels { rename: "Rename".into(), items: "{n} items".into() }
    }
}

/// What [`Sidebar::show`] reports.
pub struct SidebarResponse {
    /// The sidebar's single Tab stop.
    pub response: Response,
    /// The user moved the selection here (click, keys, type-ahead); already in the state.
    pub select: Option<ItemRef>,
    /// Enter or a double click on a row.
    pub activated: Option<ItemRef>,
    /// A secondary click on a row (for a context menu).
    pub context: Option<ItemRef>,
    /// A section folded or unfolded.
    pub toggled: Option<usize>,
    /// A rename finished with this name (trimmed, not empty, changed).
    pub renamed: Option<(ItemRef, String)>,
}

/// The sidebar. See the [module docs](self).
pub struct Sidebar<'a> {
    id: &'a str,
    label: &'a str,
    state: &'a mut SidebarState,
    sections: &'a [SidebarSection<'a>],
    labels: Option<&'a SidebarLabels>,
    max_height: Option<f32>,
}

/// Row index of each section's header, then the total row count.
fn layout(sections: &[SidebarSection<'_>], state: &SidebarState) -> Vec<usize> {
    let mut starts = Vec::with_capacity(sections.len() + 1);
    let mut n = 0usize;
    for (s, sec) in sections.iter().enumerate() {
        starts.push(n);
        n = n.saturating_add(1).saturating_add(if state.is_collapsed(s) { 0 } else { sec.len() });
    }
    starts.push(n);
    starts
}

fn at(starts: &[usize], row: usize) -> Option<Cursor> {
    let s = starts.partition_point(|&st| st <= row).checked_sub(1)?;
    let start = *starts.get(s)?;
    let end = *starts.get(s + 1)?;
    if row >= end {
        return None;
    }
    Some(if row == start { Cursor::Header(s) } else { Cursor::Item(ItemRef { section: s, item: row - start - 1 }) })
}

fn row_of(starts: &[usize], state: &SidebarState, c: Cursor) -> Option<usize> {
    match c {
        Cursor::Header(s) => starts.get(s).copied().filter(|_| s + 1 < starts.len()),
        Cursor::Item(r) => {
            let start = *starts.get(r.section)?;
            let end = *starts.get(r.section + 1)?;
            let row = start.checked_add(1)?.checked_add(r.item)?;
            (!state.is_collapsed(r.section) && row < end).then_some(row)
        }
    }
}

impl<'a> Sidebar<'a> {
    /// `label` names the sidebar for screen readers and automation.
    pub fn new(id: &'a str, label: &'a str, state: &'a mut SidebarState, sections: &'a [SidebarSection<'a>]) -> Self {
        let sections = sections.get(..sections.len().min(MAX_SECTIONS)).unwrap_or_default();
        Sidebar { id, label, state, sections, labels: None, max_height: None }
    }
    /// The words to use (default: the installed [`crate::labels`]).
    pub fn labels(mut self, labels: &'a SidebarLabels) -> Self {
        self.labels = Some(labels);
        self
    }
    /// The tallest the sidebar grows (default: the available height).
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = h.is_finite().then(|| h.max(0.0));
        self
    }

    pub fn show(self, ui: &mut Ui) -> SidebarResponse {
        let Sidebar { id, label, state, sections, labels, max_height } = self;
        let t = tokens::current(ui.ctx());
        let installed = crate::labels::current(ui.ctx());
        let labels = labels.unwrap_or(&installed.sidebar);
        let base = ui.make_persistent_id(("ashlar_ui::sidebar", id));
        let row_h = t.control_height;
        let mut out = Report::default();

        // drop references to rows that no longer exist
        let exists = |r: ItemRef| sections.get(r.section).is_some_and(|s| r.item < s.len());
        state.selected = state.selected.filter(|r| exists(*r));
        if state.rename.as_ref().is_some_and(|(r, ..)| !exists(*r)) {
            state.rename = None;
        }
        state.cursor = state.cursor.filter(|c| match c {
            Cursor::Header(s) => *s < sections.len(),
            Cursor::Item(r) => exists(*r),
        });

        let mut starts = layout(sections, state);
        let total = starts.last().copied().unwrap_or(0);
        // a click asked for the focus last frame (asked during the click, egui would hand it
        // back: the click landed on a row, not on the sidebar's own widget)
        let grab = base.with("grab");
        if ui.data_mut(|d| d.remove_temp::<bool>(grab)).unwrap_or(false) {
            ui.memory_mut(|m| m.request_focus(base));
        }
        let focused = ui.memory(|m| m.has_focus(base));
        let mut scroll_to = None;

        // keyboard, while the sidebar has the focus
        if focused && total > 0 && state.rename.is_none() {
            let cur = state.cursor.or(state.selected.map(Cursor::Item)).and_then(|c| row_of(&starts, state, c));
            let keys = ui.input_mut(|i| {
                let mut k = |key| i.consume_key(Modifiers::NONE, key);
                [
                    k(Key::ArrowUp),
                    k(Key::ArrowDown),
                    k(Key::Home),
                    k(Key::End),
                    k(Key::ArrowLeft),
                    k(Key::ArrowRight),
                    k(Key::Enter),
                    k(Key::Space),
                    k(Key::F2),
                ]
            });
            let [up, down, home, end, left, right, enter, space, f2] = keys;
            let mut target = None;
            if up {
                target = Some(cur.map_or(0, |c| c.saturating_sub(1)));
            } else if down {
                target = Some(cur.map_or(0, |c| (c + 1).min(total - 1)));
            } else if home {
                target = Some(0);
            } else if end {
                target = Some(total - 1);
            }
            let here = cur.and_then(|c| at(&starts, c));
            match here {
                Some(Cursor::Header(s)) => {
                    let open = !state.is_collapsed(s);
                    if (left && open) || (right && !open) || enter || space {
                        state.set_collapsed(s, open);
                        out.toggled = Some(s);
                    } else if right && open {
                        target = cur.map(|c| (c + 1).min(total - 1));
                    }
                }
                Some(Cursor::Item(r)) => {
                    if left {
                        target = starts.get(r.section).copied();
                    } else if enter {
                        out.activated = Some(r);
                    } else if f2 && let Some(item) = sections.get(r.section).and_then(|s| s.items.get(r.item)) {
                        state.start_rename(r, item.label);
                        scroll_to = cur;
                    }
                }
                None => {}
            }
            // type-ahead over the row labels (headers do not match)
            if target.is_none() {
                let text = |row: usize| match at(&starts, row) {
                    Some(Cursor::Item(r)) => sections.get(r.section).and_then(|s| s.items.get(r.item)).map_or("", |i| i.label),
                    _ => "",
                };
                target = crate::virtual_list::type_ahead(ui, base, total, cur, &text);
            }
            if let Some(row) = target
                && let Some(c) = at(&starts, row)
            {
                state.cursor = Some(c);
                if let Cursor::Item(r) = c
                    && state.selected != Some(r)
                {
                    state.selected = Some(r);
                    out.select = Some(r);
                }
                scroll_to = Some(row);
            }
            if keys.iter().any(|k| *k) {
                // the arrows also asked egui to move the focus spatially: keep it here
                ui.memory_mut(|m| m.move_focus(FocusDirection::None));
            }
            if out.toggled.is_some() {
                starts = layout(sections, state);
            }
        }
        let total = starts.last().copied().unwrap_or(0);

        // keep the cursor row in view
        let offset_id = base.with("view");
        let (last_offset, last_view): (f32, f32) = ui.data(|d| d.get_temp(offset_id)).unwrap_or((0.0, 0.0));
        let mut area = egui::ScrollArea::vertical().id_salt(base.with("scroll")).auto_shrink([false, true]);
        if let Some(h) = max_height {
            area = area.max_height(h);
        }
        if let Some(row) = scroll_to {
            let top = row as f32 * row_h;
            let mut y = last_offset;
            if top < y {
                y = top;
            } else if top + row_h > y + last_view && last_view > 0.0 {
                y = top + row_h - last_view;
            }
            if y != last_offset {
                area = area.vertical_scroll_offset(y.max(0.0));
            }
        }

        let cursor_row = state.cursor.and_then(|c| row_of(&starts, state, c));
        let mut toggle = None;
        let mut rename_done: Option<bool> = None;
        // the single Tab stop, sensed before the rows so they nest under it for screen readers
        // (its rect is last frame's viewport; it takes no pointer input, the rows do)
        let rect_id = base.with("rect");
        let last_rect: Option<Rect> = ui.data(|d| d.get_temp(rect_id));
        let guess = ui.available_rect_before_wrap();
        let guess = Rect::from_min_size(guess.min, vec2(guess.width(), guess.height().min(max_height.unwrap_or(f32::INFINITY))));
        let response = ui.interact(last_rect.unwrap_or(guess), base, Sense::focusable_noninteractive());
        let scrolled = ui
            .scope_builder(egui::UiBuilder::new().accessibility_parent(base).layout(egui::Layout::top_down_justified(egui::Align::Min)), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                area.show_rows(ui, row_h, total, |ui, range| {
                    for row in range {
                        let Some(c) = at(&starts, row) else { continue };
                        match c {
                            Cursor::Header(s) => {
                                let Some(sec) = sections.get(s) else { continue };
                                let open = !state.is_collapsed(s);
                                let count = sec.count.unwrap_or(sec.len()).to_string();
                                let chevron = move |p: &egui::Painter, r: Rect, col: egui::Color32| {
                                    let (c, side) = (r.center(), r.width() * 0.6);
                                    p.add(egui::Shape::convex_polygon(crate::content::chevron_points(c, side, open), col, Stroke::NONE));
                                };
                                let resp = ui.add(
                                    Row::new(("ashlar_ui::sidebar::header", s), sec.title)
                                        .height(row_h)
                                        .weak(true)
                                        .text_style(egui::TextStyle::Small)
                                        .icon(&chevron)
                                        .detail(&count)
                                        .focusable(false),
                                );
                                let spoken = labels.items.replace("{n}", &count);
                                ui.ctx().accesskit_node_builder(resp.id, |n| {
                                    n.set_role(egui::accesskit::Role::Button);
                                    n.set_expanded(open);
                                    n.clear_selected();
                                    n.set_description(spoken.as_str());
                                });
                                automation::register_fmt(
                                    ui,
                                    resp.rect,
                                    format_args!("{id}.section.{s}"),
                                    sec.title,
                                    Role::Disclosure,
                                    true,
                                    Details { value: Some(&count), selected: Some(open) },
                                );
                                if resp.clicked() {
                                    toggle = Some(s);
                                    state.cursor = Some(c);
                                    take_focus(ui, grab);
                                }
                                if focused && cursor_row == Some(row) {
                                    ui.painter().rect_stroke(
                                        resp.rect.shrink(1.0),
                                        t.radius.sm,
                                        Stroke::new(crate::controls::FOCUS_WIDTH, t.focus),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                            Cursor::Item(r) => {
                                let Some(item) = sections.get(r.section).and_then(|s| s.items.get(r.item)) else { continue };
                                let selected = state.selected == Some(r);
                                if let Some((_, text, focused_once)) = state.rename.as_mut().filter(|(e, ..)| *e == r) {
                                    let field_id = base.with("rename");
                                    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), row_h));
                                    let edit = egui::TextEdit::singleline(text).id(field_id).char_limit(MAX_NAME).desired_width(rect.width());
                                    let resp = ui.put(rect.shrink2(vec2(2.0, 1.0)), edit);
                                    automation::register_with(
                                        ui,
                                        &resp,
                                        &format!("{id}.rename"),
                                        &labels.rename,
                                        Role::Field,
                                        Details { value: Some(text.as_str()), selected: None },
                                    );
                                    ui.ctx().accesskit_node_builder(resp.id, |n| n.set_label(labels.rename.as_str()));
                                    if !*focused_once {
                                        // the whole name is selected: typing replaces it
                                        let mut st = egui::text_edit::TextEditState::load(ui.ctx(), field_id).unwrap_or_default();
                                        st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(text.chars().count()))));
                                        st.store(ui.ctx(), field_id);
                                        resp.request_focus();
                                        *focused_once = true;
                                    } else if resp.lost_focus() {
                                        let cancel = ui.input(|i| i.key_pressed(Key::Escape));
                                        rename_done = Some(!cancel);
                                    }
                                    continue;
                                }
                                let mut row_w = Row::new(("ashlar_ui::sidebar::item", r.section, r.item), item.label)
                                    .height(row_h)
                                    .selected(selected)
                                    .indent(t.space.md)
                                    .focusable(false);
                                if let Some(icon) = item.icon {
                                    row_w = row_w.icon(icon);
                                }
                                if let Some(d) = item.detail {
                                    row_w = row_w.detail(d);
                                }
                                if let Some(b) = item.badge {
                                    row_w = row_w.badge(b);
                                }
                                let resp = ui.add(row_w);
                                automation::register_fmt(
                                    ui,
                                    resp.rect,
                                    format_args!("{id}.section.{}.item.{}", r.section, r.item),
                                    item.label,
                                    Role::Row,
                                    true,
                                    Details { value: item.detail, selected: Some(selected) },
                                );
                                if resp.clicked() || resp.secondary_clicked() || resp.double_clicked() {
                                    state.cursor = Some(c);
                                    take_focus(ui, grab);
                                    if state.selected != Some(r) {
                                        state.selected = Some(r);
                                        out.select = Some(r);
                                    }
                                }
                                if resp.double_clicked() {
                                    out.activated = Some(r);
                                }
                                if resp.secondary_clicked() {
                                    out.context = Some(r);
                                }
                                if focused && cursor_row == Some(row) {
                                    ui.painter().rect_stroke(
                                        resp.rect.shrink(1.0),
                                        t.radius.sm,
                                        Stroke::new(crate::controls::FOCUS_WIDTH, t.focus),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                        }
                    }
                })
            })
            .inner;
        ui.data_mut(|d| d.insert_temp(offset_id, (scrolled.state.offset.y, scrolled.inner_rect.height())));

        if let Some(s) = toggle {
            let open = !state.is_collapsed(s);
            state.set_collapsed(s, open);
            out.toggled = Some(s);
            crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
        }
        if let Some(commit) = rename_done
            && let Some((r, text, _)) = state.rename.take()
        {
            let name = text.trim();
            let old = sections.get(r.section).and_then(|s| s.items.get(r.item)).map_or("", |i| i.label);
            if commit && !name.is_empty() && name != old {
                out.renamed = Some((r, name.to_owned()));
            }
            ui.memory_mut(|m| m.request_focus(base));
            crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
        }

        if last_rect != Some(scrolled.inner_rect) {
            ui.data_mut(|d| d.insert_temp(rect_id, scrolled.inner_rect));
            if last_rect.is_some() {
                crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
            }
        }
        let value = state.selected.and_then(|r| sections.get(r.section).and_then(|s| s.items.get(r.item))).map_or("", |i| i.label);
        ui.ctx().accesskit_node_builder(response.id, |n| {
            n.set_role(egui::accesskit::Role::List);
            n.set_label(label);
        });
        automation::register_fmt(
            ui,
            scrolled.inner_rect,
            format_args!("{id}"),
            label,
            Role::List,
            true,
            Details { value: Some(value), selected: None },
        );
        let Report { select, activated, context, toggled, renamed } = out;
        SidebarResponse { response, select, activated, context, toggled, renamed }
    }
}

/// What the user asked for this frame, gathered before the response exists.
#[derive(Default)]
struct Report {
    select: Option<ItemRef>,
    activated: Option<ItemRef>,
    context: Option<ItemRef>,
    toggled: Option<usize>,
    renamed: Option<(ItemRef, String)>,
}

/// Take the focus on the next frame (see `grab` in [`Sidebar::show`]).
fn take_focus(ui: &Ui, grab: egui::Id) {
    ui.data_mut(|d| d.insert_temp(grab, true));
    crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
}
