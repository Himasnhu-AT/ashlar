//! The ashlar component gallery: every `ashlar-ui` component in its states, in light and dark.
//!
//! `cargo run -p ashlar-example-gallery` opens it; `-- --snapshot <dir>` renders every page in
//! both themes to PNG files without a window. The tests drive the same [`Gallery`] headlessly.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::time::Duration;

use ashlar::command::CommandInfo;
use ashlar::command::Keymap;
use ashlar::command::shortcut::{Chord, Platform};
use ashlar::ui::buttons::{ButtonVisuals, IconButton, IconButtonStyle};
use ashlar::ui::chat::{Approval, ChatMessage, ChatRole, Composer, Message, ToolCard, ToolStatus, Transcript};
use ashlar::ui::code_view::CodeView;
use ashlar::ui::commands::{CommandPalette, Item, KeymapEditor, MenuNode, MenuSpec, Submenu, dispatch, menu, menu_bar};
use ashlar::ui::content::{Accordion, Banner, Card, Collapsible, EmptyState};
use ashlar::ui::controls::{Badge, Button, Keycap, Progress, RadioGroup, Segmented, Size, Skeleton, Spinner, Switch, Tone, Variant};
use ashlar::ui::dialog::{Dialog, DialogButton, DialogOutcome};
use ashlar::ui::edit::{Edit, EditPhase};
use ashlar::ui::form::{self, Field};
use ashlar::ui::overlay::{Combobox, ComboboxState, Popover, Select};
use ashlar::ui::rolling::RollingText;
use ashlar::ui::search::SearchField;
use ashlar::ui::sidebar::{Sidebar, SidebarItem, SidebarSection, SidebarState};
use ashlar::ui::slider::{Knob, Slider};
use ashlar::ui::status_bar::{self, StatusBar};
use ashlar::ui::swatches::SwatchGrid;
use ashlar::ui::tabs::{TabStrip, reorder};
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
    Controls,
    Content,
    Lists,
    Overlays,
    Commands,
    Tabs,
    Settings,
    Chat,
    Code,
    Sidebar,
}

impl Page {
    pub const ALL: [Page; 11] = [
        Page::Inputs,
        Page::Controls,
        Page::Content,
        Page::Lists,
        Page::Tabs,
        Page::Overlays,
        Page::Commands,
        Page::Settings,
        Page::Chat,
        Page::Code,
        Page::Sidebar,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Page::Inputs => "Inputs",
            Page::Controls => "Controls",
            Page::Content => "Content",
            Page::Lists => "Lists",
            Page::Overlays => "Overlays",
            Page::Commands => "Commands",
            Page::Tabs => "Tabs",
            Page::Settings => "Settings",
            Page::Chat => "Chat",
            Page::Code => "Code",
            Page::Sidebar => "Sidebar",
        }
    }
}

/// The code page's sample: a small Rust module with one very long line.
fn demo_code() -> &'static str {
    static CODE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CODE.get_or_init(|| {
        let mut s = String::from(
            "//! A tiny inventory: items, counts and a report.\n\nuse std::collections::BTreeMap;\n\n/// One stock item.\n#[derive(Clone, Debug)]\npub struct Item {\n    pub name: String,\n    pub count: u32,\n}\n\n/// Items by name, sorted.\npub fn index(items: &[Item]) -> BTreeMap<&str, u32> {\n    let mut map = BTreeMap::new();\n    for item in items {\n        *map.entry(item.name.as_str()).or_insert(0) += item.count;\n    }\n    map\n}\n\n",
        );
        s.push_str("pub const REPORT_HEADER: &str = \"");
        for i in 0..24 {
            s.push_str(&format!("column {i} | "));
        }
        s.push_str("\";\n\n");
        for i in 0..40 {
            s.push_str(&format!("/// Restock rule {i}: order more below {} items.\npub fn rule_{i}(count: u32) -> bool {{\n    count < {}\n}}\n\n", 10 + i, 10 + i));
        }
        s
    })
}

/// A long demo conversation for the chat page.
fn demo_chat() -> Vec<ChatMessage> {
    (0..2_000u64)
        .map(|i| {
            let (role, text) = if i.is_multiple_of(2) {
                (ChatRole::User, format!("Question {i}: how do I split a **long** list?"))
            } else {
                (ChatRole::Assistant, format!("Answer {i}: draw only the rows on screen.\n\n- measure what you draw\n- keep the rest as estimates\n\n```rust\nlet first = heights.row_at(top);\n```"))
            };
            ChatMessage { id: i, role, text, streaming: false, revision: 0 }
        })
        .collect()
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
    // controls
    pub sync: bool,
    pub notify: bool,
    pub view: usize,
    pub quality: usize,
    pub align: usize,
    /// The plan picker, whose last option is disabled.
    pub plan: usize,
    /// The open section of the help accordion.
    pub faq_open: Option<usize>,
    pub reduce_motion: bool,
    pub exporting: bool,
    pub button_clicks: u64,
    /// The rolling-number demo's value (cents).
    pub rolling_cents: i64,
    // content
    pub banner_dismissed: bool,
    pub advanced_open: bool,
    pub retries: u64,
    // lists
    pub list_selected: Option<usize>,
    pub table_selected: Option<usize>,
    pub sort: Sort,
    pub tree: TreeState,
    outline: Outline,
    // overlays
    pub dialog_open: bool,
    dialog_opener: Option<egui::Id>,
    pub toasts: Toasts,
    pub last_dialog: Option<DialogOutcome>,
    pub font_size: usize,
    pub font: ComboboxState,
    pub filters_open: bool,
    pub only_errors: bool,
    context_spec: MenuSpec,
    pub last_context: Option<String>,
    // commands
    pub commands: Vec<CommandInfo>,
    /// Whose key names the shortcuts show (`⌘⇧P` or `Ctrl+Shift+P`). Snapshots pin it to macOS so
    /// goldens match on every OS.
    pub platform: Platform,
    pub keymap: Keymap,
    pub palette: CommandPalette,
    pub keymap_editor: KeymapEditor,
    pub ran: Vec<String>,
    pub show_grid: bool,
    pub sidebar: SidebarState,
    /// The sidebar page's mailboxes (renamable) and tags.
    pub sidebar_places: Vec<String>,
    pub sidebar_tags: Vec<String>,
    /// What the sidebar last reported.
    pub sidebar_last: String,
    /// The code page wraps long lines.
    pub code_wrap: bool,
    /// The code page shows line numbers.
    pub code_numbers: bool,
    /// The toast demo's export progress, in quarters (0 = not started).
    pub toast_progress: u8,
    // settings
    pub settings_query: String,
    pub launch_at_login: bool,
    pub auto_update: bool,
    pub appearance: usize,
    pub sidebar_icons: bool,
    pub project_name: String,
    revision: u64,
    doc_tabs: Vec<String>,
    doc_selected: usize,
    many_tabs: Vec<String>,
    many_selected: usize,
    pub chat_messages: Vec<ChatMessage>,
    chat_revision: u64,
    pub chat_draft: String,
    pub chat_pending: bool,
    pub chat_approval: Option<Approval>,
    chat_suggestions: Vec<String>,
}

impl Default for Gallery {
    fn default() -> Self {
        Self::new()
    }
}

const FONT_SIZES: [&str; 5] = ["Small", "Medium", "Large", "Larger", "Largest"];

const FONTS: [&str; 8] = ["Inter", "Iosevka", "JetBrains Mono", "Fira Code", "Source Serif", "Source Sans", "IBM Plex Mono", "Atkinson Hyperlegible"];

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
            sync: true,
            notify: false,
            view: 1,
            quality: 1,
            align: 0,
            plan: 0,
            faq_open: None,
            reduce_motion: false,
            exporting: false,
            button_clicks: 0,
            rolling_cents: 995,
            banner_dismissed: false,
            advanced_open: true,
            retries: 0,
            list_selected: Some(3),
            table_selected: Some(1),
            sort: Sort { column: 0, ascending: true },
            tree,
            outline: Outline::new(),
            dialog_open: false,
            dialog_opener: None,
            toasts: Toasts::new(),
            sidebar: SidebarState::new(),
            sidebar_places: ["Inbox", "Drafts", "Sent", "Archive", "Trash"].map(String::from).to_vec(),
            sidebar_tags: (0..5_000).map(|i| format!("Tag {i:04}")).collect(),
            sidebar_last: String::new(),
            code_wrap: false,
            code_numbers: true,
            toast_progress: 0,
            last_dialog: None,
            font_size: 1,
            font: ComboboxState::new(),
            filters_open: false,
            only_errors: false,
            context_spec: MenuSpec {
                menus: vec![Submenu {
                    label: "Row".into(),
                    enabled: true,
                    role: None,
                    children: vec![
                        MenuNode::Item(Item::new("row.copy", "Copy").accelerator("Mod+C")),
                        MenuNode::Item(Item::new("row.rename", "Rename")),
                        MenuNode::Separator,
                        MenuNode::Item(Item::new("row.delete", "Delete")),
                    ],
                }],
            },
            last_context: None,
            commands,
            platform: Platform::current(),
            keymap,
            palette: CommandPalette::new(),
            keymap_editor: KeymapEditor::new(),
            ran: Vec::new(),
            show_grid: true,
            settings_query: String::new(),
            launch_at_login: false,
            auto_update: true,
            appearance: 2,
            sidebar_icons: true,
            project_name: String::from("Untitled"),
            revision: 1,
            doc_tabs: ["main.rs", "lib.rs", "Cargo.toml", "README.md"].map(String::from).to_vec(),
            doc_selected: 0,
            many_tabs: (1..=60).map(|i| format!("Scene {i}")).collect(),
            many_selected: 30,
            chat_messages: demo_chat(),
            chat_revision: 1,
            chat_draft: String::new(),
            chat_pending: true,
            chat_approval: None,
            chat_suggestions: Vec::new(),
        }
    }

    /// The gallery as a snapshot of one page shows it: overlays open, toasts queued.
    pub fn for_snapshot(page: Page, mode: ThemeMode) -> Self {
        let mut g = Gallery::new();
        g.page = page;
        g.mode = mode;
        g.platform = Platform::Mac;
        g.keymap = Keymap::new(Platform::Mac, g.commands.iter().map(|c| (c.id.as_str(), c.shortcut.as_deref())));
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
            chosen = menu_bar(ui, &spec, self.platform);
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
                Page::Controls => self.controls(ui),
                Page::Content => self.content(ui),
                Page::Lists => self.lists(ui),
                Page::Overlays => self.overlays(ui),
                Page::Commands => self.commands_page(ui),
                Page::Tabs => self.tabs_page(ui),
                Page::Settings => self.settings(ui),
                Page::Chat => self.chat(ui),
                Page::Code => self.code_page(ui),
                Page::Sidebar => self.sidebar_page(ui),
            }
        });
        if let Some(id) = self.palette.show(&ctx, &self.commands, self.revision, self.platform) {
            self.run(&ctx, &id);
        }
        for action in self.toasts.show_actions(&ctx) {
            self.run(&ctx, &action.command);
        }
    }

    fn settings(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        SearchField::new("settings.search", &mut self.settings_query).hint("Search settings").width(280.0).show(ui);
        ui.add_space(t.space.md);
        let Gallery { settings_query, launch_at_login, auto_update, appearance, sidebar_icons, project_name, reduce_motion, .. } = self;
        let reduced = *reduce_motion;
        egui::ScrollArea::vertical().id_salt("gallery.settings").show(ui, |ui| {
            ui.set_max_width(560.0);
            form::filter(ui, "settings", settings_query, |f| {
                f.section("general", "General", |s| {
                    s.row("launch", "Launch at login")
                        .description("Starts the app when you sign in.")
                        .keywords(&["startup", "boot"])
                        .bind(launch_at_login, false)
                        .control(|ui, id, on| Switch::new(id, "", on).reduced_motion(reduced).show(ui));
                    s.row("updates", "Check for updates")
                        .description("Looks for a new version once a day.")
                        .bind(auto_update, true)
                        .control(|ui, id, on| Switch::new(id, "", on).reduced_motion(reduced).show(ui));
                });
                f.section("appearance", "Appearance", |s| {
                    s.row("theme", "Theme")
                        .description("Light, dark, or follow the system.")
                        .keywords(&["dark mode", "colors"])
                        .bind(appearance, 2)
                        .control(|ui, id, v| Segmented::new(id, "Theme", &["Light", "Dark", "System"], v).show(ui));
                    s.row("icons", "Sidebar icons")
                        .bind(sidebar_icons, true)
                        .control(|ui, id, on| Switch::new(id, "", on).reduced_motion(reduced).show(ui));
                });
            });
            ui.add_space(t.space.md);
            let error = project_name.trim().is_empty().then_some("A project needs a name.");
            Field::new("settings.project", "Project name")
                .required()
                .description("Shown in the window title.")
                .error(error)
                .show(ui, |ui, _id| ui.add(egui::TextEdit::singleline(project_name).desired_width(280.0)));
        });
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

    fn controls(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        let reduced = self.reduce_motion;
        egui::ScrollArea::vertical().id_salt("gallery.controls").show(ui, |ui| {
            section(ui, &t, "Buttons", |ui| {
                ui.horizontal(|ui| {
                    for (id, label, variant) in [
                        ("controls.button.primary", "Save", Variant::Primary),
                        ("controls.button.secondary", "Duplicate", Variant::Secondary),
                        ("controls.button.ghost", "Rename", Variant::Ghost),
                        ("controls.button.destructive", "Delete", Variant::Destructive),
                    ] {
                        if Button::new(id, label).variant(variant).show(ui).clicked() {
                            self.button_clicks += 1;
                        }
                    }
                    let plus = |p: &egui::Painter, r: egui::Rect, c: Color32| {
                        let s = egui::Stroke::new(1.5, c);
                        let (m, h) = (r.center(), r.width() * 0.35);
                        p.line_segment([m - egui::vec2(h, 0.0), m + egui::vec2(h, 0.0)], s);
                        p.line_segment([m - egui::vec2(0.0, h), m + egui::vec2(0.0, h)], s);
                    };
                    if Button::new("controls.button.icon", "New layer").icon(&plus).show(ui).clicked() {
                        self.button_clicks += 1;
                    }
                    // an icon-only button names itself on hover
                    let style = icon_style(&t);
                    let r = IconButton::new("Add layer", egui::vec2(t.control_height, t.control_height), &style)
                        .tooltip("Add layer (⇧N)")
                        .show(ui, |ui, r, c| plus(ui.painter(), r.shrink(5.0), c));
                    ashlar::ui::automation::register(ui, &r, "controls.icon.add", "Add layer", ashlar::ui::automation::Role::Button);
                    if r.clicked() {
                        self.button_clicks += 1;
                    }
                });
                ui.horizontal(|ui| {
                    Button::new("controls.button.small", "Small").size(Size::Sm).show(ui);
                    Button::new("controls.button.medium", "Medium").show(ui);
                    Button::new("controls.button.large", "Large").size(Size::Lg).show(ui);
                    Button::new("controls.button.disabled", "Disabled").enabled(false).show(ui);
                    let label = if self.exporting { "Exporting…" } else { "Export" };
                    let r = Button::new("controls.button.loading", label)
                        .variant(Variant::Primary)
                        .loading(self.exporting)
                        .reduced_motion(reduced)
                        .min_width(110.0)
                        .show(ui);
                    if r.clicked() {
                        self.exporting = true;
                    }
                    if self.exporting && Button::new("controls.button.stop", "Stop").variant(Variant::Ghost).show(ui).clicked() {
                        self.exporting = false;
                    }
                });
            });
            section(ui, &t, "Switches and choices", |ui| {
                ui.horizontal(|ui| {
                    Switch::new("controls.switch.sync", "Sync", &mut self.sync).reduced_motion(reduced).show(ui);
                    ui.add_space(t.space.lg);
                    Switch::new("controls.switch.notify", "Notifications", &mut self.notify).reduced_motion(reduced).show(ui);
                    ui.add_space(t.space.lg);
                    let mut locked = true;
                    ui.add_enabled_ui(false, |ui| Switch::new("controls.switch.locked", "Locked", &mut locked).show(ui));
                    ui.add_space(t.space.lg);
                    Switch::new("controls.switch.reduce", "Reduce motion", &mut self.reduce_motion).reduced_motion(reduced).show(ui);
                });
                ui.add_space(t.space.sm);
                Segmented::new("controls.view", "View", &["List", "Grid", "Columns"], &mut self.view).show(ui);
                ui.add_space(t.space.sm);
                ui.horizontal_top(|ui| {
                    RadioGroup::new("controls.quality", "Quality", &["Draft", "Good", "Best"], &mut self.quality).show(ui);
                    ui.add_space(t.space.xl);
                    RadioGroup::new("controls.align", "Alignment", &["Left", "Center", "Right"], &mut self.align).horizontal(true).show(ui);
                    ui.add_space(t.space.xl);
                    // an option the current account cannot pick
                    Segmented::new("controls.plan", "Plan", &["Free", "Team", "Enterprise"], &mut self.plan).disabled(&[2]).show(ui);
                });
            });
            section(ui, &t, "Badges and keycaps", |ui| {
                ui.horizontal(|ui| {
                    for (id, text, tone) in [
                        ("controls.badge.neutral", "Draft", Tone::Neutral),
                        ("controls.badge.accent", "12", Tone::Accent),
                        ("controls.badge.info", "Beta", Tone::Info),
                        ("controls.badge.success", "Synced", Tone::Success),
                        ("controls.badge.warning", "Offline", Tone::Warning),
                        ("controls.badge.danger", "3 errors", Tone::Danger),
                    ] {
                        Badge::new(id, text).tone(tone).show(ui);
                    }
                    ui.add_space(t.space.xl);
                    if let Ok(chord) = Chord::parse("Mod+Shift+P") {
                        Keycap::chord("controls.keys.palette", chord, self.platform).show(ui);
                    }
                    Keycap::new("controls.keys.escape", "Esc").show(ui);
                    if let Ok(chord) = Chord::parse("Mod+Shift+Z") {
                        Keycap::chord("controls.keys.mac", chord, Platform::Mac).show(ui);
                    }
                });
            });
            section(ui, &t, "Progress", |ui| {
                ui.horizontal(|ui| {
                    Progress::bar("controls.progress.bar", "Export", Some(0.62)).width(200.0).show(ui);
                    ui.add_space(t.space.lg);
                    Progress::bar("controls.progress.indeterminate", "Indexing", None).width(200.0).reduced_motion(reduced).show(ui);
                });
                ui.horizontal(|ui| {
                    Progress::circle("controls.progress.circle", "Upload", Some(0.3)).show(ui);
                    Progress::circle("controls.progress.circle.busy", "Connecting", None).reduced_motion(reduced).show(ui);
                    Spinner::new("controls.spinner", "Loading").reduced_motion(reduced).show(ui);
                });
                ui.add_space(t.space.sm);
                Skeleton::rows("controls.skeleton", 3).width(260.0).reduced_motion(reduced).show(ui);
            });
            section(ui, &t, "Rolling numbers", |ui| {
                ui.horizontal(|ui| {
                    if Button::new("controls.rolling.down", "−").size(Size::Sm).show(ui).clicked() {
                        self.rolling_cents = self.rolling_cents.saturating_sub(5);
                    }
                    if Button::new("controls.rolling.up", "+").size(Size::Sm).show(ui).clicked() {
                        self.rolling_cents = self.rolling_cents.saturating_add(5);
                    }
                    let cents = self.rolling_cents;
                    let text = format!("{}${}.{:02}", if cents < 0 { "−" } else { "" }, cents.unsigned_abs() / 100, cents.unsigned_abs() % 100);
                    RollingText::new("controls.rolling.cost", &text).value(cents as f64).tabular(true).show(ui);
                });
            });
            section(ui, &t, "Fling", |ui| {
                // drag the list and let go while moving; pull past an end to feel the rubber band
                let h = t.control_height;
                ui.allocate_ui(egui::vec2(260.0, h * 6.0), |ui| {
                    VirtualList::new("controls.fling.list", 200, RowHeights::Uniform(h))
                        .label("Fling demo")
                        .max_height(h * 6.0)
                        .stripes(true)
                        .fling(true)
                        .show(ui, |ui, row| {
                            ui.label(format!("Row {}", row.index + 1));
                        });
                });
            });
        });
    }

    fn content(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        egui::ScrollArea::vertical().id_salt("gallery.content").show(ui, |ui| {
            section(ui, &t, "Banners", |ui| {
                Banner::new("content.banner.info", Tone::Info, "A new version is ready. It installs the next time the app starts.").show(ui);
                ui.add_space(t.space.sm);
                if !self.banner_dismissed {
                    let out = Banner::new("content.banner.warning", Tone::Warning, "Changes stay on this computer until the connection is back.")
                        .title("Offline")
                        .action("Retry")
                        .dismissible("Close")
                        .show(ui);
                    self.retries += u64::from(out.action);
                    self.banner_dismissed |= out.dismissed;
                    ui.add_space(t.space.sm);
                } else if Button::new("content.banner.restore", "Show the offline banner again").variant(Variant::Ghost).show(ui).clicked() {
                    self.banner_dismissed = false;
                }
                Banner::new("content.banner.danger", Tone::Danger, "The file could not be saved: the disk is full.")
                    .title("Save failed")
                    .action("Save As…")
                    .show(ui);
                ui.add_space(t.space.sm);
                Banner::new("content.banner.success", Tone::Success, "Export finished: 24 files written.").show(ui);
            });
            section(ui, &t, "Cards and collapsible sections", |ui| {
                ui.columns(2, |cols| {
                    let [a, b] = cols else { return };
                    Card::new("content.card").title("Account").show(a, |ui| {
                        ui.label("Signed in as someone@example.com");
                        ui.horizontal(|ui| {
                            Badge::new("content.card.plan", "Pro").tone(Tone::Accent).show(ui);
                            Badge::new("content.card.seats", "3 seats").show(ui);
                        });
                    });
                    Collapsible::new("content.advanced", "Advanced", &mut self.advanced_open).show(b, |ui| {
                        Switch::new("content.advanced.logs", "Verbose logs", &mut self.notify).show(ui);
                        ui.label("Settings most people never change.");
                    });
                    let mut closed = false;
                    Collapsible::new("content.about", "About", &mut closed).show(b, |ui| {
                        ui.label("Version 1.0");
                    });
                });
            });
            section(ui, &t, "Empty state", |ui| {
                let folder = |p: &egui::Painter, r: egui::Rect, c: Color32| {
                    let s = egui::Stroke::new(1.5, c);
                    let r = r.shrink(r.width() * 0.15);
                    p.rect_stroke(egui::Rect::from_min_max(r.min + egui::vec2(0.0, r.height() * 0.2), r.max), 3.0, s, egui::StrokeKind::Inside);
                    p.line_segment([r.min + egui::vec2(0.0, r.height() * 0.2), r.min + egui::vec2(r.width() * 0.4, r.height() * 0.2)], s);
                };
                if EmptyState::new("content.empty", "No files yet")
                    .body("Drop files here, or create one to get started.")
                    .icon(&folder)
                    .action("New file")
                    .show(ui)
                    .action
                {
                    self.button_clicks += 1;
                }
            });
            section(ui, &t, "Accordion (one section open)", |ui| {
                let mut acc = Accordion::new("content.faq", &mut self.faq_open);
                for (i, (title, body)) in [
                    ("Where are my files kept?", "In the folder you chose when you created the project."),
                    ("Can I work offline?", "Yes. Changes sync when the connection is back."),
                    ("How do I export?", "File, Export, then pick a format."),
                ]
                .into_iter()
                .enumerate()
                {
                    acc.section(ui, i, title, |ui| {
                        ui.label(body);
                    });
                }
            });
        });
    }

    fn sidebar_page(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        let tray = |p: &egui::Painter, r: egui::Rect, c: Color32| {
            let s = egui::Stroke::new(1.2, c);
            let r = r.shrink(r.width() * 0.12);
            p.rect_stroke(r, 2.0, s, egui::StrokeKind::Inside);
            p.hline(r.x_range(), r.center().y, s);
        };
        let dot = |p: &egui::Painter, r: egui::Rect, c: Color32| {
            p.circle_filled(r.center(), r.width() * 0.25, c);
        };
        let details = ["12", "", "", "1.2k", ""];
        let places: Vec<SidebarItem> = self
            .sidebar_places
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let item = SidebarItem::new(name).icon(&tray).detail(details.get(i).copied().unwrap_or(""));
                if i == 0 { item.badge("3") } else { item }
            })
            .collect();
        let tags: Vec<SidebarItem> = self.sidebar_tags.iter().map(|n| SidebarItem::new(n).icon(&dot)).collect();
        let sections = [SidebarSection::new("Mailboxes", &places).count(3), SidebarSection::new("Tags", &tags)];
        let mut renamed = None;
        ui.horizontal_top(|ui| {
            egui::Frame::new()
                .fill(t.surface.panel)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(t.radius.lg)
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(240.0);
                    let out = Sidebar::new("sidebar.nav", "Navigation", &mut self.sidebar, &sections).max_height(560.0).show(ui);
                    let name = |r: ashlar::ui::sidebar::ItemRef| {
                        sections.get(r.section).and_then(|s| s.items.get(r.item)).map_or("", |i| i.label).to_owned()
                    };
                    if let Some(r) = out.activated {
                        self.sidebar_last = format!("Opened {}", name(r));
                    } else if let Some(r) = out.context {
                        self.sidebar_last = format!("Context menu for {}", name(r));
                    } else if let Some(s) = out.toggled {
                        self.sidebar_last = format!(
                            "{} {}",
                            if self.sidebar.is_collapsed(s) { "Folded" } else { "Unfolded" },
                            sections.get(s).map_or("", |s| s.title)
                        );
                    }
                    if let Some((r, new)) = out.renamed {
                        self.sidebar_last = format!("Renamed {} to {new}", name(r));
                        renamed = Some((r, new));
                    }
                });
            ui.add_space(t.space.lg);
            ui.vertical(|ui| {
                let selected = self
                    .sidebar
                    .selected
                    .and_then(|r| sections.get(r.section).and_then(|s| s.items.get(r.item)))
                    .map_or("Nothing selected", |i| i.label);
                ui.label(egui::RichText::new(selected).size(t.type_scale.heading).strong());
                ui.label(
                    egui::RichText::new("Tab focuses the sidebar. Arrows move, Left and Right fold, F2 renames, typing jumps.")
                        .color(t.text.secondary),
                );
                if !self.sidebar_last.is_empty() {
                    ui.label(&self.sidebar_last);
                }
            });
        });
        if let Some((r, new)) = renamed
            && r.section == 0
            && let Some(slot) = self.sidebar_places.get_mut(r.item)
        {
            *slot = new;
        }
    }

    fn code_page(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        section(ui, &t, "Code view", |ui| {
            ui.horizontal(|ui| {
                Switch::new("code.wrap", "Wrap long lines", &mut self.code_wrap).show(ui);
                ui.add_space(t.space.lg);
                Switch::new("code.numbers", "Line numbers", &mut self.code_numbers).show(ui);
            });
            ui.add_space(t.space.sm);
            let hint =
                if self.code_wrap { "Long lines wrap at the view's width." } else { "Long lines scroll sideways: Shift+wheel or the bar below." };
            ui.label(egui::RichText::new(hint).color(t.text.secondary));
            ui.add_space(t.space.sm);
            CodeView::new("code.view", demo_code())
                .lang(Some("rust"))
                .wrap(self.code_wrap)
                .line_numbers(self.code_numbers)
                .max_height(420.0)
                .show(ui);
        });
    }

    fn tabs_page(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        section(ui, &t, "Closable, reorderable", |ui| {
            let out =
                TabStrip::new("tabs.documents", &self.doc_tabs, &mut self.doc_selected).label("Documents").closable(true).reorderable(true).show(ui);
            if let Some(m) = out.moved {
                reorder(&mut self.doc_tabs, &mut self.doc_selected, m);
            }
            if let Some(i) = out.closed
                && i < self.doc_tabs.len()
            {
                self.doc_tabs.remove(i);
                if self.doc_selected > i || self.doc_selected >= self.doc_tabs.len() {
                    self.doc_selected = self.doc_selected.saturating_sub(1);
                }
            }
            ui.add_space(t.space.sm);
            match self.doc_tabs.get(self.doc_selected) {
                Some(name) => ui.label(format!("Editing {name}. Arrows move, \u{2318}W closes, drag or \u{2318}\u{21e7}+arrow reorders.")),
                None => ui.label("All tabs closed."),
            };
            if ui.button("Reset tabs").clicked() {
                self.doc_tabs = ["main.rs", "lib.rs", "Cargo.toml", "README.md"].map(String::from).to_vec();
                self.doc_selected = 0;
            }
        });
        section(ui, &t, "Overflow: 60 tabs", |ui| {
            TabStrip::new("tabs.many", &self.many_tabs, &mut self.many_selected).label("Scenes").show(ui);
        });
    }

    fn lists(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        let h = ui.available_height();
        ui.columns(3, |cols| {
            let [a, b, c] = cols else { return };
            a.label(egui::RichText::new(format!("List — {DEMO_ROWS} rows")).strong());
            let list_row = |i: usize| demo_names().0.get(i).map_or("", String::as_str);
            let out = VirtualList::new("lists.list", DEMO_ROWS, RowHeights::Uniform(t.control_height))
                .label("Demo list")
                .row_label(&list_row)
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
            let table_row = |row: usize| {
                let n = if ascending { row } else { DEMO_ROWS - 1 - row };
                demo_names().1.get(n).map_or("", String::as_str)
            };
            let out = VirtualTable::new("lists.table", &cols_spec, DEMO_ROWS, RowHeights::Variable { revision: ascending as u64, height: &height })
                .label("Files")
                .row_label(&table_row)
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
            TreeView::new("lists.tree", &mut self.tree, &self.outline, 1).label("Outline").max_height(h - 40.0).show(c);
        });
    }

    fn chat(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        let avail = ui.available_size();
        let below = 2.0 * t.control_height + 120.0;
        ui.allocate_ui(egui::vec2(avail.x, (avail.y - below).max(120.0)), |ui| {
            Transcript::new("chat.transcript", &self.chat_messages, self.chat_revision).show(ui, |ui, m| {
                let id = format!("chat.msg.{}", m.id);
                Message::new(&id, m.role, &m.text).streaming(m.streaming).show(ui);
            });
        });
        ui.add_space(t.space.sm);
        let status = match self.chat_approval {
            None if self.chat_pending => ToolStatus::NeedsApproval,
            Some(Approval::Denied) => ToolStatus::Failed,
            _ => ToolStatus::Succeeded,
        };
        let out = ToolCard::new("chat.tool.read", "Read file notes.md", status)
            .details(|ui| {
                ui.monospace("{ \"path\": \"notes.md\", \"lines\": 120 }");
            })
            .approval(&mut self.chat_pending)
            .show(ui);
        if out.approval.is_some() {
            self.chat_approval = out.approval;
        }
        ui.add_space(t.space.sm);
        let out = Composer::new("chat.input", &mut self.chat_draft)
            .placeholder("Message (Enter sends, Shift+Enter for a new line)")
            .popup(&self.chat_suggestions)
            .show(ui);
        self.chat_suggestions = match &out.trigger {
            Some(tr) => {
                let pool: &[&str] =
                    if tr.symbol == '@' { &["notes.md", "readme.md", "src/main.rs", "Cargo.toml"] } else { &["help", "clear", "summarize"] };
                pool.iter().filter(|s| s.starts_with(tr.query.as_str())).map(|s| (*s).to_owned()).collect()
            }
            None => Vec::new(),
        };
        if let Some(text) = out.submitted {
            let id = self.chat_messages.len() as u64;
            self.chat_messages.push(ChatMessage { id, role: ChatRole::User, text, streaming: false, revision: 0 });
            self.chat_messages.push(ChatMessage { id: id + 1, role: ChatRole::Assistant, text: "Noted.".into(), streaming: false, revision: 0 });
            self.chat_revision += 1;
        }
    }

    fn overlays(&mut self, ui: &mut Ui) {
        let t = tokens::current(ui.ctx());
        section(ui, &t, "Dialog", |ui| {
            let open = Button::new("overlays.open", "Open dialog").show(ui);
            if open.clicked() {
                self.dialog_open = true;
                // a pointer click does not focus the button: name it so the focus comes back there
                self.dialog_opener = Some(open.id);
            }
            if let Some(o) = self.last_dialog {
                ui.label(format!("Last result: {o:?}"));
            }
        });
        section(ui, &t, "Select and combobox", |ui| {
            ui.horizontal(|ui| {
                ui.label("Size");
                Select::new("overlays.size", &FONT_SIZES, &mut self.font_size).label("Size").width(140.0).show(ui);
                ui.label("Font");
                Combobox::new("overlays.font", &FONTS, &mut self.font).label("Font").hint("Type to filter").width(200.0).show(ui);
            });
        });
        section(ui, &t, "Popover, tooltip and context menu", |ui| {
            ui.horizontal(|ui| {
                let anchor = ashlar::ui::help(ui.button("Filters"), "Narrow the list down");
                if anchor.clicked() {
                    self.filters_open = !self.filters_open;
                }
                Popover::new("overlays.filters").show(&anchor, &mut self.filters_open, |ui| {
                    ui.checkbox(&mut self.only_errors, "Only errors");
                });
                let row = ui.label("Right-click me");
                if let Some(id) = menu::context(&row, &self.context_spec) {
                    self.last_context = Some(id);
                }
                if let Some(id) = &self.last_context {
                    ui.label(format!("Chose {id}"));
                }
            });
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
            ui.add_space(t.space.sm);
            ui.horizontal(|ui| {
                if Button::new("overlays.toast.action", "With actions").show(ui).clicked() {
                    // the actions name gallery commands: the gallery runs them when clicked
                    let toast = Toast::success("24 files written to Exports").title("Export finished").action("Toggle grid", "view.grid");
                    self.toasts.push(toast.action("Palette", "help.palette").sticky());
                }
                if Button::new("overlays.toast.progress", "Advance export").show(ui).clicked() {
                    self.toast_progress = (self.toast_progress + 1) % 5;
                    if self.toast_progress == 0 {
                        self.toasts.push(Toast::success("Export finished").key("gallery.export").duration(Duration::from_secs(3)));
                    } else {
                        self.toasts.progress("gallery.export", "Exporting…", Some(f32::from(self.toast_progress) / 4.0));
                    }
                }
            });
            ui.label(egui::RichText::new("F6 moves focus to the newest toast and back; Escape dismisses it.").color(t.text.secondary));
        });
        if self.dialog_open {
            let buttons = [DialogButton::new("Don't Save"), DialogButton::cancel("Cancel"), DialogButton::new("Save").default()];
            let mut name = String::from("Untitled");
            let mut dialog = Dialog::new("overlays.dialog", "Save changes before closing?", &buttons)
                .description("Your changes will be lost if you don't save them.");
            if let Some(opener) = self.dialog_opener {
                dialog = dialog.return_focus_to(opener);
            }
            if let Some(o) = dialog.show(ui.ctx(), |ui| {
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

/// Colours of the gallery's icon buttons, from the tokens.
fn icon_style(t: &Tokens) -> IconButtonStyle {
    let v = |fill, icon| ButtonVisuals { fill, stroke: egui::Stroke::NONE, icon };
    IconButtonStyle {
        normal: v(Color32::TRANSPARENT, t.text.secondary),
        hovered: v(t.surface.hover, t.text.primary),
        pressed: Some(v(t.surface.pressed, t.text.primary)),
        selected: v(t.selection, t.selection_text),
        corner_radius: egui::CornerRadius::same(t.radius.md),
        focus_stroke: egui::Stroke::new(1.5, t.focus),
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

/// Row names for the Lists page (`Row n` and `file_nnnnnn`), built once on first use.
fn demo_names() -> &'static (Vec<String>, Vec<String>) {
    static NAMES: std::sync::OnceLock<(Vec<String>, Vec<String>)> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| ((0..DEMO_ROWS).map(|i| format!("Row {i}")).collect(), (0..DEMO_ROWS).map(|n| format!("file_{n:06}")).collect()))
}
