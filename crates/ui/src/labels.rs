//! Every word ashlar's components show, in one [`UiLabels`] installed on the context (like the
//! theme tokens). Components use the installed labels unless the app passes its own to that
//! component (`Toasts::labels`, `Form::labels`, `DiffView::labels`, …), which always win.
//!
//! The defaults are English. [`UiLabels::from_i18n`] builds them from an [`I18n`] that has the
//! built-in catalogs ([`add_catalogs`]: English and German), so an app switches every built-in
//! string with its own locale and can override any `ui.*` key in its own catalogs:
//!
//! ```
//! use ashlar_i18n::I18n;
//! use ashlar_ui::labels::{self, UiLabels};
//!
//! let mut i18n = I18n::new("en");
//! labels::add_catalogs(&mut i18n);
//! i18n.set_locale("de");
//! let ctx = egui::Context::default();
//! labels::install(&ctx, UiLabels::from_i18n(&i18n));
//! assert_eq!(labels::current(&ctx).toast.dismiss, "Schließen");
//! ```

use std::sync::{Arc, OnceLock};

use ashlar_i18n::I18n;

use crate::chat::ChatLabels;
use crate::commands::keymap_editor::KeymapLabels;
use crate::diff_view::DiffLabels;
use crate::form::FormLabels;
use crate::markdown::MarkdownLabels;
use crate::toasts::ToastLabels;

/// The built-in catalogs (`ui.*` keys), as `(locale, json)`.
pub const CATALOGS: &[(&str, &str)] = &[("en", include_str!("../i18n/en.json")), ("de", include_str!("../i18n/de.json"))];

/// Accessible names of the tab strip's buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLabels {
    pub overflow: String,
    pub close: String,
}

impl Default for TabLabels {
    fn default() -> Self {
        TabLabels { overflow: "More tabs".into(), close: "Close tab".into() }
    }
}

/// The print settings panel's words, owned; [`PrintLabels::as_labels`] lends them to
/// [`crate::printing::State::show`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintLabels {
    pub printer: String,
    pub select_printer: String,
    pub refresh: String,
    pub copies: String,
    pub printer_default: String,
    pub duplex: String,
    pub one_sided: String,
    pub long_edge: String,
    pub short_edge: String,
    pub grayscale: String,
}

impl Default for PrintLabels {
    fn default() -> Self {
        PrintLabels {
            printer: "Printer".into(),
            select_printer: "Select a printer".into(),
            refresh: "Refresh".into(),
            copies: "Copies".into(),
            printer_default: "Printer default".into(),
            duplex: "Two-sided".into(),
            one_sided: "One-sided".into(),
            long_edge: "Long edge".into(),
            short_edge: "Short edge".into(),
            grayscale: "Grayscale".into(),
        }
    }
}

impl PrintLabels {
    pub fn as_labels(&self) -> crate::printing::Labels<'_> {
        crate::printing::Labels {
            printer: &self.printer,
            select_printer: &self.select_printer,
            refresh: &self.refresh,
            copies: &self.copies,
            printer_default: &self.printer_default,
            duplex: &self.duplex,
            one_sided: &self.one_sided,
            long_edge: &self.long_edge,
            short_edge: &self.short_edge,
            grayscale: &self.grayscale,
        }
    }
}

/// Every built-in component string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiLabels {
    pub toast: ToastLabels,
    pub form: FormLabels,
    pub chat: ChatLabels,
    pub diff: DiffLabels,
    pub keymap: KeymapLabels,
    pub tabs: TabLabels,
    pub print: PrintLabels,
    pub search: SearchLabels,
    pub markdown: MarkdownLabels,
}

/// The search field's and the command palette's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLabels {
    /// The search field's name for screen readers (the placeholder is only a hint).
    pub field: String,
    pub clear: String,
    /// The command palette's input name.
    pub palette: String,
}

impl Default for SearchLabels {
    fn default() -> Self {
        SearchLabels { field: "Search".into(), clear: "Clear search".into(), palette: "Command palette".into() }
    }
}

/// Every key [`UiLabels::from_i18n`] reads; each must be in the English catalog (tested).
pub const KEYS: &[&str] = &[
    "ui.toast.dismiss",
    "ui.toast.more",
    "ui.form.no_match",
    "ui.form.no_match_body",
    "ui.form.reset",
    "ui.form.required",
    "ui.chat.transcript",
    "ui.chat.jump_to_latest",
    "ui.chat.user",
    "ui.chat.assistant",
    "ui.chat.system",
    "ui.chat.tool",
    "ui.chat.streaming",
    "ui.chat.approve",
    "ui.chat.deny",
    "ui.chat.show_details",
    "ui.chat.hide_details",
    "ui.chat.pending",
    "ui.chat.running",
    "ui.chat.succeeded",
    "ui.chat.failed",
    "ui.chat.needs_approval",
    "ui.chat.approved",
    "ui.chat.denied",
    "ui.chat.suggestions",
    "ui.chat.composer",
    "ui.diff.expand",
    "ui.diff.hidden",
    "ui.diff.binary",
    "ui.diff.added",
    "ui.diff.removed",
    "ui.keymap.search",
    "ui.keymap.command",
    "ui.keymap.shortcut",
    "ui.keymap.recording",
    "ui.keymap.unbound",
    "ui.keymap.reset",
    "ui.keymap.clear",
    "ui.keymap.conflicts_with",
    "ui.keymap.table",
    "ui.keymap.actions",
    "ui.search.field",
    "ui.search.clear",
    "ui.search.palette",
    "ui.tabs.overflow",
    "ui.tabs.close",
    "ui.print.printer",
    "ui.print.select_printer",
    "ui.print.refresh",
    "ui.print.copies",
    "ui.print.printer_default",
    "ui.print.duplex",
    "ui.print.one_sided",
    "ui.print.long_edge",
    "ui.print.short_edge",
    "ui.print.grayscale",
    "ui.markdown.copy",
    "ui.markdown.copied",
    "ui.markdown.code",
    "ui.markdown.more_rows",
];

/// Adds the built-in catalogs to `i18n` (add the app's own catalogs after, to override keys).
pub fn add_catalogs(i18n: &mut I18n) {
    for (locale, json) in CATALOGS {
        i18n.add(locale, *json);
    }
}

impl UiLabels {
    /// The labels in `i18n`'s current locale. Keys `i18n` lacks keep their English default, so
    /// this works without [`add_catalogs`] too.
    pub fn from_i18n(i18n: &I18n) -> Self {
        let mut l = UiLabels::default();
        let set = |field: &mut String, key: &str| {
            if let Some(text) = i18n.get(key) {
                text.clone_into(field);
            }
        };
        let t = &mut l.toast;
        set(&mut t.dismiss, "ui.toast.dismiss");
        set(&mut t.more, "ui.toast.more");
        let f = &mut l.form;
        set(&mut f.no_match, "ui.form.no_match");
        set(&mut f.no_match_body, "ui.form.no_match_body");
        set(&mut f.reset, "ui.form.reset");
        set(&mut f.required, "ui.form.required");
        let c = &mut l.chat;
        for (field, key) in [
            (&mut c.transcript, "ui.chat.transcript"),
            (&mut c.jump_to_latest, "ui.chat.jump_to_latest"),
            (&mut c.user, "ui.chat.user"),
            (&mut c.assistant, "ui.chat.assistant"),
            (&mut c.system, "ui.chat.system"),
            (&mut c.tool, "ui.chat.tool"),
            (&mut c.streaming, "ui.chat.streaming"),
            (&mut c.approve, "ui.chat.approve"),
            (&mut c.deny, "ui.chat.deny"),
            (&mut c.show_details, "ui.chat.show_details"),
            (&mut c.hide_details, "ui.chat.hide_details"),
            (&mut c.pending, "ui.chat.pending"),
            (&mut c.running, "ui.chat.running"),
            (&mut c.succeeded, "ui.chat.succeeded"),
            (&mut c.failed, "ui.chat.failed"),
            (&mut c.needs_approval, "ui.chat.needs_approval"),
            (&mut c.approved, "ui.chat.approved"),
            (&mut c.denied, "ui.chat.denied"),
            (&mut c.suggestions, "ui.chat.suggestions"),
            (&mut c.composer, "ui.chat.composer"),
        ] {
            set(field, key);
        }
        let d = &mut l.diff;
        set(&mut d.expand, "ui.diff.expand");
        set(&mut d.hidden, "ui.diff.hidden");
        set(&mut d.binary, "ui.diff.binary");
        set(&mut d.added, "ui.diff.added");
        set(&mut d.removed, "ui.diff.removed");
        let k = &mut l.keymap;
        for (field, key) in [
            (&mut k.search, "ui.keymap.search"),
            (&mut k.command, "ui.keymap.command"),
            (&mut k.shortcut, "ui.keymap.shortcut"),
            (&mut k.recording, "ui.keymap.recording"),
            (&mut k.unbound, "ui.keymap.unbound"),
            (&mut k.reset, "ui.keymap.reset"),
            (&mut k.clear, "ui.keymap.clear"),
            (&mut k.conflicts_with, "ui.keymap.conflicts_with"),
            (&mut k.table, "ui.keymap.table"),
            (&mut k.actions, "ui.keymap.actions"),
        ] {
            set(field, key);
        }
        set(&mut l.search.field, "ui.search.field");
        set(&mut l.search.clear, "ui.search.clear");
        set(&mut l.search.palette, "ui.search.palette");
        set(&mut l.tabs.overflow, "ui.tabs.overflow");
        set(&mut l.tabs.close, "ui.tabs.close");
        let p = &mut l.print;
        for (field, key) in [
            (&mut p.printer, "ui.print.printer"),
            (&mut p.select_printer, "ui.print.select_printer"),
            (&mut p.refresh, "ui.print.refresh"),
            (&mut p.copies, "ui.print.copies"),
            (&mut p.printer_default, "ui.print.printer_default"),
            (&mut p.duplex, "ui.print.duplex"),
            (&mut p.one_sided, "ui.print.one_sided"),
            (&mut p.long_edge, "ui.print.long_edge"),
            (&mut p.short_edge, "ui.print.short_edge"),
            (&mut p.grayscale, "ui.print.grayscale"),
        ] {
            set(field, key);
        }
        let m = &mut l.markdown;
        for (field, key) in [
            (&mut m.copy, "ui.markdown.copy"),
            (&mut m.copied, "ui.markdown.copied"),
            (&mut m.code, "ui.markdown.code"),
            (&mut m.more_rows, "ui.markdown.more_rows"),
        ] {
            if let Some(text) = i18n.get(key) {
                *field = text.to_owned().into();
            }
        }
        l
    }
}

fn id() -> egui::Id {
    egui::Id::new("ashlar_ui::labels")
}

/// Installs `labels` for every component on `ctx` (until replaced).
pub fn install(ctx: &egui::Context, labels: UiLabels) {
    ctx.data_mut(|d| d.insert_temp(id(), Arc::new(labels)));
    crate::wake::wake(ctx, crate::wake::Reason::SETTINGS);
}

/// The installed labels, or the English defaults. Cheap: an `Arc` clone.
pub fn current(ctx: &egui::Context) -> Arc<UiLabels> {
    ctx.data(|d| d.get_temp::<Arc<UiLabels>>(id())).unwrap_or_else(|| english().clone())
}

/// The English defaults, shared.
pub fn english() -> &'static Arc<UiLabels> {
    static ENGLISH: OnceLock<Arc<UiLabels>> = OnceLock::new();
    ENGLISH.get_or_init(|| Arc::new(UiLabels::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i18n() -> I18n {
        let mut i18n = I18n::new("en");
        add_catalogs(&mut i18n);
        i18n
    }

    #[test]
    fn catalogs_are_complete_and_match_the_defaults() {
        let i18n = i18n();
        assert_eq!(i18n.check(), vec![]);
        assert_eq!(i18n.missing_source_keys(KEYS), Vec::<&str>::new());
        // the English catalog and the Default impls say the same thing
        assert_eq!(UiLabels::from_i18n(&i18n), UiLabels::default());
    }

    #[test]
    fn switching_locale_switches_labels() {
        let mut i18n = i18n();
        i18n.set_locale("de-DE");
        let de = UiLabels::from_i18n(&i18n);
        assert_eq!(de.toast.dismiss, "Schließen");
        assert_eq!(de.tabs.close, "Tab schließen");
        i18n.set_locale("ja");
        assert_eq!(UiLabels::from_i18n(&i18n), UiLabels::default());
    }

    #[test]
    fn current_falls_back_to_english() {
        let ctx = egui::Context::default();
        assert_eq!(current(&ctx).toast.dismiss, "Dismiss");
        let mut l = UiLabels::default();
        l.toast.dismiss = "Close".into();
        install(&ctx, l);
        assert_eq!(current(&ctx).toast.dismiss, "Close");
    }
}
