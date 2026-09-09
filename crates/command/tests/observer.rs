#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The run observer on the journal: told about every top-level command that ran (id, ok,
//! duration), never about nested calls or calls refused before running, and never given params.

use std::sync::{Arc, LazyLock, Mutex};

use ashlar_command::{CommandError, CommandRun, CommandSpec, Host, Journal, Registry, always};
use serde_json::{Value, json};

#[derive(Default)]
struct State {
    journal: Journal,
}

impl Host for State {
    type Error = CommandError;
    fn journal(&mut self) -> &mut Journal {
        &mut self.journal
    }
}

fn never(_: &State) -> Result<(), String> {
    Err("not now".into())
}

static REGISTRY: LazyLock<Registry<State>> = LazyLock::new(|| {
    Registry::new(vec![
        CommandSpec::new("doc.inner", "Inner", |_, _| Ok(Value::Null)),
        CommandSpec::new("doc.edit", "Edit", |s: &mut State, _| {
            // a nested call is not reported on its own
            REGISTRY.execute(s, "doc.inner", json!({}))?;
            std::thread::sleep(std::time::Duration::from_millis(5));
            Ok(Value::Null)
        }),
        CommandSpec::new("doc.fail", "Fail", |_, _| Err(CommandError::failed("doc.fail", "no"))),
        CommandSpec { enabled: never, ..CommandSpec::new("doc.disabled", "Disabled", |_, _| Ok(Value::Null)) },
        CommandSpec { enabled: always, ..CommandSpec::new("doc.query", "Query", |_, _| Ok(json!(1))) },
    ])
    // params are not validated here: the observer must never see them anyway
    .with_validation(ashlar_command::Validation::Off)
});

fn registry() -> &'static Registry<State> {
    &REGISTRY
}

fn observed(s: &mut State) -> Arc<Mutex<Vec<CommandRun>>> {
    let seen: Arc<Mutex<Vec<CommandRun>>> = Arc::default();
    let sink = seen.clone();
    s.journal.set_observer(Some(Arc::new(move |r| sink.lock().unwrap().push(r))));
    seen
}

#[test]
fn top_level_runs_are_reported_with_ok_and_duration() {
    let r = registry();
    let mut s = State::default();
    let seen = observed(&mut s);
    r.execute(&mut s, "doc.edit", json!({"path": "/home/someone/secret.txt"})).unwrap();
    assert!(r.execute(&mut s, "doc.fail", json!({})).is_err());
    assert!(r.execute(&mut s, "doc.disabled", json!({})).is_err(), "refused before running");
    assert!(r.execute(&mut s, "no.such", json!({})).is_err(), "unknown");
    r.execute(&mut s, "doc.query", json!({})).unwrap();
    let seen = seen.lock().unwrap();
    let ids: Vec<&str> = seen.iter().map(|r| r.id).collect();
    assert_eq!(ids, ["doc.edit", "doc.fail", "doc.query"]);
    assert_eq!(seen.iter().map(|r| r.ok).collect::<Vec<_>>(), [true, false, true]);
    assert!(seen[0].took >= std::time::Duration::from_millis(5), "{:?}", seen[0].took);
}

#[test]
fn no_observer_no_calls_and_it_can_be_removed() {
    let r = registry();
    let mut s = State::default();
    r.execute(&mut s, "doc.query", json!({})).unwrap();
    let seen = observed(&mut s);
    r.execute(&mut s, "doc.query", json!({})).unwrap();
    s.journal.set_observer(None);
    r.execute(&mut s, "doc.query", json!({})).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(format!("{:?}", s.journal).contains("Observer(none)"));
}
