//! Windows: toasts through `ToastNotificationManager`.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, PoisonError};

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::core::HSTRING;
use windows_registry::CURRENT_USER;

use super::{Click, Prepared, app_identity, deliver, toast_tag, toast_xml};
use crate::{NativeError, Result};

/// Every toast from this app is in this group, so tags only need to be unique within it.
const GROUP: &str = "ashlar";
/// Toasts kept alive so their click handlers stay registered.
const KEEP: usize = 32;

static REGISTERED: Mutex<Option<String>> = Mutex::new(None);
static LIVE: Mutex<VecDeque<(String, ToastNotification)>> = Mutex::new(VecDeque::new());

fn os(e: impl std::fmt::Display) -> NativeError {
    NativeError::Os(format!("toast: {}", crate::os_text(e)))
}

/// Register the AppUserModelID once (unpackaged apps need it under HKCU) and return it.
fn app_id() -> Result<String> {
    let mut registered = REGISTERED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(id) = registered.as_ref() {
        return Ok(id.clone());
    }
    let (id, name) = app_identity();
    let key = CURRENT_USER.create(format!(r"Software\Classes\AppUserModelId\{id}")).map_err(os)?;
    key.set_string("DisplayName", &name).map_err(os)?;
    let hid = HSTRING::from(id.as_str());
    // SAFETY: `hid` is a valid, NUL-terminated wide string that outlives the call.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&hid) } {
        log::info!("notifications: could not set the process app id ({})", crate::os_text(e));
    }
    *registered = Some(id.clone());
    Ok(id)
}

fn com() {
    // SAFETY: called before any WinRT use on this thread; a thread already in another apartment
    // returns an error, which is fine because WinRT works there too.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

pub fn show(p: &Prepared) -> Result<()> {
    let app = HSTRING::from(app_id()?.as_str());
    com();
    let doc = XmlDocument::new().map_err(os)?;
    doc.LoadXml(&HSTRING::from(toast_xml(p))).map_err(os)?;
    let toast = ToastNotification::CreateToastNotification(&doc).map_err(os)?;
    let tag = toast_tag(&p.id);
    // A toast with the same tag and group replaces the earlier one.
    toast.SetTag(&HSTRING::from(tag.as_str())).map_err(os)?;
    toast.SetGroup(&HSTRING::from(GROUP)).map_err(os)?;
    let click = Click { id: p.id.clone(), info: p.info.clone() };
    let handler = TypedEventHandler::new(move |_, _| {
        // No panic may unwind into the toast platform.
        let _ = catch_unwind(AssertUnwindSafe(|| deliver(click.clone())));
        Ok(())
    });
    toast.Activated(&handler).map_err(os)?;
    ToastNotificationManager::CreateToastNotifierWithId(&app).map_err(os)?.Show(&toast).map_err(os)?;
    let mut live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    live.retain(|(t, _)| *t != tag);
    live.push_back((tag, toast));
    while live.len() > KEEP {
        live.pop_front();
    }
    Ok(())
}

pub fn remove(ids: &[String]) -> Result<()> {
    let app = HSTRING::from(app_id()?.as_str());
    com();
    let history = ToastNotificationManager::History().map_err(os)?;
    let group = HSTRING::from(GROUP);
    let tags: Vec<String> = ids.iter().map(|id| toast_tag(id)).collect();
    for tag in &tags {
        history.RemoveGroupedTagWithId(&HSTRING::from(tag.as_str()), &group, &app).map_err(os)?;
    }
    LIVE.lock().unwrap_or_else(PoisonError::into_inner).retain(|(t, _)| !tags.contains(t));
    Ok(())
}

/// Clicks arrive through each toast's `Activated` handler while the app runs.
pub fn listen() -> Result<()> {
    Ok(())
}
