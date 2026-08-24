//! Command palette: type to fuzzy-find any command, see its shortcut, run it with Enter.
//!
//! Disabled commands stay listed (dimmed, with the reason they are unavailable) so the user learns
//! why, but cannot be run. Results are ranked again only when the query or the commands'
//! revision changes. Escape or a click outside closes the palette.
//!
//! ```
//! # use ashlar_ui::commands::palette::CommandPalette;
//! # use ashlar_command::{CommandInfo, shortcut::Platform};
//! # fn frame(ctx: &egui::Context, palette: &mut CommandPalette, commands: &[CommandInfo], revision: u64) {
//! if let Some(id) = palette.show(ctx, commands, revision, Platform::current()) {
//!     // registry.execute(&id, json!({}))
//! }
//! # }
//! ```

use ashlar_command::shortcut::{Platform, display as display_shortcut};
use ashlar_command::{CommandInfo, fuzzy_match};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Id, Key, Modifiers, Rect, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::search::SearchField;
use crate::tokens::{self, Tokens};
use crate::virtual_list::{RowHeights, VirtualList};

#[derive(Clone, Debug)]
struct Hit {
    index: usize,
    score: i32,
    positions: Vec<usize>,
}

/// Palette state (keep one per window).
pub struct CommandPalette {
    open: bool,
    query: String,
    cursor: usize,
    hits: Vec<Hit>,
    key: Option<(u64, u64, usize)>,
    focus_pending: bool,
    max_results: usize,
    hint: String,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        CommandPalette {
            open: false,
            query: String::new(),
            cursor: 0,
            hits: Vec::new(),
            key: None,
            focus_pending: false,
            max_results: 200,
            hint: "Type a command".into(),
        }
    }
    /// Placeholder text (localized by the app).
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn open(&mut self) {
        self.open = true;
        self.focus_pending = true;
        self.query.clear();
        self.cursor = 0;
        self.key = None;
    }
    pub fn close(&mut self) {
        self.open = false;
    }
    pub fn toggle(&mut self) {
        if self.open { self.close() } else { self.open() }
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    /// Ids of the current results, best first.
    pub fn results<'c>(&self, commands: &'c [CommandInfo]) -> Vec<&'c str> {
        self.hits.iter().filter_map(|h| commands.get(h.index)).map(|c| c.id.as_str()).collect()
    }

    /// Rank `commands` for the current query (only when the query or `revision` changed).
    pub fn rank(&mut self, commands: &[CommandInfo], revision: u64) {
        let key = (cache::hash_of(&self.query), revision, commands.len());
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.hits.clear();
        let query = self.query.trim();
        for (index, c) in commands.iter().enumerate() {
            if query.is_empty() {
                self.hits.push(Hit { index, score: 0, positions: Vec::new() });
            } else if let Some(m) = fuzzy_match(query, &c.label) {
                self.hits.push(Hit { index, score: m.score, positions: m.positions });
            } else if let Some(m) = fuzzy_match(query, &c.id) {
                // id matches rank below label matches and highlight nothing
                self.hits.push(Hit { index, score: m.score - 1000, positions: Vec::new() });
            }
        }
        // best first; ties keep declaration order; enabled before disabled at equal score
        self.hits.sort_by_key(|h| (std::cmp::Reverse(h.score), commands.get(h.index).is_none_or(|c| !c.enabled), h.index));
        self.hits.truncate(self.max_results);
        self.cursor = 0;
    }

    /// Show the palette when open; returns the command to run.
    pub fn show(&mut self, ctx: &egui::Context, commands: &[CommandInfo], revision: u64, platform: Platform) -> Option<String> {
        if !self.open {
            return None;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.close();
            return None;
        }
        let t = tokens::current(ctx);
        let (up, down, enter) = ctx.input_mut(|i| {
            (i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::Enter))
        });
        let id = Id::new("ashlar_ui::palette");
        let width = (ctx.content_rect().width() - 32.0).clamp(200.0, 560.0);
        let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 72.0)).default_width(width);
        let frame = egui::Frame::new()
            .fill(t.surface.raised)
            .stroke(egui::Stroke::new(1.0, t.border))
            .corner_radius(t.radius.lg)
            .inner_margin(egui::Margin::same(t.space.md as i8))
            .shadow(ctx.global_style().visuals.popup_shadow);
        let mut run = None;
        let modal = egui::Modal::new(id).area(area).frame(frame).show(ctx, |ui| {
            ui.set_width(width);
            // keep asking until the field has it (the modal's first pass is a hidden sizing pass)
            if self.focus_pending {
                SearchField::focus(ui, "palette.search");
            }
            let search = SearchField::new("palette.search", &mut self.query).hint(&self.hint).show(ui);
            if search.response.has_focus() {
                self.focus_pending = false;
            } else if search.response.lost_focus() && !enter {
                search.response.request_focus();
            }
            self.rank(commands, revision);
            let n = self.hits.len();
            if n > 0 {
                if up {
                    self.cursor = self.cursor.checked_sub(1).unwrap_or(n - 1);
                }
                if down {
                    self.cursor = (self.cursor + 1) % n;
                }
                self.cursor = self.cursor.min(n - 1);
            }
            if enter
                && let Some(c) = self.hits.get(self.cursor).and_then(|h| commands.get(h.index))
                && c.enabled
            {
                run = Some(c.id.clone());
            }
            ui.add_space(t.space.sm);
            let row_h = t.control_height + 4.0;
            let hits = &self.hits;
            let label = |i: usize| hits.get(i).and_then(|h| commands.get(h.index)).map_or("", |c| c.label.as_str());
            let query_key = cache::hash_of(&self.query);
            let list = VirtualList::new("palette.results", n, RowHeights::Uniform(row_h))
                .selected(Some(self.cursor))
                .scroll_to((up || down).then_some(self.cursor))
                .max_height(row_h * 10.0)
                .row_label(&label)
                .show(ui, |ui, row| {
                    let Some(hit) = hits.get(row.index) else { return };
                    let Some(c) = commands.get(hit.index) else { return };
                    paint_row(ui, &t, row.rect, c, hit, query_key, row.selected, platform);
                });
            if let Some(i) = list.clicked
                && let Some(c) = hits.get(i).and_then(|h| commands.get(h.index))
            {
                self.cursor = i;
                if c.enabled {
                    run = Some(c.id.clone());
                }
            }
            if n == 0 {
                ui.label(egui::RichText::new("No matching commands").color(t.text.secondary));
            }
            automation::register_fmt(
                ui,
                ui.min_rect(),
                format_args!("palette"),
                "Command palette",
                Role::Palette,
                true,
                Details { value: Some(&self.query), selected: None },
            );
        });
        if run.is_some() || modal.backdrop_response.clicked() {
            self.close();
        }
        run
    }
}

fn paint_row(ui: &mut egui::Ui, t: &Tokens, rect: Rect, c: &CommandInfo, hit: &Hit, query_key: u64, selected: bool, platform: Platform) {
    let font = Tokens::font(t.type_scale.body);
    let base = if !c.enabled {
        t.text.disabled
    } else if selected {
        t.selection_text
    } else {
        t.text.primary
    };
    let strong = if c.enabled && !selected { t.accent } else { base };
    let revision = cache::hash_of(&(query_key, base, strong, font.size.to_bits(), &c.label, &c.menu));
    let galley = cache::galley(ui, ("ashlar_ui::palette-row", &c.id), revision, || {
        let mut job = LayoutJob::default();
        let plain = TextFormat::simple(font.clone(), base);
        let hl = TextFormat { underline: egui::Stroke::new(1.0, strong), ..TextFormat::simple(font.clone(), strong) };
        let mut buf = [0u8; 4];
        for (k, ch) in c.label.chars().enumerate() {
            let fmt = if hit.positions.binary_search(&k).is_ok() { hl.clone() } else { plain.clone() };
            job.append(ch.encode_utf8(&mut buf), 0.0, fmt);
        }
        if !c.menu.is_empty() {
            let path = TextFormat::simple(Tokens::font(t.type_scale.small), if selected { base } else { t.text.secondary });
            job.append(&c.menu.join(" › "), t.space.md, path);
        }
        job
    });
    let right = if c.enabled {
        c.shortcut.as_deref().and_then(|s| display_shortcut(s, platform))
    } else {
        Some(c.disabled_reason.clone().unwrap_or_else(|| "Unavailable".into()))
    };
    let pad = t.space.sm;
    let text_pos = pos2(rect.left() + pad, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, base);
    if let Some(r) = right {
        let color = if selected { t.selection_text } else { t.text.secondary };
        let g = cache::galley(ui, ("ashlar_ui::palette-right", &c.id), cache::hash_of(&(&r, color)), || {
            LayoutJob::simple_singleline(r.clone(), Tokens::font(t.type_scale.small), color)
        });
        let area = Rect::from_min_max(rect.min, pos2(rect.right() - pad, rect.bottom()));
        ui.painter().galley(Align2::RIGHT_CENTER.align_size_within_rect(g.size(), area).min, g, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::menu::tests::info;
    use egui_kittest::Harness;

    fn commands() -> Vec<CommandInfo> {
        vec![
            info("file.export", "Export As…", &["File"], Some("Mod+Shift+E"), true),
            info("edit.paste", "Paste", &["Edit"], Some("Mod+V"), false),
            info("file.exit", "Exit", &["File"], None, true),
            info("view.zoom_in", "Zoom In", &["View"], Some("Mod+="), true),
        ]
    }

    #[test]
    fn ranking_is_fuzzy_cached_and_puts_label_matches_first() {
        let cmds = commands();
        let mut p = CommandPalette::new();
        p.rank(&cmds, 1);
        assert_eq!(p.results(&cmds).len(), 4);
        p.query = "ex".into();
        p.rank(&cmds, 1);
        let r = p.results(&cmds);
        let mut top = r[..2].to_vec();
        top.sort_unstable();
        assert_eq!(top, ["file.exit", "file.export"]);
        p.query = "zoomin".into();
        p.rank(&cmds, 1);
        assert_eq!(p.results(&cmds), ["view.zoom_in"]);
        p.query = "view.z".into();
        p.rank(&cmds, 1);
        assert_eq!(p.results(&cmds), ["view.zoom_in"], "matches the id when the label does not");
        p.query = "qqq".into();
        p.rank(&cmds, 1);
        assert!(p.results(&cmds).is_empty());
    }

    struct S {
        palette: CommandPalette,
        cmds: Vec<CommandInfo>,
        ran: Vec<String>,
    }

    fn harness() -> Harness<'static, S> {
        let mut h = Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, s: &mut S| {
                if let Some(id) = s.palette.show(ui.ctx(), &s.cmds, 1, Platform::Other) {
                    s.ran.push(id);
                }
            },
            S { palette: CommandPalette::new(), cmds: commands(), ran: Vec::new() },
        );
        automation::install(&h.ctx);
        h.state_mut().palette.open();
        h.step();
        h.step();
        h
    }

    #[test]
    fn type_arrow_enter_runs_and_closes() {
        let mut h = harness();
        h.event(egui::Event::Text("zo".into()));
        h.step();
        h.step();
        assert_eq!(h.state().palette.query(), "zo");
        let row = automation::find(&h.ctx, "palette.results.row.0").unwrap();
        assert_eq!(row.label, "Zoom In");
        h.key_press(Key::Enter);
        h.step();
        assert_eq!(h.state().ran, ["view.zoom_in"]);
        assert!(!h.state().palette.is_open());
    }

    #[test]
    fn disabled_commands_are_listed_but_do_not_run_and_escape_closes() {
        let mut h = harness();
        h.event(egui::Event::Text("paste".into()));
        h.step();
        h.step();
        assert_eq!(automation::find(&h.ctx, "palette.results.row.0").map(|e| e.label), Some("Paste".into()));
        h.key_press(Key::Enter);
        h.step();
        assert!(h.state().ran.is_empty());
        assert!(h.state().palette.is_open());
        h.key_press(Key::Escape);
        h.step();
        assert!(!h.state().palette.is_open());
    }

    #[test]
    fn arrows_wrap_and_click_runs() {
        let mut h = harness();
        h.key_press(Key::ArrowUp);
        h.step();
        h.step();
        assert_eq!(automation::find(&h.ctx, "palette.results.row.3").and_then(|e| e.selected), Some(true));
        let row = automation::find(&h.ctx, "palette.results.row.0").unwrap();
        crate::test_support::click(&mut h, row.center());
        assert_eq!(h.state().ran, ["file.export"]);
    }
}
