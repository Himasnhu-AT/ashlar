//! System notifications ("Export finished").
//!
//! - macOS: UNUserNotificationCenter. It works only for an app bundle with a bundle identifier
//!   (an unbundled `cargo run` binary would be terminated by the OS), so outside a bundle this
//!   returns `Unsupported`. The first notification asks the user for permission; delivery is
//!   asynchronous and a refusal is logged, not returned. macOS shows notifications only while
//!   the app is not frontmost unless the app sets a notification-center delegate.
//! - Linux: the `org.freedesktop.Notifications` D-Bus service.
//! - Windows: `Unsupported` for now (toasts need an AppUserModelID registered at install time).

use crate::{NativeError, Result};

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Notification {
    pub title: String,
    #[serde(default)]
    pub body: String,
}

impl Notification {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self { title: title.into(), body: body.into() }
    }

    fn clean(&self) -> Result<(String, String)> {
        let clip = |s: &str, n: usize| s.chars().filter(|c| *c == '\n' || !c.is_control()).take(n).collect::<String>();
        let title = clip(&self.title, 256);
        if title.trim().is_empty() {
            return Err(NativeError::Invalid("a notification needs a title".into()));
        }
        Ok((title, clip(&self.body, 2048)))
    }
}

/// Show a notification.
pub fn notify(n: &Notification) -> Result<()> {
    let (title, body) = n.clean()?;
    crate::guarded("the notification", || imp::notify(&title, &body))
}

#[cfg(target_os = "macos")]
mod imp {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter};

    use super::*;

    /// Whether this process is a bundled app the notification center accepts.
    fn bundled() -> bool {
        let b = NSBundle::mainBundle();
        b.bundleIdentifier().is_some() && b.bundlePath().to_string().ends_with(".app")
    }

    pub fn notify(title: &str, body: &str) -> Result<()> {
        if !bundled() {
            return Err(NativeError::Unsupported);
        }
        let (title, body) = (title.to_string(), body.to_string());
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let done = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
            let _ = std::panic::catch_unwind(|| {
                if !granted.as_bool() {
                    log::info!("notifications: not allowed by the user");
                    return;
                }
                let content = UNMutableNotificationContent::new();
                content.setTitle(&NSString::from_str(&title));
                content.setBody(&NSString::from_str(&body));
                let id = NSString::from_str(&format!(
                    "ashlar-{}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
                ));
                let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
                UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
            });
        });
        center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use super::*;

    pub fn notify(title: &str, body: &str) -> Result<()> {
        let os = |e: zbus::Error| NativeError::Os(format!("no notification service answered ({})", crate::os_text(e)));
        let conn = zbus::blocking::Connection::session().map_err(os)?;
        let app = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(app.as_str(), 0u32, "", title, body, Vec::<&str>::new(), hints, -1i32),
        )
        .map_err(os)?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn notify(_: &str, _: &str) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_required_and_text_is_bounded() {
        assert!(matches!(notify(&Notification::new("  ", "body")), Err(NativeError::Invalid(_))));
        let (t, b) = Notification::new("x".repeat(1000), "line\none\u{7}").clean().unwrap();
        assert_eq!(t.len(), 256);
        assert_eq!(b, "line\none");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unbundled_binary_gets_unsupported_instead_of_being_killed() {
        // `cargo test` binaries are not app bundles.
        assert_eq!(notify(&Notification::new("Test", "")), Err(NativeError::Unsupported));
    }
}
