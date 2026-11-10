//! One menu-event handler for the whole process. The menu backend (muda) keeps a single global
//! handler, but the menu bar, tray menus and context menus each want their own activations. Every
//! owner subscribes with an id prefix; an activation goes to the owner with the longest matching
//! prefix (the newest on a tie) with the prefix removed. The menu bar uses the empty prefix.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, Once, PoisonError};

use crate::Waker;

struct Route {
    key: u64,
    prefix: String,
    tx: Sender<String>,
    waker: Waker,
}

static ROUTES: Mutex<Vec<Route>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
static INSTALL: Once = Once::new();

/// A live subscription; dropping it stops the deliveries.
pub(crate) struct Subscription(u64);

impl Drop for Subscription {
    fn drop(&mut self) {
        ROUTES.lock().unwrap_or_else(PoisonError::into_inner).retain(|r| r.key != self.0);
    }
}

/// The index in `prefixes` (oldest first) that owns `id`: the longest prefix `id` starts with,
/// the newest on a tie.
pub(crate) fn owner<'a>(prefixes: impl IntoIterator<Item = &'a str>, id: &str) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for (i, p) in prefixes.into_iter().enumerate() {
        if id.starts_with(p) && best.is_none_or(|(_, len)| p.len() >= len) {
            best = Some((i, p.len()));
        }
    }
    best.map(|(i, _)| i)
}

fn dispatch(id: &str) {
    let routes = ROUTES.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(r) = owner(routes.iter().map(|r| r.prefix.as_str()), id).and_then(|i| routes.get(i)) else { return };
    let rest = id.get(r.prefix.len()..).unwrap_or_default().to_string();
    let (tx, waker) = (r.tx.clone(), r.waker.clone());
    drop(routes);
    if tx.send(rest).is_ok() {
        waker();
    }
}

/// Deliver activations whose id starts with `prefix` (the prefix removed) to `tx`, then call
/// `waker`.
pub(crate) fn subscribe(prefix: impl Into<String>, tx: Sender<String>, waker: Waker) -> Subscription {
    INSTALL.call_once(|| {
        muda::MenuEvent::set_event_handler(Some(|e: muda::MenuEvent| {
            // runs inside the OS's menu callback: never unwind into it
            let _ = std::panic::catch_unwind(|| dispatch(&e.id().0));
        }));
    });
    let key = NEXT.fetch_add(1, Ordering::Relaxed);
    ROUTES.lock().unwrap_or_else(PoisonError::into_inner).push(Route { key, prefix: prefix.into(), tx, waker });
    Subscription(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_prefix_owns_an_id_and_the_newest_wins_a_tie() {
        assert_eq!(owner(["", "\u{1}tray/a/"], "\u{1}tray/a/quit"), Some(1));
        assert_eq!(owner(["", "\u{1}tray/a/"], "save"), Some(0));
        assert_eq!(owner(["", ""], "save"), Some(1));
        assert_eq!(owner(["\u{1}tray/a/"], "save"), None);
        assert_eq!(owner([], "save"), None);
    }

    #[test]
    fn deliveries_strip_the_prefix_and_stop_on_drop() {
        let (tx, rx) = std::sync::mpsc::channel();
        let sub = subscribe("\u{1}test-route/", tx, crate::no_waker());
        dispatch("\u{1}test-route/open");
        assert_eq!(rx.try_recv().ok().as_deref(), Some("open"));
        drop(sub);
        dispatch("\u{1}test-route/open");
        assert!(rx.try_recv().is_err());
    }
}
