//! `--headless`: run the app on a [`Driver`] with no window and write a screenshot and the UI
//! tree, for any binary whose `main` calls [`App::run`].
//!
//! ```text
//! notes --headless --size 1024x700 --scale 2 --theme dark --scene two-notes --script add.ui --at 1.5 \
//!       --snap out.png --tree tree.json
//! ```
//!
//! The app runs in a fresh temporary folder (never the person's settings or documents), on the
//! virtual clock. Exit codes match `ashlar-debug`: 0 done, 1 a step failed, 2 bad arguments.

use std::path::{Path, PathBuf};

use ashlar_debug::Exit;
use ashlar_fs::AppDirs;
use serde_json::json;

use crate::{App, Document, Driver, Renderer};

const USAGE: &str =
    "--headless [--size WxH] [--scale S] [--theme light|dark] [--scene NAME] [--script file.ui] [--at SECONDS] [--snap out.png] [--tree tree.json]";

/// What `--headless` was asked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Options {
    pub size: (f32, f32),
    pub scale: f32,
    pub theme: Option<String>,
    pub scene: Option<String>,
    pub script: Option<PathBuf>,
    pub at: f64,
    pub snap: Option<PathBuf>,
    pub tree: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options { size: (1280.0, 800.0), scale: 1.0, theme: None, scene: None, script: None, at: 0.0, snap: None, tree: None }
    }
}

/// The arguments ask for `--headless`.
pub(crate) fn requested(args: &[String]) -> bool {
    args.iter().any(|a| a == "--headless")
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        if flag == "--headless" {
            continue;
        }
        let mut value = || inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| format!("{flag} needs a value"));
        match flag {
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once(['x', 'X']).ok_or_else(|| format!("--size {v}: expected WxH, e.g. 1024x700"))?;
                let num = |s: &str| s.trim().parse::<f32>().ok().filter(|n| n.is_finite() && (16.0..=8192.0).contains(n));
                o.size = num(w).zip(num(h)).ok_or_else(|| format!("--size {v}: width and height from 16 to 8192"))?;
            }
            "--scale" => {
                let v = value()?;
                o.scale = v
                    .parse::<f32>()
                    .ok()
                    .filter(|s| s.is_finite() && (0.25..=8.0).contains(s))
                    .ok_or_else(|| format!("--scale {v}: from 0.25 to 8"))?;
            }
            "--theme" => {
                let v = value()?;
                if v != "light" && v != "dark" {
                    return Err(format!("--theme {v}: light or dark"));
                }
                o.theme = Some(v);
            }
            "--at" => {
                let v = value()?;
                o.at = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && (0.0..=3600.0).contains(s))
                    .ok_or_else(|| format!("--at {v}: seconds from 0 to 3600"))?;
            }
            "--scene" => o.scene = Some(value()?),
            "--script" => o.script = Some(PathBuf::from(value()?)),
            "--snap" => o.snap = Some(PathBuf::from(value()?)),
            "--tree" => o.tree = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument `{other}` with --headless")),
        }
    }
    Ok(o)
}

/// Run `app` headless with `args` (without the program name); returns the exit code.
pub(crate) fn run<D: Document>(app: App<D>, args: &[String]) -> u8 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\nusage: {USAGE}");
            return Exit::Usage.code();
        }
    };
    let script = match &o.script {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("error: cannot read {}: {e}\nusage: {USAGE}", p.display());
                return Exit::Usage.code();
            }
        },
        None => None,
    };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let root = std::env::temp_dir().join(format!("ashlar-headless-{}-{nanos}", std::process::id()));
    let dirs = AppDirs::under(&root);
    if let Err(e) = dirs.ensure() {
        eprintln!("error: cannot create {}: {e}", root.display());
        return Exit::Failed.code();
    }
    let code = drive(app.dirs(dirs), &o, script.as_deref());
    // only the folder made above, which holds nothing of the person's
    let _ = std::fs::remove_dir_all(&root);
    code.code()
}

fn drive<D: Document>(app: App<D>, o: &Options, script: Option<&str>) -> Exit {
    let renderer = Renderer::wgpu();
    if let Some(why) = renderer.reason() {
        eprintln!("note: no GPU renderer ({why}); the screenshot is grey");
    }
    let mut d = Driver::with_renderer(app.runtime(), renderer);
    d.set_screen(o.size.0, o.size.1);
    d.pixels_per_point(o.scale);
    d.settle(60);
    // the scene first: an explicit --theme wins over one the scene sets
    if let Some(scene) = &o.scene
        && let Err(e) = d.scene(scene)
    {
        eprintln!("error: --scene: {e}");
        return Exit::Usage;
    }
    if let Some(theme) = &o.theme
        && let Err(e) = d.call("engine.execute", json!({"command": "view.theme", "params": {"mode": theme}}))
    {
        eprintln!("error: --theme {theme}: {e}");
        return Exit::Failed;
    }
    if let Some(text) = script {
        let name = o.script.as_deref().map(Path::display).map(|p| p.to_string()).unwrap_or_default();
        if let Err(e) = ashlar_debug::run_script(&mut d, &name, text) {
            eprintln!("error: {e}");
            return e.exit();
        }
    }
    d.advance(o.at);
    d.settle(60);
    if let Some(path) = &o.tree {
        let tree = d.call("ui.snapshot", json!({})).and_then(|v| serde_json::to_string_pretty(&v).map_err(|e| e.to_string()));
        if let Err(e) = tree.and_then(|t| write(path, t.as_bytes())) {
            eprintln!("error: --tree: {e}");
            return Exit::Failed;
        }
    }
    if let Some(path) = &o.snap
        && let Err(e) = d.call("ui.screenshot", json!({})).and_then(|v| ashlar_debug::save_png(&v, path))
    {
        eprintln!("error: --snap: {e}");
        return Exit::Failed;
    }
    Exit::Ok
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_the_flags() {
        assert!(!requested(&args("--control 4000")));
        let o = parse(&args("--headless --size 640x480 --scale=2 --theme dark --at 1.5 --snap a.png --tree t.json")).unwrap();
        assert_eq!(o.size, (640.0, 480.0));
        assert_eq!((o.scale, o.at), (2.0, 1.5));
        assert_eq!(o.theme.as_deref(), Some("dark"));
        assert_eq!(o.snap, Some(PathBuf::from("a.png")));
        let o = parse(&args("--headless --scene two-notes")).unwrap();
        assert_eq!(o.scene.as_deref(), Some("two-notes"));
        for bad in ["--size 10", "--size 0x5", "--scale nan", "--theme blue", "--at -1", "--snap", "--frobnicate"] {
            assert!(parse(&args(&format!("--headless {bad}"))).is_err(), "{bad}");
        }
    }
}
