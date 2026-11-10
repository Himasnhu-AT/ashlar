#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `Runtime::log_frames` (`ASHLAR_FRAME_LOG`): a driver run of N frames writes one line per frame
//! with the documented schema.

use std::time::{Duration, Instant};

use ashlar_app::{App, Driver};
use serde_json::Value;

const FRAMES: u64 = 100;

fn lines(path: &std::path::Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))).collect()
}

#[test]
fn a_driver_run_writes_one_line_per_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frames.jsonl");
    let app = App::<i64>::new("framelogtest", "0.0.0").view(|ui, _cx| {
        ui.label("hello");
    });
    let mut rt = app.runtime();
    rt.log_frames(&path).unwrap();
    let mut d = Driver::new(rt);
    for i in 0..FRAMES {
        if i % 2 == 0 {
            d.push_event(egui::Event::PointerMoved(egui::pos2(10.0, 10.0 + i as f32)));
        }
        d.frame();
    }
    // the writer thread flushes whenever its queue runs dry
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut got = lines(&path);
    while got.last().and_then(|l| l["frame"].as_u64()) != Some(FRAMES) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        got = lines(&path);
    }
    drop(d);

    let frames: std::collections::BTreeSet<u64> = got.iter().filter_map(|l| l["frame"].as_u64()).collect();
    assert_eq!(frames, (1..=FRAMES).collect(), "every frame is logged, in order");
    let first_passes = got.iter().filter(|l| l["passes"] == 1).count() as u64;
    assert_eq!(first_passes, FRAMES, "one line per frame (plus one per extra egui pass)");
    for l in &got {
        for key in ["t", "wall_ms"] {
            assert!(l[key].as_f64().is_some_and(|v| v >= 0.0), "{key} in {l}");
        }
        assert!(l["cpu_ms"].is_number() || l["cpu_ms"].is_null(), "{l}");
        assert!(l["phases"]["pass_ms"].is_number(), "{l}");
        assert!(l["reasons"].is_array() && l["commands"].is_array(), "{l}");
        assert!(l["input"].is_boolean() && l["dropped"] == 0, "{l}");
    }
    assert!(got.iter().any(|l| l["input"] == true), "pointer frames are input frames");
    assert!(got.iter().any(|l| l["phases"]["ui_ms"].is_number()), "the runtime's part is timed");
}
