#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::automation::{Details, Role};
use egui::{RawInput, ViewportId, pos2, vec2};

/// A small egui app driven like a backend would: a frame per step, screenshots delivered on the
/// next frame.
struct App {
    ctx: egui::Context,
    time: f64,
    pending: Vec<Event>,
    saves: usize,
    shortcut_saves: usize,
    wrap: bool,
    name: String,
    submitted: Option<String>,
    show_extra: bool,
    show_button: bool,
    frames_until_extra: Option<u32>,
    last: Option<egui::FullOutput>,
    /// Screenshot requests the "backend" drops (a window that presents no frame).
    drop_shots: usize,
    /// Repaint requests made outside a pass: (delay, pass number), as a backend's callback sees them.
    asked: Arc<std::sync::Mutex<Vec<(Duration, u64)>>>,
}

impl App {
    fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.global_style_mut(|s| s.animation_time = 0.0);
        let asked: Arc<std::sync::Mutex<Vec<(Duration, u64)>>> = Arc::default();
        let a = Arc::clone(&asked);
        ctx.set_request_repaint_callback(move |info| a.lock().unwrap().push((info.delay, info.current_cumulative_pass_nr)));
        install(&ctx);
        App {
            ctx,
            time: 0.0,
            pending: Vec::new(),
            saves: 0,
            shortcut_saves: 0,
            wrap: true,
            name: "old".into(),
            submitted: None,
            show_extra: false,
            show_button: true,
            frames_until_extra: None,
            last: None,
            drop_shots: 0,
            asked,
        }
    }

    fn frame(&mut self) {
        self.time += 1.0 / 60.0;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
            time: Some(self.time),
            events: std::mem::take(&mut self.pending),
            ..Default::default()
        };
        if let Some(n) = &mut self.frames_until_extra {
            if *n == 0 {
                self.show_extra = true;
                self.frames_until_extra = None;
            } else {
                *n -= 1;
            }
        }
        let ctx = self.ctx.clone();
        let mut out = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                if ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S))) {
                    self.shortcut_saves += 1;
                }
                if self.show_button {
                    let r = ui.button("Save");
                    automation::register(ui, &r, "toolbar.save", "Save", Role::Button);
                    if r.clicked() {
                        self.saves += 1;
                    }
                }
                ui.checkbox(&mut self.wrap, "Wrap lines");
                let t = ui.text_edit_singleline(&mut self.name);
                automation::register_with(ui, &t, "form.name", "Name", Role::Field, Details { value: Some(&self.name), selected: None });
                if t.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.submitted = Some(self.name.clone());
                }
                ui.label(format!("{} saves", self.saves));
                if self.show_extra {
                    ui.label("Saved!");
                }
            });
        });
        out.textures_delta.clear();
        for c in out.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default() {
            if let ViewportCommand::Screenshot(user_data) = c {
                if self.drop_shots > 0 {
                    self.drop_shots -= 1;
                    continue;
                }
                let image = Arc::new(egui::ColorImage::new([400, 300], vec![egui::Color32::from_gray(30); 400 * 300]));
                self.pending.push(Event::Screenshot { viewport_id: ViewportId::ROOT, user_data, image });
            }
        }
        self.last = Some(out);
    }

    fn call(&mut self, method: &str, params: Value) -> Reply {
        let rx = call(&self.ctx, method, params);
        for _ in 0..600 {
            self.frame();
            if let Ok(r) = rx.try_recv() {
                return r;
            }
        }
        panic!("{method} not answered")
    }

    /// Like [`App::call`], but runs only the frames something asked for (as an event-driven
    /// backend does), on a virtual clock: `(reply, frames run)`.
    fn call_honestly(&mut self, method: &str, params: Value) -> (Reply, u32) {
        let rx = call(&self.ctx, method, params);
        let mut frames = 0;
        let start = self.time;
        loop {
            if let Ok(r) = rx.try_recv() {
                return (r, frames);
            }
            let done = self.ctx.cumulative_pass_nr();
            let from_output = self.last.as_ref().and_then(|o| o.viewport_output.get(&ViewportId::ROOT)).map_or(Duration::MAX, |v| v.repaint_delay);
            let later = self.asked.lock().unwrap().iter().filter(|(_, nr)| *nr >= done).map(|(d, _)| *d).min().unwrap_or(Duration::MAX);
            let delay = from_output.min(later);
            self.asked.lock().unwrap().clear();
            assert!(delay < Duration::MAX || !self.pending.is_empty(), "{method}: no frame pending and no answer");
            assert!(self.time - start < 30.0, "{method}: not answered in 30 s");
            // `frame` adds 1/60 s itself
            self.time += delay.as_secs_f64().min(30.0);
            self.frame();
            frames += 1;
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        match self.call(method, params) {
            Reply::Ok(v) => v,
            Reply::Err { error, .. } => panic!("{method}: {error}"),
        }
    }

    fn settle(&mut self) {
        for _ in 0..5 {
            self.frame();
        }
    }

    fn ref_of(&mut self, needle: &str) -> String {
        let snap = self.ok("ui.snapshot", json!({}));
        let tree = snap["tree"].as_str().unwrap();
        let line = tree.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle} in\n{tree}"));
        let start = line.find("[ref=").unwrap() + 5;
        let end = line[start..].find(']').unwrap() + start;
        line[start..end].to_string()
    }
}

#[test]
fn snapshot_shows_plain_egui_widgets_and_ashlar_ids_with_stable_refs() {
    let mut app = App::new();
    app.settle();
    let snap = app.ok("ui.snapshot", json!({}));
    let tree = snap["tree"].as_str().unwrap();
    assert!(tree.contains("- button \"Save\" [ref=e"), "{tree}");
    assert!(tree.contains("[id=toolbar.save]"), "{tree}");
    assert!(tree.contains("- checkbox \"Wrap lines\""), "{tree}");
    assert!(tree.contains("[checked]"), "{tree}");
    assert!(tree.contains("- textbox \"Name\""), "{tree}");
    assert!(tree.contains("[id=form.name] [value=\"old\"]"), "{tree}");
    assert!(tree.contains("- label \"0 saves\""), "{tree}");
    let elements = snap["elements"].as_array().unwrap();
    let save = elements.iter().find(|e| e["id"] == "toolbar.save").unwrap();
    assert_eq!(save["role"], "button");
    assert_eq!(save["enabled"], true);
    assert_eq!(save["visible"], true);
    assert_eq!(save["rect"].as_array().unwrap().len(), 4);
    let generation = snap["generation"].as_u64().unwrap();

    app.settle();
    let again = app.ok("ui.snapshot", json!({}));
    assert_eq!(again["generation"].as_u64(), Some(generation + 1));
    let save2 = again["elements"].as_array().unwrap().iter().find(|e| e["id"] == "toolbar.save").unwrap();
    assert_eq!(save["ref"], save2["ref"], "refs are stable across snapshots");

    let interactive = app.ok("ui.snapshot", json!({"interactive": true}));
    let t = interactive["tree"].as_str().unwrap();
    assert!(!t.contains("label"), "{t}");
    assert!(t.contains("checkbox"), "{t}");
}

#[test]
fn click_by_ref_and_text_and_stale_refs() {
    let mut app = App::new();
    app.settle();
    let save = app.ref_of("\"Save\"");
    app.ok("ui.click", json!({"target": {"ref": save}}));
    assert_eq!(app.saves, 1);
    app.ok("ui.click", json!({"ref": format!("@{save}")}));
    assert_eq!(app.saves, 2);
    app.ok("ui.click", json!({"target": {"text": "Save", "role": "button"}}));
    assert_eq!(app.saves, 3);
    app.ok("ui.click", json!({"id": "toolbar.save"}));
    assert_eq!(app.saves, 4);
    app.ok("ui.click", json!({"target": {"text": "wrap lines"}}));
    assert!(!app.wrap);
    assert_eq!(app.ok("ui.get", json!({"target": {"text": "Wrap lines"}, "what": "checked"}))["value"], json!(false));

    app.show_button = false;
    app.settle();
    let Reply::Err { error, detail } = app.call("ui.click", json!({"target": {"ref": save}})) else { panic!("clicked a gone button") };
    assert!(error.starts_with("stale_ref"), "{error}");
    assert_eq!(detail.as_ref().unwrap()["kind"], "stale_ref");
    assert!(detail.unwrap()["generation"].as_u64().is_some());
    assert!(!app.call("ui.click", json!({"target": {"text": "Nothing like this"}})).is_ok());
    assert!(!app.call("ui.click", json!({})).is_ok());
    assert!(!app.call("ui.get", json!({"id": "form.name", "what": "colour"})).is_ok());
}

#[test]
fn fill_submit_press_and_get() {
    let mut app = App::new();
    app.settle();
    app.ok("ui.fill", json!({"target": {"id": "form.name"}, "text": "Ada", "submit": true}));
    assert_eq!(app.name, "Ada");
    assert_eq!(app.submitted.as_deref(), Some("Ada"));
    app.settle();
    assert_eq!(app.ok("ui.get", json!({"id": "form.name", "what": "value"}))["value"], json!("Ada"));
    assert_eq!(app.ok("ui.get", json!({"id": "form.name"}))["value"], json!("Ada"), "text of a field is its content");
    app.ok("ui.fill", json!({"id": "form.name", "text": ""}));
    assert_eq!(app.name, "");
    // typing into the focused field
    app.ok("ui.type", json!({"text": "xyz"}));
    assert_eq!(app.name, "xyz");
    app.ok("ui.press", json!({"keys": "Escape"}));
    app.ok("ui.press", json!({"keys": "Mod+S"}));
    assert_eq!(app.shortcut_saves, 1);
    app.ok("ui.press", json!({"keys": "Mod+S Mod+S"}));
    assert_eq!(app.shortcut_saves, 3);
    assert!(!app.call("ui.press", json!({"keys": "Mod+Bogus"})).is_ok());
    let rect = app.ok("ui.get", json!({"id": "toolbar.save", "what": "rect"}))["value"].clone();
    assert!(rect[2].as_f64().unwrap() > 10.0, "{rect}");
}

#[test]
fn typing_into_a_target_twice_adds_to_the_text() {
    let mut app = App::new();
    app.settle();
    // the first request clicks the field to give it the keyboard; the second finds it focused
    // and does not click again (two clicks this close would select a word to replace)
    app.ok("ui.type", json!({"target": {"id": "form.name"}, "text": "ab"}));
    app.ok("ui.type", json!({"target": {"id": "form.name"}, "text": "cd"}));
    assert_eq!(app.name, "oldabcd");
}

#[test]
fn waits_finish_on_a_later_frame_or_time_out() {
    let mut app = App::new();
    app.settle();
    app.frames_until_extra = Some(10);
    let v = app.ok("ui.wait", json!({"for": "text", "text": "Saved!", "timeout_ms": 2000}));
    assert!(v["elapsed_ms"].as_u64().unwrap() > 0);
    assert!(app.show_extra);
    app.ok("ui.wait", json!({"for": "element", "target": {"text": "Saved!"}}));
    let Reply::Err { error, detail } = app.call("ui.wait", json!({"for": "gone", "target": {"text": "Saved!"}, "timeout_ms": 100})) else { panic!() };
    assert!(error.starts_with("timeout"), "{error}");
    assert_eq!(detail.unwrap()["kind"], "timeout");
    app.ok("ui.wait", json!({"for": "enabled", "target": {"id": "toolbar.save"}}));
    app.ok("ui.wait", json!({"for": "idle", "timeout_ms": 1000}));
    assert!(!app.call("ui.wait", json!({"for": "dawn"})).is_ok());
}

#[test]
fn screenshots_crop_and_annotate() {
    let mut app = App::new();
    app.settle();
    let full = app.ok("ui.screenshot", json!({}));
    assert_eq!((full["width"].as_u64(), full["height"].as_u64()), (Some(400), Some(300)));
    assert!(full["png"].as_str().unwrap().starts_with("iVBORw0KGgo"));
    assert!(full.get("annotations").is_none());

    let rect = app.ok("ui.get", json!({"id": "toolbar.save", "what": "rect"}))["value"].clone();
    let (w, h) = (rect[2].as_f64().unwrap(), rect[3].as_f64().unwrap());
    let shot = app.ok("ui.screenshot", json!({"target": {"id": "toolbar.save"}, "pad": 4, "annotate": true}));
    assert_eq!(shot["width"].as_u64(), Some((w + 8.0).round() as u64), "{shot:?}");
    assert_eq!(shot["height"].as_u64(), Some((h + 8.0).round() as u64));
    let notes = shot["annotations"].as_array().unwrap();
    let save = app.ref_of("\"Save\"");
    let a = notes.iter().find(|a| a["ref"] == save.as_str()).unwrap();
    // relative to the cropped image: the button starts at the padding
    assert_eq!(a["rect"][0].as_f64(), Some(4.0));
    // the legacy form crops by automation id too
    let legacy = app.ok("ui.screenshot", json!({"id": "toolbar.save"}));
    assert_eq!(legacy["width"].as_u64(), Some(w.round() as u64));
    assert!(!app.call("ui.screenshot", json!({"id": "nope"})).is_ok());
    app.ok("ui.highlight", json!({"target": {"id": "toolbar.save"}, "ms": 100}));
}

#[test]
fn inspector_toggles_and_stays_out_of_snapshots() {
    let mut app = App::new();
    app.settle();
    assert!(!is_open(&app.ctx));
    app.pending.push(Event::Key { key: Key::F12, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    app.settle();
    assert!(is_open(&app.ctx));
    section(&app.ctx, "Logs", || vec!["hello".into()]);
    let snap = app.ok("ui.snapshot", json!({}));
    let tree = snap["tree"].as_str().unwrap();
    assert!(!tree.contains("Devtools") && !tree.contains("Elements"), "the inspector is not part of the app:\n{tree}");
    assert!(tree.contains("\"Save\""), "{tree}");
    let st = app.ok("devtools.toggle", json!({}));
    assert_eq!(st["open"], false);
    assert!(!is_open(&app.ctx));
    assert_eq!(app.ok("devtools.toggle", json!({"open": true}))["open"], true);
    assert_eq!(app.ok("devtools.state", json!({}))["open"], true);
    set_open(&app.ctx, false);
    app.settle();
}

#[test]
fn closed_and_idle_asks_for_no_frames() {
    let mut app = App::new();
    app.settle();
    // after a full round of debugger work…
    app.ok("ui.snapshot", json!({}));
    app.ok("ui.click", json!({"id": "toolbar.save"}));
    app.ok("ui.screenshot", json!({"annotate": true}));
    app.settle();
    // …an idle frame asks for nothing more
    app.frame();
    let delay = app.last.as_ref().unwrap().viewport_output.get(&ViewportId::ROOT).unwrap().repaint_delay;
    assert_eq!(delay, Duration::MAX);
    assert!(app.ctx.with_plugin::<Plugin, _>(|p| p.st.inbox.is_empty() && !p.st.script.is_busy() && p.st.shots.is_empty()).unwrap());
}

#[test]
fn foreign_methods_are_handed_back() {
    let ctx = egui::Context::default();
    let (req, _rx) = Request::new("app.frobnicate", json!({}));
    assert!(matches!(handle(&ctx, req), Outcome::NotMine(_)));
    let rx = call(&ctx, "app.frobnicate", json!({}));
    assert!(!rx.try_recv().unwrap().is_ok());
    assert!(handles("ui.snapshot") && !handles("ui.inspect"));
    let _ = pos2(0.0, 0.0);
}

#[test]
fn drives_a_plain_egui_app_in_a_kittest_harness() {
    let mut h = egui_kittest::Harness::new_ui_state(
        |ui, clicks: &mut u32| {
            if ui.button("Plain button").clicked() {
                *clicks += 1;
            }
            ui.label(format!("clicked {clicks}"));
        },
        0,
    );
    install(&h.ctx);
    let ask = |h: &mut egui_kittest::Harness<'_, u32>, method: &str, params: Value| -> Value {
        let rx = call(&h.ctx, method, params);
        for _ in 0..60 {
            h.step();
            if let Ok(r) = rx.try_recv() {
                return r.into_result().unwrap();
            }
        }
        panic!("{method} not answered");
    };
    let snap = ask(&mut h, "ui.snapshot", json!({"interactive": true}));
    let tree = snap["tree"].as_str().unwrap();
    assert!(tree.starts_with("- button \"Plain button\" [ref=e"), "{tree}");
    let r = snap["elements"][0]["ref"].as_str().unwrap().to_string();
    ask(&mut h, "ui.click", json!({"ref": r}));
    assert_eq!(*h.state(), 1);
    ask(&mut h, "ui.wait", json!({"text": "clicked 1", "timeout_ms": 500}));
}

#[test]
fn a_dropped_screenshot_is_asked_again_and_a_window_that_never_draws_fails_fast() {
    let mut app = App::new();
    app.settle();
    app.drop_shots = 2;
    let shot = app.ok("ui.screenshot", json!({}));
    assert_eq!(shot["width"].as_u64(), Some(400));
    app.drop_shots = usize::MAX;
    let Reply::Err { error, .. } = app.call("ui.screenshot", json!({"raise": false})) else { panic!("a screenshot from nowhere") };
    assert!(error.contains("presented no frame"), "{error}");
}

#[test]
fn a_screenshot_that_never_arrives_does_not_spin_the_frame_loop() {
    let mut app = App::new();
    app.settle();
    app.drop_shots = usize::MAX;
    let (r, frames) = app.call_honestly("ui.screenshot", json!({"raise": false}));
    let Reply::Err { error, .. } = r else { panic!("a screenshot from nowhere") };
    assert!(error.contains("presented no frame"), "{error}");
    println!("bench: failed_screenshot_frames {frames}");
    assert!(frames <= 16, "{frames} frames for one failed screenshot");
    // and it gave up within about a second
    app.drop_shots = 0;
    let (r, frames) = app.call_honestly("ui.screenshot", json!({}));
    assert!(r.is_ok());
    println!("bench: screenshot_frames {frames}");
    assert!(frames <= 6, "{frames} frames for one screenshot");
}

#[test]
fn an_offscreen_renderer_needs_no_presented_frame_and_a_failing_one_falls_back() {
    let mut app = App::new();
    app.settle();
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let s2 = Arc::clone(&seen);
    set_offscreen(
        &app.ctx,
        Some(Arc::new(move |f: &OffscreenFrame<'_>| {
            *s2.lock().unwrap() = f.primitives.len();
            let [w, h] = f.size_in_pixels;
            Ok(egui::ColorImage::new([w as usize, h as usize], vec![f.clear; (w * h) as usize]))
        })),
    );
    app.drop_shots = usize::MAX; // the window never presents
    let shot = app.ok("ui.screenshot", json!({"target": {"id": "toolbar.save"}, "annotate": true}));
    assert!(shot["width"].as_u64().unwrap() > 10);
    assert!(*seen.lock().unwrap() > 0, "the frame's shapes were rendered");
    set_offscreen(&app.ctx, Some(Arc::new(|_: &OffscreenFrame<'_>| Err("no gpu".into()))));
    app.drop_shots = 0;
    assert_eq!(app.ok("ui.screenshot", json!({}))["width"].as_u64(), Some(400), "falls back to the backend's capture");
    set_offscreen(&app.ctx, None);
}
