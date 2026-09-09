//! Command ids as event names, and folding of rapid repeats of one command.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Runs of one command id closer together than this are folded into the next report (a UI pass
/// that runs a query every frame would otherwise flood the queue).
pub const REPEAT_WINDOW: Duration = Duration::from_millis(250);
/// Most distinct command ids remembered (registries hold a few thousand at most).
const MAX_IDS: usize = 4096;

/// Per-id folding of rapid repeats, and each id as a valid event name.
#[derive(Default)]
pub(crate) struct Repeats {
    ids: HashMap<&'static str, Seen>,
}

struct Seen {
    last: Option<Instant>,
    folded: u32,
    name: Option<&'static str>,
}

impl Repeats {
    /// `Some((event name, runs folded into this one))` when this run is reported.
    pub(crate) fn admit(&mut self, id: &'static str, now: Instant) -> Option<(Option<&'static str>, u32)> {
        if !self.ids.contains_key(id) && self.ids.len() >= MAX_IDS {
            return Some((None, 0));
        }
        let seen = self.ids.entry(id).or_insert_with(|| Seen { last: None, folded: 0, name: event_name(id) });
        if seen.last.is_some_and(|t| now.saturating_duration_since(t) < REPEAT_WINDOW) {
            seen.folded = seen.folded.saturating_add(1);
            return None;
        }
        seen.last = Some(now);
        Some((seen.name, std::mem::take(&mut seen.folded)))
    }
}

/// A command id as a tracer event name: `file.openRecent` → `file.open_recent`. Called once per
/// distinct registered id (a bounded set), so keeping the names for the life of the process is a
/// few kilobytes.
pub(crate) fn event_name(id: &str) -> Option<&'static str> {
    let snake = snake_case(id);
    tracer_sdk::proto::validate_name(&snake).ok()?;
    Some(Box::leak(snake.into_boxed_str()))
}

pub(crate) fn snake_case(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 8);
    let mut prev_lower = false;
    for c in id.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_lower = true;
        } else if c == '.' {
            out.push('.');
            prev_lower = false;
        } else {
            out.push('_');
            prev_lower = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ids_become_event_names() {
        assert_eq!(snake_case("file.openRecent"), "file.open_recent");
        assert_eq!(snake_case("app.settings.audioHardware"), "app.settings.audio_hardware");
        assert_eq!(snake_case("edit.undo"), "edit.undo");
        assert_eq!(snake_case("export.toXML"), "export.to_xml");
        assert_eq!(event_name("file.saveAs"), Some("file.save_as"));
        assert_eq!(event_name("9bad"), None);
    }

    #[test]
    fn rapid_repeats_are_folded() {
        let mut r = Repeats::default();
        let t0 = Instant::now();
        assert_eq!(r.admit("view.tick", t0), Some((Some("view.tick"), 0)));
        assert_eq!(r.admit("view.tick", t0 + Duration::from_millis(16)), None);
        assert_eq!(r.admit("view.tick", t0 + Duration::from_millis(32)), None);
        assert_eq!(r.admit("file.save", t0 + Duration::from_millis(40)), Some((Some("file.save"), 0)), "ids are folded separately");
        assert_eq!(r.admit("view.tick", t0 + REPEAT_WINDOW + Duration::from_millis(1)), Some((Some("view.tick"), 2)));
    }
}
