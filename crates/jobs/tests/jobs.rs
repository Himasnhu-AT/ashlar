#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use ashlar_jobs::{JobId, Jobs, Status, Step, apply};
use serde_json::{Value, json};

#[derive(Default)]
struct Doc {
    items: Vec<String>,
    undo_steps: usize,
}

impl Doc {
    /// What an app does in an apply: one edit, one undo step.
    fn edit(&mut self, f: impl FnOnce(&mut Vec<String>)) {
        f(&mut self.items);
        self.undo_steps += 1;
    }
}

const T: Duration = Duration::from_secs(20);

fn finish(jobs: &mut Jobs<Doc>, doc: &mut Doc) -> Vec<ashlar_jobs::JobRecord> {
    assert!(jobs.wait_all(T), "jobs did not finish");
    jobs.drain_ready(doc)
}

#[test]
fn a_job_works_on_a_snapshot_and_applies_on_the_ui_thread_as_one_step() {
    let mut jobs = Jobs::with_workers(2);
    let mut doc = Doc::default();
    let snapshot: Arc<Vec<u32>> = Arc::new((1..=100).collect());
    let id = jobs.spawn("sum", "Summing", snapshot, |data, p| {
        let mut sum = 0;
        for (i, x) in data.iter().enumerate() {
            sum += x;
            p.steps(i as u64 + 1, data.len() as u64);
        }
        p.message(format!("summed {}", data.len()));
        Ok(apply(move |d: &mut Doc| {
            d.edit(|items| items.push(format!("sum={sum}")));
            Ok(json!({"sum": sum}))
        }))
    });
    // nothing lands before drain_ready
    assert!(jobs.wait_all(T));
    assert!(doc.items.is_empty());
    assert_eq!(jobs.list()[0].status, Status::Ready);
    let recs = jobs.drain_ready(&mut doc);
    assert_eq!(recs.len(), 1);
    assert_eq!((recs[0].id, recs[0].status, &recs[0].result), (id, Status::Done, &json!({"sum": 5050})));
    assert_eq!(recs[0].message, "summed 100");
    assert_eq!((doc.items.as_slice(), doc.undo_steps), (["sum=5050".to_string()].as_slice(), 1));
    assert!(!jobs.is_busy());
    assert!(jobs.drain_ready(&mut doc).is_empty(), "applied exactly once");
}

#[test]
fn a_panicking_job_fails_alone() {
    let mut jobs = Jobs::with_workers(1);
    let mut doc = Doc::default();
    jobs.spawn("bad", "Bad", (), |(), _| -> Result<ashlar_jobs::Apply<Doc>, String> {
        let v: Vec<u8> = vec![];
        let i = v.len() + 1;
        Err(format!("{}", v[i]))
    });
    jobs.spawn("good", "Good", (), |(), _| Ok(apply(|d: &mut Doc| Ok(json!(d.items.len())))));
    let recs = finish(&mut jobs, &mut doc);
    assert_eq!(recs.len(), 2);
    let bad = recs.iter().find(|r| r.kind == "bad").unwrap();
    assert_eq!(bad.status, Status::Failed);
    assert!(bad.message.contains("internal error"), "{}", bad.message);
    assert_eq!(recs.iter().find(|r| r.kind == "good").unwrap().status, Status::Done);
    // the worker survived: more jobs run
    jobs.spawn("again", "Again", (), |(), _| Ok(apply(|_: &mut Doc| Ok(Value::Null))));
    assert_eq!(finish(&mut jobs, &mut doc)[0].status, Status::Done);
}

#[test]
fn a_panicking_apply_fails_the_job_and_errors_are_reported() {
    let mut jobs = Jobs::inline();
    let mut doc = Doc::default();
    jobs.spawn("a", "Apply panics", (), |(), _| Ok(apply(|_: &mut Doc| panic!("apply bug"))));
    jobs.spawn("b", "Work fails", (), |(), _| Err("file not found".into()));
    jobs.spawn("c", "Apply fails", (), |(), _| Ok(apply(|_: &mut Doc| Err("document changed".into()))));
    let recs = jobs.drain_ready(&mut doc);
    let msgs: Vec<(Status, &str)> = recs.iter().map(|r| (r.status, r.message.as_str())).collect();
    assert_eq!(
        msgs,
        [
            (Status::Failed, "internal error while applying the result (apply bug)"),
            (Status::Failed, "file not found"),
            (Status::Failed, "document changed")
        ]
    );
}

#[test]
fn cancelling_a_running_job_applies_nothing() {
    let mut jobs = Jobs::with_workers(1);
    let mut doc = Doc::default();
    let (started_tx, started_rx) = mpsc::channel();
    let id = jobs.spawn("loop", "Endless", (), move |(), p| {
        started_tx.send(()).unwrap();
        let mut i = 0u64;
        while p.steps(i % 100, 100) {
            i += 1;
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(apply(|d: &mut Doc| {
            d.items.push("should not land".into());
            Ok(Value::Null)
        }))
    });
    started_rx.recv_timeout(T).unwrap();
    assert!(jobs.list()[0].cancellable);
    assert!(jobs.cancel(id));
    let recs = finish(&mut jobs, &mut doc);
    assert_eq!(recs[0].status, Status::Cancelled);
    assert!(doc.items.is_empty());
    assert!(!jobs.cancel(id), "already finished");
    assert!(!jobs.cancel(JobId(999)));
}

#[test]
fn a_queued_job_cancels_at_once_even_when_workers_are_busy() {
    let mut jobs = Jobs::with_workers(1);
    let mut doc = Doc::default();
    let gate = Arc::new(Barrier::new(2));
    let g = gate.clone();
    jobs.spawn("block", "Blocker", (), move |(), _| {
        g.wait();
        Ok(apply(|_: &mut Doc| Ok(Value::Null)))
    });
    let ran = Arc::new(AtomicUsize::new(0));
    let r = ran.clone();
    let queued = jobs.spawn("queued", "Queued", (), move |(), _| {
        r.fetch_add(1, Ordering::SeqCst);
        Ok(apply(|_: &mut Doc| Ok(Value::Null)))
    });
    assert_eq!(jobs.list()[1].status, Status::Queued);
    jobs.cancel(queued);
    let recs = jobs.drain_ready(&mut doc);
    assert_eq!(recs.len(), 1);
    assert_eq!((recs[0].id, recs[0].status), (queued, Status::Cancelled));
    gate.wait();
    let recs = finish(&mut jobs, &mut doc);
    assert_eq!(recs[0].kind, "block");
    assert_eq!(ran.load(Ordering::SeqCst), 0, "a cancelled queued job never starts");
}

#[test]
fn results_apply_in_completion_order() {
    let mut jobs = Jobs::with_workers(3);
    let mut doc = Doc::default();
    // job 0 waits for job 2, job 1 waits for job 0: completion order 2, 0, 1
    let (tx2, rx2) = mpsc::channel::<()>();
    let (tx0, rx0) = mpsc::channel::<()>();
    jobs.spawn("j", "0", (), move |(), _| {
        rx2.recv().unwrap();
        tx0.send(()).unwrap();
        Ok(apply(|d: &mut Doc| {
            d.items.push("0".into());
            Ok(Value::Null)
        }))
    });
    jobs.spawn("j", "1", (), move |(), _| {
        rx0.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        Ok(apply(|d: &mut Doc| {
            d.items.push("1".into());
            Ok(Value::Null)
        }))
    });
    jobs.spawn("j", "2", (), move |(), _| {
        let a = apply(|d: &mut Doc| {
            d.items.push("2".into());
            Ok(Value::Null)
        });
        tx2.send(()).unwrap();
        Ok(a)
    });
    let recs = finish(&mut jobs, &mut doc);
    assert_eq!(doc.items, ["2", "0", "1"]);
    assert_eq!(recs.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["2", "0", "1"]);
    // inline: spawn order
    let mut jobs = Jobs::inline();
    let mut doc = Doc::default();
    for i in 0..5 {
        jobs.spawn("j", i.to_string(), i, |i, _| {
            Ok(apply(move |d: &mut Doc| {
                d.items.push(i.to_string());
                Ok(Value::Null)
            }))
        });
    }
    jobs.drain_ready(&mut doc);
    assert_eq!(doc.items, ["0", "1", "2", "3", "4"]);
}

#[test]
fn concurrency_is_bounded() {
    let mut jobs = Jobs::with_workers(2);
    let mut doc = Doc::default();
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    for i in 0..12 {
        let (running, peak) = (running.clone(), peak.clone());
        jobs.spawn("w", format!("w{i}"), (), move |(), _| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(apply(|_: &mut Doc| Ok(Value::Null)))
        });
    }
    let recs = finish(&mut jobs, &mut doc);
    assert_eq!(recs.len(), 12);
    // with the `rayon` feature the bound is rayon's pool
    let bound = if cfg!(feature = "rayon") { jobs.workers() } else { 2 };
    assert!(peak.load(Ordering::SeqCst) <= bound, "peak {}", peak.load(Ordering::SeqCst));
    assert_eq!(jobs.workers(), bound);
    assert_eq!(jobs.recent().count(), 12);
}

#[test]
fn wake_fires_on_start_progress_and_finish_but_not_for_tiny_moves() {
    let mut jobs: Jobs<Doc> = Jobs::inline();
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    jobs.set_wake(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    jobs.spawn("p", "Progress", (), |(), p| {
        for i in 0..=10_000u64 {
            p.steps(i, 10_000);
        }
        Ok(apply(|_: &mut Doc| Ok(Value::Null)))
    });
    let n = wakes.load(Ordering::SeqCst);
    // start + finish + about 200 visible progress moves, not 10,000
    assert!((150..=260).contains(&n), "{n} wakes");
}

#[test]
fn listing_for_agents() {
    let mut jobs = Jobs::with_workers(1);
    let mut doc = Doc::default();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (at_half_tx, at_half_rx) = mpsc::channel::<()>();
    let id = jobs.spawn("export", "Exporting movie", (), move |(), p| {
        std::thread::sleep(Duration::from_millis(30));
        p.report(0.5, "frame 50 of 100");
        at_half_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        Ok(apply(|_: &mut Doc| Ok(json!({"path": "/tmp/x.mov"}))))
    });
    at_half_rx.recv_timeout(T).unwrap();
    let info = &jobs.list()[0];
    assert_eq!((info.id, info.status, info.fraction, info.message.as_str()), (id, Status::Running, Some(0.5), "frame 50 of 100"));
    let eta = info.eta_secs.unwrap();
    assert!(eta > 0.02 && eta < 10.0, "{eta}");
    let v = jobs.to_json();
    assert_eq!(v["active"][0]["id"], json!(id.0));
    assert_eq!(v["active"][0]["status"], "running");
    assert_eq!(v["active"][0]["kind"], "export");
    go_tx.send(()).unwrap();
    finish(&mut jobs, &mut doc);
    let v = jobs.to_json();
    assert_eq!(v["active"], json!([]));
    assert_eq!(v["recent"][0]["status"], "done");
    assert_eq!(v["recent"][0]["result"], json!({"path": "/tmp/x.mov"}));
}

#[test]
fn stepped_jobs_advance_per_drain_inline_and_run_through_on_threads() {
    fn stepped(jobs: &mut Jobs<Doc>) -> JobId {
        jobs.spawn_stepped("count", "Counting", 0u32, |n, p| {
            *n += 1;
            p.steps(u64::from(*n), 10);
            if *n == 10 {
                let total = *n;
                Step::Done(apply(move |d: &mut Doc| {
                    d.items.push(format!("counted {total}"));
                    Ok(Value::Null)
                }))
            } else {
                Step::Continue
            }
        })
    }
    let mut jobs = Jobs::inline();
    jobs.set_steps_per_drain(3);
    let mut doc = Doc::default();
    stepped(&mut jobs);
    let mut frames = 0;
    while jobs.needs_frames() {
        assert!(jobs.drain_ready(&mut doc).is_empty() || !jobs.needs_frames());
        frames += 1;
        assert!(frames < 10);
    }
    jobs.drain_ready(&mut doc);
    assert_eq!(frames, 4, "10 steps at 3 per frame");
    assert_eq!(doc.items, ["counted 10"]);

    // cancelled inline stepped job
    let id = stepped(&mut jobs);
    jobs.drain_ready(&mut doc);
    jobs.cancel(id);
    let recs = jobs.drain_ready(&mut doc);
    assert_eq!(recs[0].status, Status::Cancelled);
    assert!(!jobs.needs_frames());

    // on threads the same job runs straight through
    let mut jobs = Jobs::with_workers(1);
    let mut doc = Doc::default();
    stepped(&mut jobs);
    assert!(!jobs.needs_frames());
    finish(&mut jobs, &mut doc);
    assert_eq!(doc.items, ["counted 10"]);
}

#[test]
fn dropping_jobs_cancels_and_does_not_block() {
    let mut jobs: Jobs<Doc> = Jobs::with_workers(1);
    let (seen_tx, seen_rx) = mpsc::channel();
    jobs.spawn("loop", "Endless", (), move |(), p| {
        while !p.is_cancelled() {
            std::thread::sleep(Duration::from_millis(1));
        }
        seen_tx.send(()).unwrap();
        Err("stopped".into())
    });
    std::thread::sleep(Duration::from_millis(10));
    drop(jobs);
    seen_rx.recv_timeout(T).expect("the running job saw the cancel");
}
