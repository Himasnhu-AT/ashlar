use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};

use egui::{Id, RawInput, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;

const LABELS: Labels<'static> = Labels {
    printer: "Drucker",
    select_printer: "Drucker wählen",
    refresh: "Aktualisieren",
    copies: "Exemplare",
    printer_default: "Druckerstandard",
    duplex: "Beidseitig",
    one_sided: "Einseitig",
    long_edge: "Lange Kante",
    short_edge: "Kurze Kante",
    grayscale: "Graustufen",
};

fn printer(name: &str, default: bool) -> Printer {
    Printer { name: name.into(), default }
}

/// A discovery worker the test answers by hand: each call leaves its sender in `pending`.
fn manual() -> (Discover, Rc<RefCell<Vec<Sender<Vec<Printer>>>>>) {
    let pending = Rc::new(RefCell::new(Vec::new()));
    let senders = Rc::clone(&pending);
    let discover: Discover = Box::new(move || {
        let (tx, rx) = channel();
        senders.borrow_mut().push(tx);
        rx
    });
    (discover, pending)
}

/// A discovery worker that answers at once with `list`.
fn answering(list: Vec<Printer>) -> Discover {
    Box::new(move || {
        let (tx, rx) = channel();
        let _ = tx.send(list.clone());
        rx
    })
}

fn answer(pending: &Rc<RefCell<Vec<Sender<Vec<Printer>>>>>, list: Vec<Printer>) {
    let tx = pending.borrow_mut().pop().unwrap();
    tx.send(list).unwrap();
}

#[test]
fn discovery_keeps_a_surviving_choice_and_clears_a_stale_one() {
    let ctx = egui::Context::default();
    let (discover, pending) = manual();
    let mut s = State::default();
    s.settings.printer = "Chosen".into();
    assert!(s.refresh(&discover));
    assert!(!s.refresh(&discover), "only one discovery at a time");
    assert!(!s.ready());
    answer(&pending, vec![printer("Default", true), printer("Chosen", false)]);
    s.poll(&ctx);
    assert_eq!(s.settings.printer, "Chosen");
    assert!(s.ready());
    assert_eq!(s.message, None);

    assert!(s.refresh(&discover));
    assert!(!s.ready());
    answer(&pending, Vec::new());
    s.poll(&ctx);
    assert!(!s.ready());
    assert_eq!(s.message, Some(Feedback::NoPrinters));
    assert_eq!(s.settings.printer, "");

    assert!(s.refresh(&discover));
    answer(&pending, vec![printer("Other", false), printer("Default", true)]);
    s.poll(&ctx);
    assert_eq!(s.settings.printer, "Default");
}

#[test]
fn a_vanished_discovery_worker_is_a_failure() {
    let ctx = egui::Context::default();
    let (discover, pending) = manual();
    let mut s = State { printers: vec![printer("Old", true)], ..State::default() };
    s.settings.printer = "Old".into();
    assert!(s.ready());
    s.refresh(&discover);
    pending.borrow_mut().clear();
    s.poll(&ctx);
    assert_eq!(s.message, Some(Feedback::DiscoveryFailed));
    assert!(s.printers.is_empty() && !s.ready() && !s.discovering());
}

#[test]
fn one_submission_at_a_time_and_its_outcomes() {
    let ctx = egui::Context::default();
    let mut s = State { printers: vec![printer("Office", true)], ..State::default() };
    s.settings.printer = "Office".into();

    let (tx, rx) = channel();
    assert!(s.begin(rx).is_ok());
    assert_eq!(s.message, Some(Feedback::Submitting));
    let (_other_tx, other) = channel();
    assert!(s.begin(other).is_err());
    let (discover, _pending) = manual();
    assert!(!s.refresh(&discover), "no discovery while submitting");
    s.poll(&ctx);
    assert!(s.busy() && !s.ready());
    tx.send(Ok("Queue-42".to_string())).unwrap();
    s.poll(&ctx);
    assert_eq!(s.message, Some(Feedback::Submitted("Queue-42".into())));
    assert!(s.ready());

    let (tx, rx) = channel();
    s.begin(rx).unwrap();
    tx.send(Err("Paper jam in tray 2".to_string())).unwrap();
    s.poll(&ctx);
    assert_eq!(s.message, Some(Feedback::Failed("Paper jam in tray 2".into())));

    let (tx, rx) = channel::<Result<String, String>>();
    s.begin(rx).unwrap();
    drop(tx);
    s.poll(&ctx);
    assert_eq!(s.message, Some(Feedback::WorkerStopped));
    assert!(s.ready());
}

#[test]
fn the_first_show_finds_printers() {
    let ctx = egui::Context::default();
    let discover = answering(vec![printer("Only", false)]);
    let mut s = State::default();
    let input = RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0))), ..Default::default() };
    crate::test_support::run(&ctx, input, |ui| s.show(ui, Id::new("print"), &discover, &LABELS));
    assert_eq!(s.settings, Settings { printer: "Only".into(), copies: 1, duplex: None, grayscale: false });
    assert!(s.ready() && !s.busy());
}

#[test]
fn controls_fit_a_narrow_panel_and_lock_while_submitting() {
    let long = "A very long printer name on the third floor next to the kitchen (colour, duplex, stapler)";
    let discover = answering(vec![printer(long, true)]);
    let mut h = Harness::builder()
        .with_size(vec2(220.0, 360.0))
        .build_ui_state(|ui, s: &mut State| s.show(ui, Id::new("print"), &discover, &LABELS), State::default());
    h.step();
    h.step();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(220.0, 360.0));
    {
        let node = h.get_by_label(LABELS.grayscale);
        assert!(screen.contains_rect(node.rect()), "{:?}", node.rect());
        assert!(!node.accesskit_node().is_disabled());
        node.click();
    }
    h.step();
    h.step();
    assert!(h.state().settings.grayscale);

    let (_tx, rx) = channel();
    h.state_mut().begin(rx).unwrap();
    h.step();
    {
        let node = h.get_by_label(LABELS.grayscale);
        assert!(node.accesskit_node().is_disabled());
        node.click();
    }
    h.step();
    h.step();
    assert!(h.state().settings.grayscale);
}
