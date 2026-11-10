#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `app.clock` on the headless driver: a paused animation stands still, a step moves it one
//! frame, and a resume goes on without a jump.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ashlar_app::{App, Driver};
use ashlar_motion::{Curve, Easing};
use serde_json::json;

/// Most motion units a 100 ms frame moves: 100 over a 2 s tween, with room for its easing.
const MAX_STEP: f32 = 10.0;

struct Probe {
    go: Arc<AtomicBool>,
    seen: Arc<Mutex<(f32, f32)>>,
}

fn driver() -> (Driver<i64>, Probe) {
    let go = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new((0.0, 0.0)));
    let (g, s) = (go.clone(), seen.clone());
    let app = App::<i64>::new("clocktest", "0.0.0").view(move |ui, _cx| {
        let on = g.load(Ordering::Relaxed);
        let ctx = ui.ctx().clone();
        let curve = Curve::Tween { duration: 2.0, easing: Easing::LINEAR };
        let m = ashlar_ui::motion::f32(&ctx, egui::Id::new("clock.m"), if on { 100.0 } else { 0.0 }, curve);
        let b = ctx.animate_bool_with_time(egui::Id::new("clock.b"), on, 2.0);
        ui.add_space(m);
        ui.label("moving");
        if let Ok(mut v) = s.lock() {
            *v = (m, b);
        }
    });
    (Driver::new(app.runtime()), Probe { go, seen })
}

fn seen(p: &Probe) -> (f32, f32) {
    *p.seen.lock().unwrap()
}

/// One frame from input (a paused app still runs frames for input).
fn poke(d: &mut Driver<i64>) {
    d.push_event(egui::Event::PointerMoved(egui::pos2(5.0, 5.0)));
    d.advance(1.0 / 60.0);
}

#[test]
fn paused_animation_stands_still_steps_and_resumes_without_a_jump() {
    let (mut d, p) = driver();
    d.frame();
    let methods = d.call("app.version", json!({})).unwrap();
    assert!(methods["methods"].as_array().unwrap().iter().any(|m| m == "app.clock"), "{methods}");

    p.go.store(true, Ordering::Relaxed);
    poke(&mut d);
    d.advance(0.5);
    let r = d.call("app.clock", json!({"pause": true})).unwrap();
    assert_eq!(r["paused"], json!(true), "{r}");
    let t0 = r["time"].as_f64().unwrap();
    let before = seen(&p);
    assert!(before.0 > 0.0 && before.0 < 100.0, "mid-flight: {before:?}");
    assert!(before.1 > 0.0 && before.1 < 1.0, "mid-flight: {before:?}");

    // a second of host time with input in it: nothing moves
    d.advance(1.0);
    poke(&mut d);
    assert_eq!(seen(&p), before, "paused");
    assert_eq!(d.call("app.clock", json!({})).unwrap(), json!({"paused": true, "time": t0}));

    // one 100 ms step: one frame's worth of movement, then still again
    let r = d.call("app.clock", json!({"step_ms": 100.0})).unwrap();
    assert_eq!(r["paused"], json!(true));
    assert!((r["time"].as_f64().unwrap() - (t0 + 0.1)).abs() < 1e-9, "{r} vs {t0}");
    d.advance(0.05);
    let stepped = seen(&p);
    assert!(stepped.0 > before.0 && stepped.0 - before.0 < MAX_STEP, "{before:?} → {stepped:?}");
    // egui's bool animation is linear: 100 ms of 2 s
    assert!((stepped.1 - before.1 - 0.05).abs() < 0.001, "{before:?} → {stepped:?}");
    d.advance(1.0);
    poke(&mut d);
    assert_eq!(seen(&p), stepped, "a step is one frame");

    // resume: time goes on from the frozen value, not from the host's clock
    let r = d.call("app.clock", json!({"resume": true})).unwrap();
    assert_eq!(r["paused"], json!(false));
    d.advance(0.1);
    let after = seen(&p);
    assert!(after.0 > stepped.0, "running again: {stepped:?} → {after:?}");
    assert!(after.0 - stepped.0 < MAX_STEP, "no jump: {stepped:?} → {after:?}");
    assert!(after.1 - stepped.1 < 0.1, "no jump: {stepped:?} → {after:?}");
    d.advance(3.0);
    assert_eq!(seen(&p), (100.0, 1.0), "the animation finishes");
}

#[test]
fn clock_params_are_checked() {
    let (mut d, _p) = driver();
    d.frame();
    assert!(d.call("app.clock", json!({"step_ms": -1})).is_err());
    assert!(d.call("app.clock", json!({"pause": true, "resume": true})).is_err());
    assert_eq!(d.call("app.clock", json!({"resume": true})).unwrap()["paused"], json!(false), "resume while running is harmless");
}

#[test]
fn ui_scripts_hold_the_clock() {
    let (mut d, _p) = driver();
    d.frame();
    let script = "clock pause\nsleep 500\nclock step 16.7\nclock\nclock resume\n";
    let r = d.run_ui(script);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(d.call("app.clock", json!({})).unwrap()["paused"], json!(false));
    assert!(d.run_ui("clock rewind").is_err(), "unknown clock verbs are usage errors");
}
