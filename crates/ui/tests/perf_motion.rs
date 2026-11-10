//! Motion integration costs. Run in release:
//! `cargo test -p ashlar-ui --release --test perf_motion -- --ignored --nocapture --test-threads=1`.

use std::time::Instant;

use ashlar_motion::{Spring, tokens};
use ashlar_ui::motion;
use ashlar_ui::wake::{Reason, Scheduler};
use egui::Id;

const N: usize = 1000;
const DT: f64 = 1.0 / 60.0;
const PASSES: usize = 200;

/// Runs one pass at time `t`; `f` returns the microseconds it measured.
fn pass(ctx: &egui::Context, t: f64, mut f: impl FnMut(&egui::Context) -> f64) -> f64 {
    let input = egui::RawInput { time: Some(t), predicted_dt: DT as f32, ..Default::default() };
    let mut us = 0.0;
    ctx.run_ui(input, |ui| us = f(ui.ctx())).textures_delta.clear();
    us
}

/// Times `N` value calls toward `target`, in microseconds.
fn calls(ctx: &egui::Context, ids: &[Id], target: f32, curve: Spring) -> f64 {
    let start = Instant::now();
    let mut sum = 0.0_f32;
    for id in ids {
        sum += motion::f32(ctx, *id, target, curve);
    }
    std::hint::black_box(sum);
    start.elapsed().as_secs_f64() * 1e6
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

fn ids() -> Vec<Id> {
    (0..N).map(|i| Id::new("perf_motion").with(i)).collect()
}

#[test]
#[ignore]
fn perf_motion_value_1k_ids() {
    let ctx = egui::Context::default();
    let ids = ids();
    // a long spring keeps every value moving for the whole run
    let slow = Spring::new(30.0, 0.0);
    pass(&ctx, 0.0, |c| calls(c, &ids, 0.0, slow));
    let times: Vec<f64> = (1..=PASSES).map(|k| pass(&ctx, k as f64 * DT, |c| calls(c, &ids, 1000.0, slow))).collect();
    assert_eq!(motion::active(&ctx), N);
    println!("bench: motion_value_1k_ids {:.1} us", median(times));
}

#[test]
#[ignore]
fn perf_motion_settled_1k_ids() {
    let ctx = egui::Context::default();
    let ids = ids();
    let snap = tokens::SNAP.enter;
    let times: Vec<f64> = (0..PASSES).map(|k| pass(&ctx, k as f64 * DT, |c| calls(c, &ids, 5.0, snap))).collect();
    assert_eq!(motion::active(&ctx), 0);
    println!("bench: motion_settled_1k_ids {:.1} us", median(times));
}

#[test]
#[ignore]
fn perf_motion_idle_wakes_after_settle() {
    let ctx = egui::Context::default();
    let ids = ids();
    let snap = tokens::SNAP.enter;
    let requests = || Scheduler::of(&ctx).stats().requests_for(Reason::ANIMATION);
    pass(&ctx, 0.0, |c| calls(c, &ids, 0.0, snap));
    let mut k = 1;
    while motion::active(&ctx) > 0 || k == 1 {
        pass(&ctx, k as f64 * DT, |c| calls(c, &ids, 100.0, snap));
        k += 1;
        assert!(k < 600, "values never settled");
    }
    let before = requests();
    for j in 0..100 {
        pass(&ctx, (k + j) as f64 * DT, |c| calls(c, &ids, 100.0, snap));
    }
    let wakes = requests() - before;
    println!("bench: idle_wakes_after_settle {wakes} wakes");
    assert_eq!(wakes, 0);
}

/// The yardstick for `motion_settled_1k_ids`: one plain context-data lookup per id.
#[test]
#[ignore]
fn perf_motion_ctx_data_lookup_1k_ids() {
    let ctx = egui::Context::default();
    let ids = ids();
    let lookups = |c: &egui::Context| {
        let start = Instant::now();
        let mut sum = 0.0_f32;
        for id in &ids {
            sum += c.data_mut(|d| *d.get_temp_mut_or_default::<f32>(*id));
        }
        std::hint::black_box(sum);
        start.elapsed().as_secs_f64() * 1e6
    };
    let times: Vec<f64> = (0..PASSES).map(|k| pass(&ctx, k as f64 * DT, lookups)).collect();
    println!("bench: ctx_data_lookup_1k_ids {:.1} us", median(times));
}
