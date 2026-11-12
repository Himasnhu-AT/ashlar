//! Golden snapshots: compare a `ui.screenshot` reply with a baseline PNG.
//!
//! Baselines live in `tests/snapshots/<name>.png` (relative to the working directory, which is
//! the package folder under `cargo test`; `ASHLAR_SNAPSHOT_DIR` overrides it) next to
//! `<name>.meta.json`, which records what rendered them: `{adapter, backend, scale, width,
//! height, tolerance?}`.
//!
//! - The comparison is exact unless a [`Tolerance`] is given, or recorded in the baseline's meta.
//! - On a mismatch, `<name>-new.png` (what was rendered) and `<name>-diff.png` (the new picture
//!   with every differing pixel magenta) are written next to the baseline, and the check fails.
//! - `ASHLAR_UPDATE_SNAPSHOTS=1` writes the baseline instead of comparing.
//! - A baseline rendered on another adapter or backend, or a screenshot with no GPU renderer
//!   (grey), is skipped with a note, not failed: pixels from different GPUs differ.
//!
//! Egui's own test harness has a snapshot feature, but it is built on its own renderer, compares
//! perceptually, takes no adapter metadata, and pulls the UI toolkit into this rank-4 crate; this
//! module is small and works on any `ui.screenshot` reply, from a live app or a `Driver`.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};

/// The colour of a differing pixel in `-diff.png` (as in the UI crate's visual regression report).
const DIFF: [u8; 4] = [255, 0, 128, 255];
/// Largest picture decoded (bytes of RGBA).
const MAX_BYTES: usize = 256 << 20;

/// How far a screenshot may drift from its baseline and still match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tolerance {
    /// A pixel differs when one of its channels is off by more than this.
    pub channel: u8,
    /// How many differing pixels are allowed.
    pub pixels: usize,
}

impl Tolerance {
    /// `channel=2,pixels=4` (either part may be left out).
    pub fn parse(s: &str) -> Result<Tolerance, String> {
        let mut t = Tolerance::default();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').ok_or_else(|| format!("tolerance `{part}`: expected key=value"))?;
            match k.trim() {
                "channel" => t.channel = v.trim().parse().map_err(|_| format!("tolerance channel={v}: 0 to 255"))?,
                "pixels" => t.pixels = v.trim().parse().map_err(|_| format!("tolerance pixels={v}: a count"))?,
                other => return Err(format!("tolerance `{other}`: channel or pixels")),
            }
        }
        Ok(t)
    }

    fn to_json(self) -> Value {
        json!({"channel": self.channel, "pixels": self.pixels})
    }

    fn from_json(v: &Value) -> Option<Tolerance> {
        let channel = u8::try_from(v.get("channel")?.as_u64()?).ok()?;
        let pixels = usize::try_from(v.get("pixels")?.as_u64()?).ok()?;
        Some(Tolerance { channel, pixels })
    }
}

/// Where baselines live and how they are checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshots {
    /// The baseline folder.
    pub dir: PathBuf,
    /// `None`: the baseline's recorded tolerance, or exact.
    pub tolerance: Option<Tolerance>,
    /// Write baselines instead of comparing.
    pub update: bool,
}

impl Snapshots {
    /// `tests/snapshots` (or `ASHLAR_SNAPSHOT_DIR`), updating when `ASHLAR_UPDATE_SNAPSHOTS` is
    /// `1`, `true` or `yes`.
    pub fn from_env() -> Snapshots {
        let dir = std::env::var_os("ASHLAR_SNAPSHOT_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("tests/snapshots"));
        let update = std::env::var("ASHLAR_UPDATE_SNAPSHOTS").is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
        Snapshots { dir, tolerance: None, update }
    }

    /// Baselines in `dir`, compared exactly.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Snapshots {
        Snapshots { dir: dir.into(), tolerance: None, update: false }
    }

    /// Check (or with `update`, record) the screenshot in `reply` as `name`.
    ///
    /// `reply` is a `ui.screenshot` reply: `{png, renderer?, adapter?, backend?, scale?}`.
    pub fn check(&self, name: &str, reply: &Value) -> Result<Outcome, String> {
        check_name(name)?;
        let base = self.dir.join(format!("{name}.png"));
        let meta_path = self.dir.join(format!("{name}.meta.json"));
        let new_path = self.dir.join(format!("{name}-new.png"));
        let diff_path = self.dir.join(format!("{name}-diff.png"));
        let png = png_bytes(reply)?;
        let now = Meta::of(reply);
        if reply.get("renderer").and_then(Value::as_str) == Some("none") {
            return Ok(Outcome::Skipped(format!("snapshot `{name}`: no GPU renderer, the screenshot is grey; not compared")));
        }
        let img = decode(&png)?;
        if self.update {
            let mut meta = now.to_json(img.width, img.height);
            if let Some(t) = self.tolerance {
                meta["tolerance"] = t.to_json();
            }
            write(&base, &png)?;
            let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
            write(&meta_path, format!("{text}\n").as_bytes())?;
            remove(&new_path);
            remove(&diff_path);
            return Ok(Outcome::Written(base));
        }
        let Ok(old_png) = std::fs::read(&base) else {
            write(&new_path, &png)?;
            return Err(format!(
                "snapshot `{name}`: no baseline at {}; the screenshot is in {}. Run with ASHLAR_UPDATE_SNAPSHOTS=1 to record it",
                base.display(),
                new_path.display()
            ));
        };
        let old_meta: Value = std::fs::read_to_string(&meta_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        if let Some(note) = now.other_device(&old_meta) {
            return Ok(Outcome::Skipped(format!("snapshot `{name}`: {note}; not compared (record one here with ASHLAR_UPDATE_SNAPSHOTS=1)")));
        }
        let tol = self.tolerance.or_else(|| old_meta.get("tolerance").and_then(Tolerance::from_json)).unwrap_or_default();
        let old = decode(&old_png)?;
        let fail = |why: String| -> Result<Outcome, String> {
            write(&new_path, &png)?;
            let mut msg = format!("snapshot `{name}` differs from {}: {why}. New: {}", base.display(), new_path.display());
            if old.width == img.width && old.height == img.height {
                write(&diff_path, &encode(&diff(&old, &img, tol.channel), img.width, img.height)?)?;
                msg.push_str(&format!(", differences (magenta): {}", diff_path.display()));
            }
            msg.push_str(". If the change is intended, rerun with ASHLAR_UPDATE_SNAPSHOTS=1");
            Err(msg)
        };
        if (old.width, old.height) != (img.width, img.height) {
            return fail(format!("the size is {}x{}, the baseline's {}x{}", img.width, img.height, old.width, old.height));
        }
        let off = differing(&old, &img, tol.channel);
        if off > tol.pixels {
            return fail(format!("{off} pixel(s) differ by more than {} (allowed: {})", tol.channel, tol.pixels));
        }
        remove(&new_path);
        remove(&diff_path);
        Ok(Outcome::Matched { differing: off })
    }
}

/// What a snapshot check did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The screenshot matches the baseline (`differing` pixels within the tolerance).
    Matched { differing: usize },
    /// The baseline was written (update mode).
    Written(PathBuf),
    /// Not compared, and why (another adapter, or no GPU renderer).
    Skipped(String),
}

impl Outcome {
    /// One line for a person.
    pub fn text(&self) -> String {
        match self {
            Outcome::Matched { differing: 0 } => "matches".into(),
            Outcome::Matched { differing } => format!("matches ({differing} pixel(s) within tolerance)"),
            Outcome::Written(p) => format!("wrote {}", p.display()),
            Outcome::Skipped(note) => format!("note: {note}"),
        }
    }

    /// For a script step's JSON result.
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::Matched { differing } => json!({"snapshot": "matched", "differing": differing}),
            Outcome::Written(p) => json!({"snapshot": "written", "path": p}),
            Outcome::Skipped(note) => json!({"snapshot": "skipped", "note": note}),
        }
    }
}

/// What rendered a screenshot.
struct Meta {
    adapter: String,
    backend: String,
    scale: Option<f64>,
}

impl Meta {
    fn of(reply: &Value) -> Meta {
        let s = |k: &str| reply.get(k).and_then(Value::as_str).unwrap_or("unknown").to_string();
        Meta { adapter: s("adapter"), backend: s("backend"), scale: reply.get("scale").and_then(Value::as_f64) }
    }

    fn to_json(&self, width: u32, height: u32) -> Value {
        json!({"adapter": self.adapter, "backend": self.backend, "scale": self.scale, "width": width, "height": height})
    }

    /// Why a baseline with `old` meta was made somewhere else (`None`: same device, or unknown).
    fn other_device(&self, old: &Value) -> Option<String> {
        let field = |k: &str| old.get(k).and_then(Value::as_str).filter(|v| *v != "unknown");
        for (k, now) in [("adapter", &self.adapter), ("backend", &self.backend)] {
            if let Some(then) = field(k)
                && now != "unknown"
                && then != now
            {
                return Some(format!("the baseline was rendered on {k} `{then}`, this run on `{now}`"));
            }
        }
        None
    }
}

/// Names are file names: letters, digits, `-`, `_` and `.`, not starting with `.`.
fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok { Ok(()) } else { Err(format!("snapshot name `{name}`: use letters, digits, `-`, `_` and `.`")) }
}

fn png_bytes(reply: &Value) -> Result<Vec<u8>, String> {
    let b64 = reply.get("png").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("the reply carried no picture")?;
    base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| format!("the picture is not base64: {e}"))
}

/// An RGBA8 picture.
struct Rgba {
    width: u32,
    height: u32,
    px: Vec<u8>,
}

fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    let mut dec = png::Decoder::new_with_limits(std::io::Cursor::new(bytes), png::Limits { bytes: MAX_BYTES });
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| format!("not a PNG: {e}"))?;
    let size = reader.output_buffer_size().filter(|n| *n <= MAX_BYTES).ok_or("the PNG is too large")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("bad PNG: {e}"))?;
    buf.truncate(info.buffer_size());
    let px = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed PNGs are not supported".into()),
    };
    Ok(Rgba { width: info.width, height: info.height, px })
}

fn encode(px: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(px).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

fn pixel_differs(a: &[u8; 4], b: &[u8; 4], channel: u8) -> bool {
    a.iter().zip(b).any(|(x, y)| x.abs_diff(*y) > channel)
}

fn differing(old: &Rgba, new: &Rgba, channel: u8) -> usize {
    old.px.as_chunks::<4>().0.iter().zip(new.px.as_chunks::<4>().0).filter(|(a, b)| pixel_differs(a, b, channel)).count()
}

/// The new picture with every pixel off by more than `channel` painted [`DIFF`].
fn diff(old: &Rgba, new: &Rgba, channel: u8) -> Vec<u8> {
    let mut out = new.px.clone();
    for (o, (a, b)) in out.as_chunks_mut::<4>().0.iter_mut().zip(old.px.as_chunks::<4>().0.iter().zip(new.px.as_chunks::<4>().0)) {
        if pixel_differs(a, b, channel) {
            o.copy_from_slice(&DIFF);
        }
    }
    out
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Remove a stale `-new`/`-diff` file (missing is fine).
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A 4x2 screenshot reply, all `colour`, with one pixel `odd` at (1, 1).
    fn shot(colour: [u8; 4], odd: [u8; 4], adapter: &str) -> Value {
        let mut px: Vec<u8> = (0..8).flat_map(|_| colour).collect();
        px[20..24].copy_from_slice(&odd);
        let png = encode(&px, 4, 2).unwrap();
        json!({"png": base64::engine::general_purpose::STANDARD.encode(png), "renderer": "wgpu", "adapter": adapter, "backend": "Metal", "scale": 2.0})
    }

    const RED: [u8; 4] = [200, 0, 0, 255];

    fn update(dir: &Path) -> Snapshots {
        Snapshots { update: true, ..Snapshots::in_dir(dir) }
    }

    #[test]
    fn update_writes_the_baseline_and_its_meta() {
        let t = tempfile::tempdir().unwrap();
        let out = update(t.path()).check("home", &shot(RED, RED, "GPU A")).unwrap();
        assert_eq!(out, Outcome::Written(t.path().join("home.png")));
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(t.path().join("home.meta.json")).unwrap()).unwrap();
        assert_eq!(meta, json!({"adapter": "GPU A", "backend": "Metal", "scale": 2.0, "width": 4, "height": 2}));
        assert_eq!(Snapshots::in_dir(t.path()).check("home", &shot(RED, RED, "GPU A")).unwrap(), Outcome::Matched { differing: 0 });
    }

    #[test]
    fn a_changed_colour_fails_and_writes_new_and_diff() {
        let t = tempfile::tempdir().unwrap();
        update(t.path()).check("home", &shot(RED, RED, "GPU A")).unwrap();
        let e = Snapshots::in_dir(t.path()).check("home", &shot(RED, [0, 0, 255, 255], "GPU A")).unwrap_err();
        assert!(e.contains("1 pixel(s) differ") && e.contains("ASHLAR_UPDATE_SNAPSHOTS=1"), "{e}");
        let d = decode(&std::fs::read(t.path().join("home-diff.png")).unwrap()).unwrap();
        assert_eq!(&d.px[20..24], &DIFF);
        assert_eq!(&d.px[0..4], &RED);
        assert!(t.path().join("home-new.png").exists());
        // a tolerance lets it pass, and a pass clears the stale files
        let loose = Snapshots { tolerance: Some(Tolerance { channel: 0, pixels: 1 }), ..Snapshots::in_dir(t.path()) };
        assert_eq!(loose.check("home", &shot(RED, [0, 0, 255, 255], "GPU A")).unwrap(), Outcome::Matched { differing: 1 });
        assert!(!t.path().join("home-diff.png").exists());
    }

    #[test]
    fn another_adapter_or_no_gpu_is_skipped() {
        let t = tempfile::tempdir().unwrap();
        update(t.path()).check("home", &shot(RED, RED, "GPU A")).unwrap();
        let out = Snapshots::in_dir(t.path()).check("home", &shot([0, 0, 0, 255], RED, "GPU B")).unwrap();
        assert!(matches!(&out, Outcome::Skipped(n) if n.contains("GPU A") && n.contains("GPU B")), "{out:?}");
        let mut grey = shot(RED, RED, "GPU A");
        grey["renderer"] = json!("none");
        assert!(matches!(Snapshots::in_dir(t.path()).check("home", &grey).unwrap(), Outcome::Skipped(_)));
    }

    #[test]
    fn missing_baselines_sizes_and_names_fail_clearly() {
        let t = tempfile::tempdir().unwrap();
        let e = Snapshots::in_dir(t.path()).check("none", &shot(RED, RED, "GPU A")).unwrap_err();
        assert!(e.contains("no baseline"), "{e}");
        assert!(Snapshots::in_dir(t.path()).check("../up", &shot(RED, RED, "GPU A")).is_err());
        let tiny = encode(&RED, 1, 1).unwrap();
        std::fs::write(t.path().join("small.png"), tiny).unwrap();
        let e = Snapshots::in_dir(t.path()).check("small", &shot(RED, RED, "GPU A")).unwrap_err();
        assert!(e.contains("4x2") && e.contains("1x1"), "{e}");
    }

    #[test]
    fn tolerances_parse_and_are_recorded() {
        assert_eq!(Tolerance::parse("channel=2,pixels=4").unwrap(), Tolerance { channel: 2, pixels: 4 });
        assert_eq!(Tolerance::parse("pixels=9").unwrap(), Tolerance { channel: 0, pixels: 9 });
        assert!(Tolerance::parse("channel=300").is_err());
        assert!(Tolerance::parse("colour=1").is_err());
        let t = tempfile::tempdir().unwrap();
        let rec = Snapshots { tolerance: Some(Tolerance { channel: 0, pixels: 1 }), ..update(t.path()) };
        rec.check("home", &shot(RED, RED, "GPU A")).unwrap();
        // the recorded tolerance applies when the check names none
        assert_eq!(Snapshots::in_dir(t.path()).check("home", &shot(RED, [1, 2, 3, 255], "GPU A")).unwrap(), Outcome::Matched { differing: 1 });
    }
}
