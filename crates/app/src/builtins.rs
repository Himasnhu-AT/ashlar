//! The standard commands every app gets: files, undo/redo, palette, keymap, theme, quit.

use std::path::PathBuf;

use ashlar_command::{CommandError, CommandSpec, Param, Params};
use ashlar_ui::tokens::ThemeMode;
use serde_json::{Value, json};

use crate::session::{Document, Effect, Session};

type R = Result<Value, CommandError>;

const PATH_DISCARD: &[Param] =
    &[Param::string("path").about("file to open; without it the window asks"), Param::bool("discard").about("drop unsaved changes without asking")];
const PATH: &[Param] = &[Param::string("path").about("where to save; without it the window asks")];
const DISCARD: &[Param] = &[Param::bool("discard").about("drop unsaved changes without asking")];
const THEME: &[Param] = &[Param::one_of("mode", &["light", "dark", "system"]).about("without it: the next mode")];

/// File and edit commands, first in the menus.
pub(crate) fn leading<D: Document>() -> Vec<CommandSpec<Session<D>>> {
    vec![
        CommandSpec::new("file.new", "New", file_new::<D>).menu(&["File"]).shortcut("Mod+N").params(Params::Static(DISCARD)).query(),
        CommandSpec::new("file.open", "Open…", file_open::<D>).menu(&["File"]).shortcut("Mod+O").params(Params::Static(PATH_DISCARD)).query(),
        CommandSpec::new("file.save", "Save", file_save::<D>).menu(&["File"]).shortcut("Mod+S").query(),
        CommandSpec::new("file.save_as", "Save As…", file_save_as::<D>).menu(&["File"]).shortcut("Mod+Shift+S").params(Params::Static(PATH)).query(),
        CommandSpec::new("file.revert", "Revert to Saved", file_revert::<D>)
            .menu(&["File"])
            .params(Params::Static(DISCARD))
            .enabled(can_revert::<D>)
            .query(),
        CommandSpec::new("app.quit", "Quit", app_quit::<D>).menu(&["File"]).shortcut("Mod+Q").params(Params::Static(DISCARD)).query(),
        CommandSpec::new("app.undo", "Undo", app_undo::<D>).menu(&["Edit"]).shortcut("Mod+Z").enabled(can_undo::<D>).query(),
        CommandSpec::new("app.redo", "Redo", app_redo::<D>).menu(&["Edit"]).shortcut("Mod+Shift+Z / Mod+Y").enabled(can_redo::<D>).query(),
    ]
}

/// View commands, after the app's own.
pub(crate) fn trailing<D: Document>() -> Vec<CommandSpec<Session<D>>> {
    vec![
        CommandSpec::new("app.palette", "Command Palette…", app_palette::<D>).menu(&["View"]).shortcut("Mod+Shift+P").query(),
        CommandSpec::new("app.keymap", "Keyboard Shortcuts…", app_keymap::<D>).menu(&["View"]).query(),
        CommandSpec::new("view.theme", "Switch Theme", view_theme::<D>).menu(&["View"]).params(Params::Static(THEME)).query(),
        CommandSpec::new("app.document", "Document Info", app_document::<D>).query(),
    ]
}

fn can_undo<D: Document>(s: &Session<D>) -> Result<(), String> {
    if s.can_undo() { Ok(()) } else { Err("nothing to undo".into()) }
}

fn can_redo<D: Document>(s: &Session<D>) -> Result<(), String> {
    if s.can_redo() { Ok(()) } else { Err("nothing to redo".into()) }
}

fn can_revert<D: Document>(s: &Session<D>) -> Result<(), String> {
    match (s.path(), s.is_dirty()) {
        (None, _) => Err("the document was never saved".into()),
        (Some(_), false) => Err("no unsaved changes".into()),
        (Some(_), true) => Ok(()),
    }
}

fn flag(p: &Value, key: &str) -> bool {
    p.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn path_param(p: &Value) -> Option<PathBuf> {
    p.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn fail(e: impl std::fmt::Display) -> CommandError {
    CommandError::failed("", e.to_string())
}

/// With unsaved changes and no `discard`: ask the window, or refuse headless.
fn guard_unsaved<D: Document>(s: &mut Session<D>, command: &str, p: &Value) -> Result<bool, CommandError> {
    if !s.is_dirty() || flag(p, "discard") {
        return Ok(true);
    }
    if s.is_windowed() {
        let mut params = p.clone();
        if let Some(m) = params.as_object_mut() {
            m.insert("discard".into(), Value::Bool(true));
        }
        s.request(Effect::ConfirmDiscard { command: command.to_string(), params });
        return Ok(false);
    }
    Err(fail("the document has unsaved changes (save it, or pass `discard: true`)"))
}

fn status<D: Document>(s: &Session<D>) -> Value {
    json!({
        "path": s.path().map(|p| p.to_string_lossy().into_owned()),
        "name": s.display_name(),
        "dirty": s.is_dirty(),
        "revision": s.revision().0,
        "can_undo": s.can_undo(),
        "can_redo": s.can_redo(),
        "migrated_from": s.migrated_from(),
    })
}

fn file_new<D: Document>(s: &mut Session<D>, p: &Value) -> R {
    if !guard_unsaved(s, "file.new", p)? {
        return Ok(json!({"pending": "confirm"}));
    }
    s.new_document();
    Ok(status(s))
}

fn file_open<D: Document>(s: &mut Session<D>, p: &Value) -> R {
    if !guard_unsaved(s, "file.open", p)? {
        return Ok(json!({"pending": "confirm"}));
    }
    match path_param(p) {
        Some(path) => {
            s.open(&path).map_err(fail)?;
            Ok(status(s))
        }
        None if s.is_windowed() => {
            s.request(Effect::PickOpen);
            Ok(json!({"pending": "dialog"}))
        }
        None => Err(CommandError::bad_params("", "`path` is required without a window")),
    }
}

fn file_save<D: Document>(s: &mut Session<D>, _: &Value) -> R {
    if s.path().is_none() {
        if s.is_windowed() {
            s.request(Effect::PickSaveAs);
            return Ok(json!({"pending": "dialog"}));
        }
        return Err(fail("the document has no file yet: use file.save_as with a `path`"));
    }
    s.save().map_err(fail)?;
    Ok(status(s))
}

fn file_save_as<D: Document>(s: &mut Session<D>, p: &Value) -> R {
    match path_param(p) {
        Some(path) => {
            s.save_as(&path).map_err(fail)?;
            Ok(status(s))
        }
        None if s.is_windowed() => {
            s.request(Effect::PickSaveAs);
            Ok(json!({"pending": "dialog"}))
        }
        None => Err(CommandError::bad_params("", "`path` is required without a window")),
    }
}

fn file_revert<D: Document>(s: &mut Session<D>, p: &Value) -> R {
    if !guard_unsaved(s, "file.revert", p)? {
        return Ok(json!({"pending": "confirm"}));
    }
    let Some(path) = s.path().map(std::path::Path::to_path_buf) else { return Err(fail("the document was never saved")) };
    s.open(&path).map_err(fail)?;
    Ok(status(s))
}

fn app_quit<D: Document>(s: &mut Session<D>, p: &Value) -> R {
    s.request(Effect::Quit { discard: flag(p, "discard") });
    Ok(Value::Null)
}

fn app_undo<D: Document>(s: &mut Session<D>, _: &Value) -> R {
    let label = s.undo();
    Ok(json!({"undone": label}))
}

fn app_redo<D: Document>(s: &mut Session<D>, _: &Value) -> R {
    let label = s.redo();
    Ok(json!({"redone": label}))
}

fn app_palette<D: Document>(s: &mut Session<D>, _: &Value) -> R {
    s.request(Effect::OpenPalette);
    Ok(Value::Null)
}

fn app_keymap<D: Document>(s: &mut Session<D>, _: &Value) -> R {
    s.request(Effect::OpenKeymap);
    Ok(Value::Null)
}

fn view_theme<D: Document>(s: &mut Session<D>, p: &Value) -> R {
    let mode = match p.get("mode").and_then(Value::as_str) {
        Some("light") => ThemeMode::Light,
        Some("dark") => ThemeMode::Dark,
        Some(_) => ThemeMode::System,
        None => match s.view::<ThemeMode>("theme").unwrap_or_default() {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        },
    };
    s.set_view("theme", &mode);
    s.request(Effect::Theme(mode));
    Ok(json!({"mode": mode}))
}

fn app_document<D: Document>(s: &mut Session<D>, _: &Value) -> R {
    Ok(status(s))
}
