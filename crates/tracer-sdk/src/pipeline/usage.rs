//! Usage events: which ones the host listens to, and the limits on what one event may carry.

use std::collections::{BTreeMap, HashSet};

use tracer_proto::{Event, MAX_PROP_STR_LEN, MAX_PROPS, PropValue, validate_name};

use crate::Prop;
use crate::config::Config;

pub(super) const DROP_INVALID_ID: &str = "invalid_id";
pub(super) const DROP_PROP: &str = "prop_dropped";

/// What the host chose to record, as two small sets built once at startup.
#[derive(Debug, Default)]
pub(super) struct Listening {
    /// Declared events that are listened to, whether picked one by one or through their category.
    events: HashSet<String>,
    /// Categories listened to as a whole. Ids outside the catalog are accepted only under these.
    categories: HashSet<String>,
}

impl Listening {
    pub(super) fn new(config: &Config) -> Self {
        let categories: HashSet<String> = config.listen_categories.iter().cloned().collect();
        let events = config
            .catalog
            .events
            .iter()
            .filter(|event| categories.contains(&event.category) || config.listen_events.contains(&event.name))
            .map(|event| event.name.clone())
            .collect();
        Self { events, categories }
    }

    /// The cheap check `track` makes before anything is built or queued.
    pub(super) fn wants(&self, name: &str, category: &str, dynamic: bool) -> bool {
        if dynamic { self.categories.contains(category) } else { self.events.contains(name) }
    }

    /// The listened categories, sorted, for `session_start`.
    pub(super) fn categories(&self) -> Vec<String> {
        let mut categories: Vec<String> = self.categories.iter().cloned().collect();
        categories.sort();
        categories
    }
}

/// Builds the wire event, enforcing the limits. `Err` names the reason the whole event is dropped; the
/// number returned with `Ok` is how many properties were left out for breaking a limit.
pub(super) fn build_event(
    name: &'static str,
    category: &'static str,
    dynamic: bool,
    props: &[(&'static str, Prop)],
) -> Result<(Event, u64), &'static str> {
    if dynamic && validate_name(name).is_err() {
        return Err(DROP_INVALID_ID);
    }
    let mut kept = BTreeMap::new();
    let mut dropped = 0;
    for (key, prop) in props {
        let value = match *prop {
            Prop::Bool(value) => Some(PropValue::Bool(value)),
            Prop::Int(value) => Some(PropValue::Int(value)),
            Prop::Float(value) => value.is_finite().then_some(PropValue::Float(value)),
            Prop::Str(value) => (value.len() <= MAX_PROP_STR_LEN).then(|| PropValue::Str(value.to_owned())),
        };
        match value {
            Some(value) if validate_name(key).is_ok() && (kept.len() < MAX_PROPS || kept.contains_key(*key)) => {
                kept.insert((*key).to_owned(), value);
            }
            _ => dropped += 1,
        }
    }
    Ok((Event { name: name.to_owned(), category: category.to_owned(), dynamic, props: kept }, dropped))
}

#[cfg(test)]
mod tests {
    use tracer_proto::{Catalog, CatalogEvent};

    use super::*;

    fn config() -> Config {
        let mut config = Config::new("key", "host", "1.0.0");
        let event = |name: &str, category: &str| CatalogEvent { name: name.to_owned(), category: category.to_owned() };
        config.catalog =
            Catalog { events: vec![event("file.open", "file"), event("file.save", "file"), event("edit.undo", "edit"), event("view.zoom", "view")] };
        config
    }

    #[test]
    fn nothing_is_wanted_until_the_host_listens() {
        let listening = Listening::new(&config());
        assert!(!listening.wants("file.open", "file", false));
        assert!(!listening.wants("anything", "file", true));
        assert!(listening.categories().is_empty());
    }

    #[test]
    fn categories_and_single_events_select_declared_events() {
        let mut config = config();
        config.listen_categories = vec!["file".to_owned()];
        config.listen_events = vec!["edit.undo".to_owned(), "not.declared".to_owned()];
        let listening = Listening::new(&config);
        assert!(listening.wants("file.open", "file", false));
        assert!(listening.wants("file.save", "file", false));
        assert!(listening.wants("edit.undo", "edit", false));
        assert!(!listening.wants("view.zoom", "view", false));
        assert!(!listening.wants("not.declared", "edit", false));
        assert_eq!(listening.categories(), vec!["file".to_owned()]);
    }

    #[test]
    fn ids_outside_the_catalog_need_their_whole_category() {
        let mut config = config();
        config.listen_categories = vec!["file".to_owned()];
        config.listen_events = vec!["edit.undo".to_owned()];
        let listening = Listening::new(&config);
        assert!(listening.wants("file.export_pdf", "file", true));
        assert!(!listening.wants("edit.redo", "edit", true));
    }

    #[test]
    fn listening_without_a_catalog_selects_no_declared_event() {
        let mut config = Config::new("key", "host", "1.0.0");
        config.listen_categories = vec!["file".to_owned()];
        assert!(!Listening::new(&config).wants("file.open", "file", false));
    }

    #[test]
    fn a_bad_dynamic_id_drops_the_event() {
        assert_eq!(build_event("File Open", "file", true, &[]).err(), Some(DROP_INVALID_ID));
        assert_eq!(build_event("", "file", true, &[]).err(), Some(DROP_INVALID_ID));
        let (event, dropped) = build_event("file.export_pdf", "file", true, &[]).unwrap();
        assert!(event.dynamic && event.props.is_empty());
        assert_eq!(dropped, 0);
    }

    #[test]
    fn properties_over_a_limit_are_left_out_and_counted() {
        const LONG: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const EXACT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!((LONG.len(), EXACT.len()), (MAX_PROP_STR_LEN + 1, MAX_PROP_STR_LEN));
        let props = [
            ("ok", Prop::Bool(true)),
            ("count", Prop::Int(3)),
            ("ratio", Prop::Float(0.5)),
            ("kind", Prop::Str(EXACT)),
            ("too_long", Prop::Str(LONG)),
            ("Bad Key", Prop::Int(1)),
            ("not_finite", Prop::Float(f64::NAN)),
        ];
        let (event, dropped) = build_event("file.open", "file", false, &props).unwrap();
        assert_eq!(dropped, 3);
        assert_eq!(event.props.keys().map(String::as_str).collect::<Vec<_>>(), vec!["count", "kind", "ok", "ratio"]);
        assert_eq!(event.props.get("kind"), Some(&PropValue::Str(EXACT.to_owned())));
        assert!(!event.dynamic);
    }

    #[test]
    fn at_most_max_props_are_kept() {
        const KEYS: [&str; 20] = [
            "p00", "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09", "p10", "p11", "p12", "p13", "p14", "p15", "p16", "p17", "p18",
            "p19",
        ];
        let props: Vec<(&'static str, Prop)> = KEYS.iter().map(|key| (*key, Prop::Int(1))).collect();
        let (event, dropped) = build_event("file.open", "file", false, &props).unwrap();
        assert_eq!(event.props.len(), MAX_PROPS);
        assert_eq!(dropped, 4);
        assert!(event.props.contains_key("p15") && !event.props.contains_key("p16"));
    }
}
