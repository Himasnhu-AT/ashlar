//! Linux: the `org.freedesktop.Notifications` D-Bus service.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use super::{Click, Prepared, app_identity, deliver};
use crate::{NativeError, Result};

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Notifications remembered for replacing, removing and clicks.
const KEEP: usize = 256;

/// One session connection: servers send `ActionInvoked` to the connection that showed the
/// notification.
static CONN: Mutex<Option<Connection>> = Mutex::new(None);
/// `(server id, our id, info)`, oldest first.
static LIVE: Mutex<VecDeque<(u32, String, BTreeMap<String, String>)>> = Mutex::new(VecDeque::new());
static LISTENING: Mutex<bool> = Mutex::new(false);

fn os(e: zbus::Error) -> NativeError {
    NativeError::Os(format!("no notification service answered ({})", crate::os_text(e)))
}

fn conn() -> Result<Connection> {
    let mut slot = CONN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(c) = slot.as_ref() {
        return Ok(c.clone());
    }
    let c = Connection::session().map_err(os)?;
    *slot = Some(c.clone());
    Ok(c)
}

fn server_id(id: &str) -> Option<u32> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(_, ours, _)| ours == id).map(|(n, _, _)| *n)
}

pub fn show(p: &Prepared) -> Result<()> {
    let conn = conn()?;
    let (app_id, _) = app_identity();
    let app = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from(app_id.as_str()));
    if p.silent {
        hints.insert("suppress-sound", Value::from(true));
    }
    // "default" is the click on the body; servers show no button for it.
    let actions = vec!["default", "Open"];
    let replaces = server_id(&p.id).unwrap_or(0);
    let reply = conn
        .call_method(Some(DEST), PATH, Some(DEST), "Notify", &(app.as_str(), replaces, "", p.title.as_str(), p.body.as_str(), actions, hints, -1i32))
        .map_err(os)?;
    let nid: u32 = reply.body().deserialize().map_err(os)?;
    let mut live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    live.retain(|(n, ours, _)| *ours != p.id && *n != nid);
    live.push_back((nid, p.id.clone(), p.info.clone()));
    while live.len() > KEEP {
        live.pop_front();
    }
    Ok(())
}

pub fn remove(ids: &[String]) -> Result<()> {
    let found: Vec<u32> = {
        let mut live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
        let found = live.iter().filter(|(_, ours, _)| ids.contains(ours)).map(|(n, _, _)| *n).collect();
        live.retain(|(_, ours, _)| !ids.contains(ours));
        found
    };
    if found.is_empty() {
        return Ok(());
    }
    let conn = conn()?;
    for nid in found {
        conn.call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(nid,)).map_err(os)?;
    }
    Ok(())
}

/// Start one thread that turns `ActionInvoked` signals into clicks.
pub fn listen() -> Result<()> {
    let mut listening = LISTENING.lock().unwrap_or_else(PoisonError::into_inner);
    if *listening {
        return Ok(());
    }
    let conn = conn()?;
    let proxy = Proxy::new(&conn, DEST, PATH, DEST).map_err(os)?;
    let signals = proxy.receive_signal("ActionInvoked").map_err(os)?;
    std::thread::Builder::new()
        .name("ashlar-notify".into())
        .spawn(move || {
            let _proxy = proxy;
            for msg in signals {
                let Ok((nid, action)) = msg.body().deserialize::<(u32, String)>() else { continue };
                if action != "default" {
                    continue;
                }
                let hit = LIVE
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .find(|(n, _, _)| *n == nid)
                    .map(|(_, id, info)| Click { id: id.clone(), info: info.clone() });
                if let Some(click) = hit {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deliver(click)));
                }
            }
        })
        .map_err(|e| NativeError::Os(format!("could not start the notification listener ({})", crate::os_text(e))))?;
    *listening = true;
    Ok(())
}
