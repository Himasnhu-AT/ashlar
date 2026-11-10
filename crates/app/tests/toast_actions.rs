//! `App::run_toast_actions`: a toast's action button runs its command through the registry.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ashlar_app::{App, Driver, Session};
use ashlar_command::{CommandError, CommandSpec};
use ashlar_ui::toasts::Toast;
use serde_json::{Value, json};

fn bump(s: &mut Session<i64>, _: &Value) -> Result<Value, CommandError> {
    s.edit("Bump", |n| *n += 1);
    Ok(Value::Null)
}

/// A driver whose view pushes one toast with a registered action and one with an app action,
/// and records the actions left for the view.
fn driver(on: bool, left: Rc<RefCell<Vec<String>>>, doc: Rc<Cell<i64>>) -> Driver<i64> {
    let mut pushed = false;
    let app = App::<i64>::new("toastcmd", "0.0.0").command(CommandSpec::new("count.bump", "Bump", bump)).run_toast_actions(on).view(move |ui, cx| {
        if !std::mem::replace(&mut pushed, true) {
            cx.toasts.push(Toast::info("Own").action("Open", "app.own").sticky());
            cx.toasts.push(Toast::info("Bump?").action("Bump", "count.bump").sticky());
        }
        left.borrow_mut().extend(cx.toasts.take_actions().into_iter().map(|a| a.command));
        doc.set(*cx.doc());
        ui.label(cx.doc().to_string());
    });
    let mut d = Driver::new(app.runtime());
    d.settle(10);
    d
}

fn click(d: &mut Driver<i64>, id: &str) {
    d.call("ui.click", json!({ "id": id })).expect("ui.click");
    d.settle(10);
}

#[test]
fn a_registered_action_runs_and_others_reach_the_view() {
    let (left, doc) = (Rc::new(RefCell::new(Vec::new())), Rc::new(Cell::new(0)));
    let mut d = driver(true, left.clone(), doc.clone());
    click(&mut d, "toast.2.action.0");
    assert_eq!(doc.get(), 1, "count.bump ran");
    click(&mut d, "toast.1.action.0");
    assert_eq!(doc.get(), 1);
    assert_eq!(*left.borrow(), ["app.own"], "an unknown command is left to the view");
}

#[test]
fn off_by_default_every_action_goes_to_the_view() {
    let (left, doc) = (Rc::new(RefCell::new(Vec::new())), Rc::new(Cell::new(0)));
    let mut d = driver(false, left.clone(), doc.clone());
    click(&mut d, "toast.2.action.0");
    assert_eq!(doc.get(), 0);
    assert_eq!(*left.borrow(), ["count.bump"]);
}
