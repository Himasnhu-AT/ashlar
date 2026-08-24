//! System preferences the UI follows (light / dark, accent colour, reduced motion, more
//! contrast) and hand-offs to the OS (open a URL in the browser, open a file in its default app,
//! reveal a file in the file manager).
//!
//! Preferences are read on demand and cheap enough for a settings change or window focus, not
//! for every frame: cache them and re-read when the window gains focus.

use std::path::Path;

use crate::{NativeError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    Light,
    Dark,
}

/// What the user chose in the OS. `None` = unknown here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub appearance: Option<Appearance>,
    /// The accent colour as sRGB.
    pub accent: Option<[u8; 3]>,
    pub reduced_motion: Option<bool>,
    pub increased_contrast: Option<bool>,
}

/// The OS preferences now.
pub fn preferences() -> Preferences {
    crate::guarded("reading system preferences", || Ok(imp::preferences())).unwrap_or_default()
}

/// URL schemes [`open_url`] hands to the OS. Anything else (`file:`, `javascript:`, custom app
/// schemes) could start arbitrary programs, so it is refused.
pub const OPEN_URL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Check a URL for [`open_url`]: an allowed scheme, one line, bounded.
pub fn check_url(url: &str) -> Result<()> {
    let bad = |why: &str| NativeError::Invalid(format!("{why}: `{}`", url.chars().take(80).collect::<String>()));
    if url.len() > 8192 || url.chars().any(|c| c.is_control() || c == ' ') {
        return Err(bad("not a URL"));
    }
    let (scheme, rest) = url.split_once(':').ok_or_else(|| bad("not a URL"))?;
    if !OPEN_URL_SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return Err(bad("only web and mail links are opened"));
    }
    if rest.is_empty() || (!scheme.eq_ignore_ascii_case("mailto") && !rest.starts_with("//")) {
        return Err(bad("not a URL"));
    }
    Ok(())
}

/// Open a web or mail link in the user's default app.
pub fn open_url(url: &str) -> Result<()> {
    check_url(url)?;
    crate::guarded("opening a link", || imp::open_url(url))
}

/// Open a file or folder in its default app.
pub fn open_path(path: &Path) -> Result<()> {
    let path = existing(path)?;
    crate::guarded("opening a file", || imp::open_path(&path))
}

/// Show a file selected in Finder / Explorer / the file manager.
pub fn reveal(path: &Path) -> Result<()> {
    let path = existing(path)?;
    crate::guarded("revealing a file", || imp::reveal(&path))
}

fn existing(path: &Path) -> Result<std::path::PathBuf> {
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    if !abs.exists() {
        return Err(NativeError::Invalid(format!("`{}` does not exist", abs.display())));
    }
    Ok(abs)
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn spawn(cmd: &mut std::process::Command) -> Result<()> {
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|e| NativeError::Os(crate::os_text(e)))?;
    // Reap it in the background so no zombie is left.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::{NSColor, NSColorSpace, NSWorkspace};
    use objc2_foundation::{NSArray, NSString, NSURL, NSUserDefaults};

    use super::*;

    fn channel(v: f64) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    pub fn preferences() -> Preferences {
        let style = NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str("AppleInterfaceStyle"));
        let dark = style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"));
        let accent = NSColor::controlAccentColor()
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .map(|c| [channel(c.redComponent()), channel(c.greenComponent()), channel(c.blueComponent())]);
        let ws = NSWorkspace::sharedWorkspace();
        Preferences {
            appearance: Some(if dark { Appearance::Dark } else { Appearance::Light }),
            accent,
            reduced_motion: Some(ws.accessibilityDisplayShouldReduceMotion()),
            increased_contrast: Some(ws.accessibilityDisplayShouldIncreaseContrast()),
        }
    }

    pub fn open_url(url: &str) -> Result<()> {
        let u = NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(|| NativeError::Invalid("not a URL".into()))?;
        if NSWorkspace::sharedWorkspace().openURL(&u) { Ok(()) } else { Err(NativeError::Os("no app opened the link".into())) }
    }

    pub fn open_path(path: &Path) -> Result<()> {
        let u = NSURL::from_file_path(path).ok_or_else(|| NativeError::Invalid("not a file path".into()))?;
        if NSWorkspace::sharedWorkspace().openURL(&u) { Ok(()) } else { Err(NativeError::Os("no app opened the file".into())) }
    }

    pub fn reveal(path: &Path) -> Result<()> {
        let u = NSURL::from_file_path(path).ok_or_else(|| NativeError::Invalid("not a file path".into()))?;
        NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[u]));
        Ok(())
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod imp {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    use windows::core::{BOOL, HSTRING, PCWSTR, w};
    use windows_registry::CURRENT_USER;

    use super::*;

    pub fn preferences() -> Preferences {
        let light =
            CURRENT_USER.open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").and_then(|k| k.get_u32("AppsUseLightTheme")).ok();
        // 0xAABBGGRR
        let accent = CURRENT_USER
            .open(r"Software\Microsoft\Windows\DWM")
            .and_then(|k| k.get_u32("AccentColor"))
            .ok()
            .map(|c| [c as u8, (c >> 8) as u8, (c >> 16) as u8]);
        let mut animate = BOOL(1);
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to `pvParam`, which points at a live
        // local BOOL for the duration of the call; `uiParam` is unused (0).
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(&mut animate as *mut BOOL as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        Preferences {
            appearance: light.map(|l| if l == 0 { Appearance::Dark } else { Appearance::Light }),
            accent,
            reduced_motion: ok.is_ok().then_some(!animate.as_bool()),
            increased_contrast: None,
        }
    }

    fn shell_open(target: &HSTRING) -> Result<()> {
        // SAFETY: every pointer argument is either null or a NUL-terminated wide string (`w!`,
        // `HSTRING`) that outlives the call; ShellExecuteW does not keep them.
        let r = unsafe { ShellExecuteW(Some(HWND::default()), w!("open"), target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        // Values above 32 mean success.
        if r.0 as isize > 32 { Ok(()) } else { Err(NativeError::Os(format!("Windows could not open it (code {})", r.0 as isize))) }
    }

    pub fn open_url(url: &str) -> Result<()> {
        shell_open(&HSTRING::from(url))
    }

    pub fn open_path(path: &Path) -> Result<()> {
        shell_open(&HSTRING::from(path.as_os_str()))
    }

    pub fn reveal(path: &Path) -> Result<()> {
        use std::os::windows::process::CommandExt;
        let p = path.to_string_lossy();
        if p.contains('"') {
            return Err(NativeError::Invalid("a path with a quote".into()));
        }
        spawn(std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{p}\"")))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use zbus::zvariant::Value;

    use super::*;

    fn read(conn: &zbus::blocking::Connection, namespace: &str, key: &str) -> Option<zbus::zvariant::OwnedValue> {
        let call = |method: &str| {
            conn.call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.Settings"),
                method,
                &(namespace, key),
            )
            .ok()?
            .body()
            .deserialize::<zbus::zvariant::OwnedValue>()
            .ok()
        };
        call("ReadOne").or_else(|| call("Read"))
    }

    /// Unwrap nested variants (`Read` wraps the value twice).
    fn inner<'a>(v: &'a Value<'a>, depth: usize) -> &'a Value<'a> {
        match v {
            Value::Value(b) if depth < 4 => inner(b, depth + 1),
            other => other,
        }
    }

    fn channel(v: &Value<'_>) -> Option<u8> {
        match inner(v, 0) {
            Value::F64(f) if (0.0..=1.0).contains(f) => Some((f * 255.0).round() as u8),
            _ => None,
        }
    }

    pub fn preferences() -> Preferences {
        let Ok(conn) = zbus::blocking::Connection::session() else { return Preferences::default() };
        let appearance = read(&conn, "org.freedesktop.appearance", "color-scheme").and_then(|v| match inner(&v, 0) {
            Value::U32(1) => Some(Appearance::Dark),
            Value::U32(2) => Some(Appearance::Light),
            _ => None,
        });
        let accent = read(&conn, "org.freedesktop.appearance", "accent-color").and_then(|v| match inner(&v, 0) {
            Value::Structure(s) => match s.fields() {
                [r, g, b] => Some([channel(r)?, channel(g)?, channel(b)?]),
                _ => None,
            },
            _ => None,
        });
        let animations = read(&conn, "org.gnome.desktop.interface", "enable-animations").and_then(|v| match inner(&v, 0) {
            Value::Bool(b) => Some(*b),
            _ => None,
        });
        let contrast = read(&conn, "org.freedesktop.appearance", "contrast").and_then(|v| match inner(&v, 0) {
            Value::U32(c) => Some(*c == 1),
            _ => None,
        });
        Preferences { appearance, accent, reduced_motion: animations.map(|a| !a), increased_contrast: contrast }
    }

    pub fn open_url(url: &str) -> Result<()> {
        spawn(std::process::Command::new("xdg-open").arg(url))
    }

    pub fn open_path(path: &Path) -> Result<()> {
        spawn(std::process::Command::new("xdg-open").arg(path))
    }

    /// `file://` URI of an absolute path, percent-encoding everything but unreserved characters
    /// and `/`.
    fn file_uri(path: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut out = String::from("file://");
        for &b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    pub fn reveal(path: &Path) -> Result<()> {
        let shown = zbus::blocking::Connection::session().ok().and_then(|conn| {
            conn.call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &(vec![file_uri(path)], ""),
            )
            .ok()
        });
        if shown.is_some() {
            return Ok(());
        }
        // No file manager service: open the folder instead.
        spawn(std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)))
    }

    #[cfg(test)]
    #[test]
    fn file_uris_are_encoded() {
        assert_eq!(file_uri(Path::new("/a b/ü#.txt")), "file:///a%20b/%C3%BC%23.txt");
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;

    fn matches(query: &str) -> Option<bool> {
        web_sys::window()?.match_media(query).ok()??.matches().into()
    }

    pub fn preferences() -> Preferences {
        Preferences {
            appearance: matches("(prefers-color-scheme: dark)").map(|d| if d { Appearance::Dark } else { Appearance::Light }),
            accent: None,
            reduced_motion: matches("(prefers-reduced-motion: reduce)"),
            increased_contrast: matches("(prefers-contrast: more)"),
        }
    }

    pub fn open_url(url: &str) -> Result<()> {
        let w = web_sys::window().ok_or(NativeError::Unsupported)?;
        match w.open_with_url_and_target(url, "_blank") {
            Ok(Some(_)) => Ok(()),
            _ => Err(NativeError::Denied("the browser blocked the new tab".into())),
        }
    }

    pub fn open_path(_: &Path) -> Result<()> {
        Err(NativeError::Unsupported)
    }

    pub fn reveal(_: &Path) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux", target_arch = "wasm32")))]
mod imp {
    use super::*;

    pub fn preferences() -> Preferences {
        Preferences::default()
    }
    pub fn open_url(_: &str) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn open_path(_: &Path) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn reveal(_: &Path) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_links_are_opened() {
        for ok in ["https://example.com/a?b=c", "http://localhost:8080", "mailto:someone@example.com", "HTTPS://EXAMPLE.COM"] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "myapp://x",
            "https:",
            "https:no-slashes",
            "https://a b",
            "https://a\nb",
            "",
            "no scheme",
            "C:\\Windows\\notepad.exe",
        ] {
            assert!(matches!(open_url(bad), Err(NativeError::Invalid(_))), "{bad:?}");
        }
    }

    #[test]
    fn missing_files_are_not_handed_to_the_os() {
        assert!(matches!(reveal(Path::new("/definitely/not/here/ashlar")), Err(NativeError::Invalid(_))));
        assert!(matches!(open_path(Path::new("/definitely/not/here/ashlar")), Err(NativeError::Invalid(_))));
    }

    #[test]
    fn preferences_are_read_without_failing() {
        let p = preferences();
        if cfg!(target_os = "macos") {
            assert!(p.appearance.is_some() && p.reduced_motion.is_some() && p.accent.is_some(), "{p:?}");
        }
        let json = serde_json::to_value(p).unwrap();
        assert!(json.get("reducedMotion").is_some());
    }
}
