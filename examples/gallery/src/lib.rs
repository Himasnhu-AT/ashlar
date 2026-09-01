//! The ashlar component gallery: every `ashlar-ui` component in its states, in light and dark.
//!
//! `cargo run -p ashlar-example-gallery` opens it; `-- --snapshot <dir>` renders every page in
//! both themes to PNG files without a window. The tests drive the same [`Gallery`] headlessly.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::time::Duration;

use ashlar::command::CommandInfo;
use ashlar::command::Keymap;
use ashlar::command::shortcut::Platform;
use ashlar::ui::commands::{CommandPalette, KeymapEditor, MenuSpec, dispatch, menu_bar};
use ashlar::ui::dialog::{Dialog, DialogButton, DialogOutcome};
use ashlar::ui::edit::{Edit, EditPhase};
use ashlar::ui::search::SearchField;
use ashlar::ui::slider::{Knob, Slider};
use ashlar::ui::status_bar::{self, StatusBar};
use ashlar::ui::swatches::SwatchGrid;
use ashlar::ui::toasts::{Toast, ToastLevel, Toasts};
use ashlar::ui::tokens::{self, ThemeMode, Tokens};
use ashlar::ui::tree::{TreeModel, TreeState, TreeView};
use ashlar::ui::value_field::{Decimal, FnFormat, ValueField};
use ashlar::ui::virtual_list::{Column, RowHeights, Sort, VirtualList, VirtualTable};
use egui::{Color32, Ui};

/// The gallery's pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Inputs,
    Lists,
    Overlays,
    Commands,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Inputs, Page::Lists, Page::Overlays, Page::Commands];
    pub fn name(self) -> &'static str {
        match self {
            Page::Inputs => "Inputs",
            Page::Lists => "Lists",
            Page::Overlays => "Overlays",
            Page::Commands => "Commands",
        }
    }
}

/// Rows in the list and table demos.
pub const DEMO_ROWS: usize = 100_000;

/// A generated outline: 40 chapters × 25 sections × 10 paragraphs.
pub struct Outline {
    labels: Vec<String>,
}

impl Outline {
    fn new() -> Self {
        let mut labels = Vec::with_capacity(10_041);
        for id in 0..(1 + 40 + 40 * 25 + 40 * 25 * 10) {
            labels.push(Self::make_label(id));
        }
        Outline { labels }
    }
    fn make_label(id: u64) -> String {
        match id {
            0 => "Outline".into(),
            1..=40 => format!("Chapter {id}"),
            41..=1040 => format!("Section {}.{}", (id - 41) / 25 + 1, (id - 41) % 25 + 1),
            _ => format!("Paragraph {}", id - 1040),
        }
    }
}

impl TreeModel for Outline {
    fn children(&self, parent: Option<u64>, out: &mut Vec<u64>) {
        match parent {
            None => out.extend(1..=40),
            Some(c @ 1..=40) => out.extend((0..25).map(|s| 41 + (c - 1) * 25 + s)),
            Some(s @ 41..=1040) => out.extend((0..10).map(|p| 1041 + (s - 41) * 10 + p)),
            Some(_) => {}
        }
    }
    fn has_children(&self, node: u64) -> bool {
        node <= 1040
    }
    fn label(&self, node: u64) -> &str {
        self.labels.get(node as usize).map_or("", String::as_str)
    }
}

fn cmd(id: &str, label: &str, menu: &[&str], shortcut: Option<&str>, disabled: Option<&str>) -> CommandInfo {
    CommandInfo {
        id: id.into(),
        label: label.into(),
        menu: menu.iter().map(|s| (*s).to_owned()).collect(),
        shortcut: shortcut.map(Into::into),
        params_schema: serde_json::Value::Null,
        params_text: None,
        enabled: disabled.is_none(),
        disabled_reason: disabled.map(Into::into),
        mutates: false,
    }
}

/// The demo command table.
pub fn demo_commands() -> Vec<CommandInfo> {
    vec![
        cmd("file.new", "New", &["File"], Some("Mod+N"), None),
        cmd("file.open", "Open…", &["File"], Some("Mod+O"), None),
        cmd("file.save", "Save", &["File"], Some("Mod+S"), None),
        cmd("file.export.png", "PNG Image…", &["File", "Export"], None, None),
        cmd("file.export.pdf", "PDF Document…", &["File", "Export"], Some("Mod+Shift+E"), None),
        cmd("edit.undo", "Undo", &["Edit"], Some("Mod+Z"), Some("Nothing to undo")),
        cmd("edit.redo", "Redo", &["Edit"], Some("Mod+Shift+Z"), Some("Nothing to redo")),
        cmd("edit.find", "Find…", &["Edit"], Some("Mod+F"), None),
        cmd("view.grid", "Show Grid", &["View"], Some("Mod+'"), None),
        cmd("view.theme.light", "Light Theme", &["View", "Theme"], None, None),
        cmd("view.theme.dark", "Dark Theme", &["View", "Theme"], None, None),
        cmd("view.theme.system", "Follow System", &["View", "Theme"], None, None),
        cmd("help.palette", "Command Palette…", &["Help"], Some("Mod+Shift+P"), None),
        cmd("help.toast", "Show a Notification", &["Help"], None, None),
        cmd("help.save_as", "Save As (conflicting key)", &[], Some("Mod+S"), None),
    ]
}

/// Everything the gallery shows, with its demo state.
pub struct Gallery {
    pub page: Page,
    pub mode: ThemeMode,
    pub scale: f64,
    installed: bool,
    // inputs
    pub opacity: f64,
    pub duration: f64,
    pub gain: f64,
    pub pan: f64,
    pub swatch: Option<usize>,
    pub query: String,
    pub last_edit: Option<Edit>,
    edits: u64,
    // lists
    pub list_selected: Option<usize>,
    pub table_selected: Option<usize>,
    pub sort: Sort,
    pub tree: TreeState,
    outline: Outline,
    // overlays
    pub dialog_open: bool,
    pub toasts: Toasts,
    pub last_dialog: Option<DialogOutcome>,
    // commands
    pub commands: Vec<CommandInfo>,
    pub keymap: Keymap,
    pub palette: CommandPalette,
    pub keymap_editor: KeymapEditor,
    pub ran: Vec<String>,
    pub show_grid: bool,
    revision: u64,
}

impl Default for Gallery {
    fn default() -> Self {
        Self::new()
    }
}

const SWATCHES: [Color32; 16] = [
    Color32::from_rgb(0xef, 0x44, 0x44),
    Color32::from_rgb(0xf9, 0x73, 0x16),
    Color32::from_rgb(0xf5, 0x9e, 0x0b),
    Color32::from_rgb(0xea, 0xb3, 0x08),
    Color32::from_rgb(0x84, 0xcc, 0x16),
    Color32::from_rgb(0x22, 0xc5, 0x5e),
    Color32::from_rgb(0x10, 0xb9, 0x81),
    Color32::from_rgb(0x14, 0xb8, 0xa6),
    Color32::from_rgb(0x06, 0xb6, 0xd4),
    Color32::from_rgb(0x0e, 0xa5, 0xe9),
    Color32::from_rgb(0x3b, 0x82, 0xf6),
    Color32::from_rgb(0x63, 0x66, 0xf1),
    Color32::from_rgb(0x8b, 0x5c, 0xf6),
    Color32::from_rgb(0xd9, 0x46, 0xef),
    Color32::from_rgba_premultiplied(0x40, 0x10, 0x30, 0x80),
    Color32::TRANSPARENT,
];

impl Gallery {
    pub fn new() -> Self {
        let commands = demo_commands();
        let keymap = Keymap::new(Platform::current(), commands.iter().map(|c| (c.id.as_str(), c.shortcut.as_deref())));
        let mut tree = TreeState::default();
        tree.set_expanded(1, true);
        tree.set_expanded(41, true);
        tree.select(Some(42));
        Gallery {
            page: Page::Inputs,
            mode: ThemeMode::System,
            scale: 1.0,
            installed: false,
            opacity: 80.0,
            duration: 125.0,
            gain: -6.0,
            pan: 0.25,
            swatch: Some(10),
            query: String::new(),
            last_edit: None,
            edits: 0,
            list_selected: Some(3),
            table_selected: Some(1),
            sort: Sort { column: 0, ascending: true },
            tree,
            outline: Outline::new(),
            dialog_open: false,
            toasts: Toasts::new(),
            last_dialog: None,
            commands,
            keymap,
            palette: CommandPalette::new(),
            keymap_editor: KeymapEditor::new(),
            ran: Vec::new(),
            show_grid: true,
            revision: 1,
        }
    }

    /// The gallery as a snapshot of one page shows it: overlays open, toasts queued.
    pub fn for_snapshot(page: Page, mode: ThemeMode) -> Self {
        let mut g = Gallery::new();
        g.page = page;
        g.mode = mode;
        if page == Page::Overlays {
            g.dialog_open = true;
            g.toasts.push(Toast::new(ToastLevel::Success, "Export finished: 24 files written").sticky());
            g.toasts.push(Toast::new(ToastLevel::Error, "Could not reach the sync server. Working offline.").sticky());
        }
        if page == Page::Commands {
            g.palette.open();
        }
        g
    }

    fn install(&mut self, ctx: &egui::Context) {
        if !self.installed {
            tokens::install(ctx, &Tokens::light(), &Tokens::dark(), self.mode);
            ashlar::ui::automation::install(ctx);
            self.installed = true;
        }
    }

    /// Run a command picked from a menu, the palette or a shortcut.
    pub fn run(&mut self, ctx: &egui::Context, id: &str) {
        self.ran.push(id.to_owned());
        match id {
            "help.palette" => self.palette.open(),
            "view.theme.light" => self.set_mode(ctx, ThemeMode::Light),
            "view.theme.dark" => self.set_mode(ctx, ThemeMode::Dark),
            "view.theme.system" => self.set_mode(ctx, ThemeMode::System),
            "view.grid" => self.show_grid = !self.show_grid,
            "edit.find" => self.page = Page::Inputs,
            _ => {
                let label = self.commands.iter().find(|c| c.id == id).map_or(id, |c| c.label.as_str());
                self.toasts.push(Toast::new(ToastLevel::Info, format!("Ran “{label}”")).duration(Duration::from_secs(3)));
            }
        }
    }

    fn set_mode(&mut self, ctx: &egui::Context, mode: ThemeMode) {
        self.mode = mode;
        tokens::set_mode(ctx, mode);
    }

    /// Draw the whole gallery into `ui` (the root ui of a frame).
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.install(&ctx);
        if !self.keymap_editor.is_recording()
            && !self.palette.is_open()
            && let Some(id) = dispatch(&ctx, &self.keymap)
        {
            self.run(&ctx, &id);
        }
        let t = tokens::current(&ctx);
        let mut spec = MenuSpec::from_commands(&self.commands, &[]);
        spec.separator_before("file.export.png");
        spec.set_checked("view.grid", Some(self.show_grid));
        for (id, m) in [("view.theme.light", ThemeMode::Light), ("view.theme.dark", ThemeMode::Dark), ("view.theme.system", ThemeMode::System)] {
            spec.set_checked(id, Some(self.mode == m));
        }
        let mut chosen = None;
        egui::Panel::top("gallery.top").frame(egui::Frame::new().fill(t.surface.panel).inner_margin(egui::Margin::symmetric(8, 4))).show(ui, |ui| {
            chosen = menu_bar(ui, &spec, Platform::current());
            ui.horizontal(|ui| {
                for p in Page::ALL {
                    let r = ui.selectable_label(self.page == p, p.name());
                    ashlar::ui::automation::register(
                        ui,
                        &r,
                        &format!("gallery.page.{}", p.name().to_lowercase()),
                        p.name(),
                        ashlar::ui::automation::Role::Tab,
                    );
                    if r.clicked() {
                        self.page = p;
                    }
                }
                ui.separator();
                for (m, name) in [(ThemeMode::Light, "Light"), (ThemeMode::Dark, "Dark"), (ThemeMode::System, "System")] {
                    if ui.selectable_label(self.mode == m, name).clicked() {
                        self.set_mode(&ctx, m);
                    }
                }
                ui.separator();
                ui.label("Scale");
                let fmt = Decimal { decimals: 2, suffix: "×" };
                let out =
                    ValueField::new("gallery.scale", "Interface scale", &mut self.scale, &fmt).range(0.75..=2.0).step(0.05).width(70.0).show(ui);
                if out.edit.is_some_and(|e| matches!(e.phase, EditPhase::Commit)) {
                    tokens::set_interface_scale(&ctx, self.scale as f32);
                }
            });
        });
        if let Some(id) = chosen {
            self.run(&ctx, &id);
        }
        let selected_label = self.list_selected.map(|i| format!("Row {i} selected")).unwrap_or_else(|| "No selection".into());
        let edit_label = self.last_edit.map(|e| format!("{:?} {:.2}", e.phase, e.value)).unwrap_or_default();
        StatusBar::new("gallery.status").show(
            ui,
            |bar| {
                status_bar::text(bar, "gallery.status.selection", &selected_label);
                if !edit_label.is_empty() {
                    status_bar::text(bar, "gallery.status.edit", &edit_label);
                }
            },
            |bar| {
                status_bar::progress(bar, "gallery.status.export", "Exporting", Some(0.62));
                if status_bar::button(bar, "gallery.status.palette", "Commands").clicked() {
                    self.palette.open();
                }
            },
        );
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.surface.app).inner_margin(egui::Margin::same(12))).show(ui, |ui| {
            match self.page {
                Page::Inputs => self.inputs(ui),
                Page::Lists => self.lists(ui),
                Page::Overlays => self.overlays(ui),
                Page::Commands => self.commands_page(ui),
            }
        });
        if let Some(id) = self.palette.show(&ctx, &self.commands, self.revision, Platform::current()) {
            self.run(&ctx, &id);
        }
        self.toasts.show(&ctx);
    }

    fn record(&mut self, edit: Option<Edit>) {
        if let Some(e) = edit {
            self.last_edit = Some(e);
            self.edits += 1;
        }
    }

    fn inputs(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        egui::ScrollArea::vertical().id_salt("gallery.inputs").show(ui, |ui| {
            section(ui, &t, "Value fields", |ui| {
                egui::Grid::new("vf").num_columns(3).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("");
                    ui.label("Enabled");
                    ui.label("Disabled");
                    ui.end_row();
                    let pct = Decimal { decimals: 0, suffix: " %" };
                    ui.label("Opacity");
                    let e = ValueField::new("inputs.opacity", "Opacity", &mut self.opacity, &pct).range(0.0..=100.0).step(1.0).show(ui).edit;
                    self.record(e);
                    let mut copy = self.opacity;
                    ui.add_enabled_ui(false, |ui| ValueField::new("inputs.opacity.disabled", "Opacity", &mut copy, &pct).range(0.0..=100.0).show(ui));
                    ui.end_row();
                    let timecode = FnFormat {
                        format: |v: f64, out: &mut String| {
                            let s = v.max(0.0).round() as u64;
                            out.push_str(&format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60));
                        },
                        parse: |text: &str| {
                            let parts: Vec<f64> = text.split(':').map(|p| p.trim().parse::<f64>().ok()).collect::<Option<Vec<_>>>()?;
                            Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p))
                        },
                        tag: 1,
                    };
                    ui.label("Duration (formatted)");
                    let e = ValueField::new("inputs.duration", "Duration", &mut self.duration, &timecode)
                        .range(0.0..=86_399.0)
                        .speed(1.0)
                        .step(1.0)
                        .width(110.0)
                        .show(ui)
                        .edit;
                    self.record(e);
                    let mut copy = self.duration;
                    ui.add_enabled_ui(false, |ui| {
                        ValueField::new("inputs.duration.disabled", "Duration", &mut copy, &timecode).width(110.0).show(ui)
                    });
                    ui.end_row();
                });
            });
            section(ui, &t, "Sliders and knobs", |ui| {
                let db = Decimal { decimals: 1, suffix: " dB" };
                ui.horizontal(|ui| {
                    let e = Slider::new("inputs.gain", "Gain", &mut self.gain, -60.0..=12.0).step(0.5).show_value(&db).show(ui).edit;
                    self.record(e);
                    let mut copy = self.gain;
                    ui.add_enabled_ui(false, |ui| Slider::new("inputs.gain.disabled", "Gain", &mut copy, -60.0..=12.0).show_value(&db).show(ui));
                });
                ui.horizontal(|ui| {
                    let e = Knob::new("inputs.pan", "Pan", &mut self.pan, -1.0..=1.0).default_value(0.0).show(ui).edit;
                    self.record(e);
                    let mut copy = self.pan;
                    ui.add_enabled_ui(false, |ui| Knob::new("inputs.pan.disabled", "Pan", &mut copy, -1.0..=1.0).show(ui));
                    let mut big = 0.8;
                    Knob::new("inputs.knob.large", "Large", &mut big, 0.0..=1.0).diameter(64.0).show(ui);
                });
            });
            section(ui, &t, "Swatches", |ui| {
                let out = SwatchGrid::new("inputs.swatches", &SWATCHES).columns(8).selected(self.swatch).show(ui);
                if let Some(i) = out.clicked {
                    self.swatch = Some(i);
                }
            });
            section(ui, &t, "Search", |ui| {
                SearchField::new("inputs.search", &mut self.query).hint("Filter layers").width(260.0).show(ui);
                let mut prefilled = String::from("brightness");
                SearchField::new("inputs.search.filled", &mut prefilled).width(260.0).show(ui);
                let mut empty = String::new();
                ui.add_enabled_ui(false, |ui| SearchField::new("inputs.search.disabled", &mut empty).hint("Disabled").width(260.0).show(ui));
            });
        });
    }

    fn lists(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        let h = ui.available_height();
        ui.columns(3, |cols| {
            let [a, b, c] = cols else { return };
            a.label(egui::RichText::new(format!("List — {DEMO_ROWS} rows")).strong());
            let out = VirtualList::new("lists.list", DEMO_ROWS, RowHeights::Uniform(t.control_height))
                .selected(self.list_selected)
                .stripes(true)
                .max_height(h - 40.0)
                .show(a, |ui, row| {
                    ui.label(format!("Row {}", row.index));
                });
            if let Some(i) = out.select {
                self.list_selected = Some(i);
            }
            b.label(egui::RichText::new("Table — variable row heights").strong());
            let cols_spec = [Column::new("Name", 120.0), Column::new("Size", 70.0), Column::new("Kind", 80.0)];
            let height = |i: usize| if i.is_multiple_of(5) { 40.0 } else { 24.0 };
            let ascending = self.sort.ascending;
            let out = VirtualTable::new("lists.table", &cols_spec, DEMO_ROWS, RowHeights::Variable { revision: ascending as u64, height: &height })
                .selected(self.table_selected)
                .sort(Some(self.sort))
                .max_height(h - 40.0)
                .show(b, |ui, row, col| {
                    let n = if ascending { row } else { DEMO_ROWS - 1 - row };
                    match col {
                        0 => ui.label(format!("file_{n:06}")),
                        1 => ui.label(format!("{} KB", (n * 37) % 9000)),
                        _ => ui.label(if n % 5 == 0 { "Folder\n(tall row)" } else { "Document" }),
                    };
                });
            if let Some(i) = out.list.select {
                self.table_selected = Some(i);
            }
            if let Some(c) = out.header_clicked {
                self.sort =
                    if self.sort.column == c { Sort { column: c, ascending: !self.sort.ascending } } else { Sort { column: c, ascending: true } };
            }
            c.label(egui::RichText::new("Tree — 10 041 nodes").strong());
            TreeView::new("lists.tree", &mut self.tree, &self.outline, 1).max_height(h - 40.0).show(c);
        });
    }

    fn overlays(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        section(ui, &t, "Dialog", |ui| {
            if ui.button("Open dialog").clicked() {
                self.dialog_open = true;
            }
            if let Some(o) = self.last_dialog {
                ui.label(format!("Last result: {o:?}"));
            }
        });
        section(ui, &t, "Toasts", |ui| {
            ui.horizontal(|ui| {
                for (level, name) in
                    [(ToastLevel::Info, "Info"), (ToastLevel::Success, "Success"), (ToastLevel::Warning, "Warning"), (ToastLevel::Error, "Error")]
                {
                    if ui.button(name).clicked() {
                        self.toasts.push(Toast::new(level, format!("{name}: something happened at {:.1}s", ui.input(|i| i.time))));
                    }
                }
                if ui.button("Sticky").clicked() {
                    self.toasts.push(Toast::new(ToastLevel::Warning, "This one stays until you close it").sticky());
                }
            });
        });
        if self.dialog_open {
            let buttons = [DialogButton::new("Don't Save"), DialogButton::cancel("Cancel"), DialogButton::new("Save").default()];
            let mut name = String::from("Untitled");
            if let Some(o) = Dialog::new("overlays.dialog", "Save changes before closing?", &buttons).show(ui.ctx(), |ui| {
                ui.label("Your changes will be lost if you don't save them.");
                ui.text_edit_singleline(&mut name);
            }) {
                self.last_dialog = Some(o);
                self.dialog_open = false;
            }
        }
    }

    fn commands_page(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        section(ui, &t, "Command palette", |ui| {
            ui.label("Open with the Help menu, the status bar, or its shortcut.");
            if ui.button("Open palette").clicked() {
                self.palette.open();
            }
            if let Some(last) = self.ran.last() {
                ui.label(format!("Last command: {last}"));
            }
        });
        section(ui, &t, "Keymap", |ui| {
            if self.keymap_editor.show(ui, &mut self.keymap, &self.commands, self.revision).is_some() {
                // a real app saves `keymap.overrides()` here
            }
        });
    }

    /// How many value edits happened (for tests).
    pub fn edit_count(&self) -> u64 {
        self.edits
    }
}

fn section(ui: &mut Ui, t: &Tokens, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(t.surface.panel)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(t.radius.lg)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).size(t.type_scale.heading).strong());
            ui.add_space(t.space.sm);
            add(ui);
        });
    ui.add_space(t.space.lg);
}

/// Headless snapshot mode: render every page in light and dark to `<dir>/<page>-<theme>.png`.
/// Returns the files written.
pub fn snapshot(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut written = Vec::new();
    for page in Page::ALL {
        for (mode, theme) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            let mut h = harness(page, mode, true);
            h.run_steps(6);
            let image = h.render()?;
            let path = dir.join(format!("{}-{theme}.png", page.name().to_lowercase()));
            image.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            written.push(path);
        }
    }
    Ok(written)
}

/// A headless harness showing one page (with a wgpu renderer when `render` is set).
pub fn harness(page: Page, mode: ThemeMode, render: bool) -> egui_kittest::Harness<'static, Gallery> {
    let theme = if mode == ThemeMode::Light { egui::Theme::Light } else { egui::Theme::Dark };
    let mut builder = egui_kittest::Harness::builder().with_size(egui::vec2(1180.0, 780.0)).with_theme(theme).with_max_steps(16);
    if render {
        builder = builder.wgpu();
    }
    builder.build_ui_state(|ui, g: &mut Gallery| g.ui(ui), Gallery::for_snapshot(page, mode))
}
