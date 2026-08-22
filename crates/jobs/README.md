# ashlar-jobs

Work off the UI thread. A job runs on a worker with a snapshot of what it needs, reports progress,
can be cancelled, runs under `catch_unwind`, and finishes with an `Apply` closure that the UI thread
runs on the app state. That apply is where the document is edited, so the result lands as one undo
step. Headless; builds for wasm32 (jobs run inline there).

```rust
let mut jobs: Jobs<App> = Jobs::new();            // cores − 1 workers (1..=8); inline on wasm32
jobs.set_wake(move || ctx.request_repaint());     // repaint only when a job has news

let snapshot = app.doc.clone();                   // e.g. an Arc of the document
let id = jobs.spawn("analyze", "Detecting scenes", snapshot, |doc, p| {
    for (i, frame) in doc.frames().enumerate() {
        if !p.steps(i as u64, doc.len() as u64) {
            return Err("cancelled".into());       // the job was cancelled
        }
        …
    }
    p.message("found 12 cuts");
    Ok(apply(move |app: &mut App| {
        app.edit("Scene Edit Detection", |d| d.add_cuts(cuts))?;   // one undo step
        Ok(json!({"cuts": 12}))
    }))
});

// every frame (or when woken):
for rec in jobs.drain_ready(&mut app) { app.toast(&rec); }
```

- `Progress`: `set(f)`, `steps(done, total)`, `message(..)`, `report(f, msg)`; the `bool` they return
  turns `false` once the job is cancelled. `token()` gives a `CancelToken` for inner code.
- `drain_ready(&mut state)`: applies finished jobs in the order they finished, each under
  `catch_unwind`; a job cancelled after it finished is not applied. Returns `JobRecord`s.
- `cancel(id)`: a queued job never starts (and is reported at once, even when all workers are busy);
  a running one stops at its next check. `cancel_all()`. Dropping `Jobs` cancels everything and
  never blocks.
- `list()` / `to_json()`: `{"active": [{id, kind, label, status, fraction, message, elapsed_secs,
  eta_secs, cancellable}], "recent": [{id, kind, label, status, message, result, seconds}]}` for the
  progress panel and agents (`status`: queued, running, ready, done, failed, cancelled).
- `wake`: called from the worker on start, on progress moves of at least 0.5 % or a new message, and
  on finish. An idle app does nothing; a reporting job causes at most ~200 repaints.
- Workers: a bounded pool of std threads started on demand (`Jobs::with_workers(n)`); with the
  `rayon` feature jobs go to rayon's global pool instead. `Jobs::inline()` runs jobs inside `spawn`
  (applies still wait for `drain_ready`): what wasm32 uses, and handy in tests.
- Stepped jobs (`spawn_stepped`, a step function returning `Continue` / `Done(apply)` /
  `Failed`): on threads they run straight through; inline (wasm32) `drain_ready` advances them a
  few steps per frame (`set_steps_per_drain`), so long work never freezes a frame. Keep frames
  coming while `needs_frames()` is true.
- `wait_all(timeout)`: for CLIs, scripts and tests (never on a UI thread).
