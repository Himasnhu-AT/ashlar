//! Which key runs which command: the commands' default shortcuts plus the user's overrides.
//!
//! An override replaces all of a command's keys (an empty list unbinds it). Lookup prefers a user
//! binding over a default one, then declaration order; [`Keymap::conflicts`] lists every chord that
//! more than one command claims. Chords are compared as the keyboard of the keymap's platform
//! produces them (off macOS, `Mod` and `Ctrl` are one key).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::shortcut::{Chord, Platform, parse_list};
use crate::{Host, Registry};

/// At most this many overridden commands are read from settings.
const MAX_OVERRIDES: usize = 2000;
/// At most this many keys per command.
const MAX_KEYS: usize = 8;
/// Longest command id read from settings.
const MAX_ID_LEN: usize = 128;

/// The user's overrides as saved in settings: command id → its keys (portable form, `[]` = no key).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Overrides {
    #[serde(default)]
    pub bindings: BTreeMap<String, Vec<String>>,
}

/// A chord claimed by more than one command.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflict {
    pub chord: Chord,
    /// In lookup order: the first one wins.
    pub ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct Binding {
    id: String,
    user: bool,
    order: usize,
}

/// Default shortcuts plus user overrides, indexed for lookup by chord.
#[derive(Debug, Clone)]
pub struct Keymap {
    platform: Platform,
    /// Every known command with its default keys, in declaration order.
    defaults: Vec<(String, Vec<Chord>)>,
    positions: HashMap<String, usize>,
    user: BTreeMap<String, Vec<Chord>>,
    index: HashMap<Chord, Vec<Binding>>,
    invalid: Vec<(String, String)>,
}

impl Keymap {
    /// A keymap from `(command id, default shortcut text)` pairs. Commands without a default key
    /// (`None`) are known, so the user can bind them. Unparseable defaults are skipped and listed
    /// in [`Keymap::invalid_defaults`].
    pub fn new<'a>(platform: Platform, defaults: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> Self {
        let mut map = Keymap { platform, defaults: vec![], positions: HashMap::new(), user: BTreeMap::new(), index: HashMap::new(), invalid: vec![] };
        for (id, text) in defaults {
            if map.positions.contains_key(id) {
                continue;
            }
            let chords = match text.map(parse_list) {
                None => vec![],
                Some(Ok(c)) => c,
                Some(Err(_)) => {
                    map.invalid.push((id.to_string(), text.unwrap_or_default().to_string()));
                    vec![]
                }
            };
            map.positions.insert(id.to_string(), map.defaults.len());
            map.defaults.push((id.to_string(), chords));
        }
        map.reindex();
        map
    }

    /// The default keymap of a command table.
    pub fn from_registry<S: Host>(registry: &Registry<S>, platform: Platform) -> Self {
        Self::new(platform, registry.specs().iter().map(|c| (c.id, c.shortcut)))
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Default shortcuts that did not parse: `(command id, text)`.
    pub fn invalid_defaults(&self) -> &[(String, String)] {
        &self.invalid
    }

    fn reindex(&mut self) {
        self.index.clear();
        let user = self.user.iter().map(|(id, c)| (id, c, true));
        let defaults = self.defaults.iter().filter(|(id, _)| !self.user.contains_key(id)).map(|(id, c)| (id, c, false));
        for (id, chords, is_user) in user.chain(defaults) {
            let order = self.positions.get(id).copied().unwrap_or(usize::MAX);
            for chord in chords {
                let list = self.index.entry(chord.on(self.platform)).or_default();
                if !list.iter().any(|b| &b.id == id) {
                    list.push(Binding { id: id.clone(), user: is_user, order });
                }
            }
        }
        for list in self.index.values_mut() {
            list.sort_by_key(|b| (!b.user, b.order));
        }
    }

    /// The command a chord runs (user bindings first, then declaration order).
    pub fn lookup(&self, chord: &Chord) -> Option<&str> {
        self.index.get(&chord.on(self.platform)).and_then(|l| l.first()).map(|b| b.id.as_str())
    }

    /// Every command bound to a chord, in lookup order.
    pub fn commands_for(&self, chord: &Chord) -> Vec<&str> {
        self.index.get(&chord.on(self.platform)).map(|l| l.iter().map(|b| b.id.as_str()).collect()).unwrap_or_default()
    }

    /// The keys that run a command now (its overrides, else its defaults).
    pub fn keys_for(&self, id: &str) -> &[Chord] {
        if let Some(keys) = self.user.get(id) {
            return keys;
        }
        self.positions.get(id).and_then(|&i| self.defaults.get(i)).map(|(_, c)| c.as_slice()).unwrap_or_default()
    }

    /// The default keys of a command.
    pub fn default_keys(&self, id: &str) -> &[Chord] {
        self.positions.get(id).and_then(|&i| self.defaults.get(i)).map(|(_, c)| c.as_slice()).unwrap_or_default()
    }

    /// Whether the command is known to this keymap.
    pub fn knows(&self, id: &str) -> bool {
        self.positions.contains_key(id)
    }

    /// Replace a command's keys (an empty list unbinds it). Returns `false` for an unknown command.
    pub fn bind(&mut self, id: &str, mut keys: Vec<Chord>) -> bool {
        if !self.knows(id) {
            return false;
        }
        keys.truncate(MAX_KEYS);
        keys.dedup();
        if keys.as_slice() == self.default_keys(id) {
            self.user.remove(id);
        } else {
            self.user.insert(id.to_string(), keys);
        }
        self.reindex();
        true
    }

    /// Back to the default keys of one command.
    pub fn reset(&mut self, id: &str) {
        if self.user.remove(id).is_some() {
            self.reindex();
        }
    }

    pub fn reset_all(&mut self) {
        self.user.clear();
        self.reindex();
    }

    /// Commands other than `id` that already use `chord` (to warn before binding).
    pub fn would_conflict(&self, id: &str, chord: &Chord) -> Vec<&str> {
        self.commands_for(chord).into_iter().filter(|c| *c != id).collect()
    }

    /// Every chord claimed by more than one command, sorted by chord.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out: Vec<Conflict> = self
            .index
            .iter()
            .filter(|(_, l)| l.len() > 1)
            .map(|(chord, l)| Conflict { chord: *chord, ids: l.iter().map(|b| b.id.clone()).collect() })
            .collect();
        out.sort_by_key(|c| c.chord);
        out
    }

    /// The overrides, to save in settings.
    pub fn overrides(&self) -> Overrides {
        Overrides { bindings: self.user.iter().map(|(id, keys)| (id.clone(), keys.iter().map(Chord::portable).collect())).collect() }
    }

    /// Apply saved overrides. Never fails: unknown commands and keys that don't parse are skipped
    /// (their count is returned), and the input is capped.
    pub fn apply_overrides(&mut self, overrides: &Overrides) -> usize {
        let mut skipped = 0;
        for (id, keys) in overrides.bindings.iter().take(MAX_OVERRIDES) {
            if id.len() > MAX_ID_LEN || !self.knows(id) {
                skipped += 1;
                continue;
            }
            let mut chords = Vec::new();
            for k in keys.iter().take(MAX_KEYS) {
                match Chord::parse(k) {
                    Ok(c) => chords.push(c),
                    Err(_) => skipped += 1,
                }
            }
            chords.dedup();
            if chords.as_slice() == self.default_keys(id) {
                self.user.remove(id);
            } else {
                self.user.insert(id.clone(), chords);
            }
        }
        skipped += overrides.bindings.len().saturating_sub(MAX_OVERRIDES);
        self.reindex();
        skipped
    }

    /// Read overrides from a settings value (`{"bindings": {...}}`); anything malformed is ignored.
    pub fn apply_overrides_json(&mut self, value: &Value) -> usize {
        let mut o = Overrides::default();
        let Some(b) = value.get("bindings").and_then(Value::as_object) else { return 0 };
        let mut skipped = 0;
        for (id, keys) in b.iter().take(MAX_OVERRIDES) {
            match keys.as_array() {
                Some(a) => {
                    o.bindings.insert(id.clone(), a.iter().take(MAX_KEYS).filter_map(Value::as_str).map(str::to_string).collect());
                }
                None => skipped += 1,
            }
        }
        skipped + self.apply_overrides(&o)
    }
}
