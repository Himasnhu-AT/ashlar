# Background jobs

Long work (an export, an import, indexing) runs as a job on a worker thread, so the UI never
blocks. A job:

- works on a snapshot moved into it (an `Arc` of the document, a path), never on live state;
- reports progress and checks for cancellation through a `Progress`;
- runs under `catch_unwind`, so a panic fails the job and nothing else;
- finishes with an `Apply`: a closure the UI thread runs on the session. Edits made there are one
  undo step.

## Starting one from a view

```rust,ignore
use ashlar::prelude::*;

if ui.button("Count words").clicked() {
    let doc = cx.snapshot();
    cx.jobs.spawn("count", "Counting words", doc, |doc, progress| {
        let total = doc.notes.len() as u64;
        let mut words = 0;
        for (i, n) in doc.notes.iter().enumerate() {
            if !progress.steps(i as u64, total) {
                return Err("cancelled".into()); // the job was cancelled
            }
            words += n.body.split_whitespace().count();
        }
        Ok(apply(move |s: &mut Session<Notes>| {
            s.toast(ToastLevel::Info, format!("{words} words"));
            Ok(json!(words))
        }))
    });
}
```

`spawn(kind, label, input, work)` returns a `JobId`. `work` gets the input and a `&Progress`:

| `Progress` call | What it does |
| --- | --- |
| `set(fraction)` | Report 0..=1; returns `false` once the job was cancelled |
| `steps(done, total)` | The same, as steps |
| `message(text)` | Say what the job is doing ("Reading frame 12 of 300") |
| `report(fraction, text)` | Both at once |
| `is_cancelled()`, `token()` | Check for cancellation, or hand a `CancelToken` to other code |

Return `Err(message)` to fail the job; the runtime shows the message as an error toast.

## Variants

`cx.jobs.job(kind, label)` describes a job before starting it:

- `.here()` runs it on the calling thread, now (a command that needs the result at once, a CLI).
  It is still listed, reports progress and runs under `catch_unwind`.
- `.spawn_streaming(input, work)` also gets an `Updates` handle, to post partial results while it
  runs (search hits as they are found).
- `.spawn_stepped(input, step)` splits the work into steps. On the web, where there are no
  threads, stepped jobs advance a few steps per frame so a frame never freezes.

## Watching and cancelling

`cx.jobs.list()` describes every running job (kind, label, progress, message) for a progress
panel; `cx.jobs.is_busy()` says whether any is running; `cx.jobs.cancel(id)` and
`cx.jobs.cancel_all()` cancel. `status_bar::progress` draws a compact progress indicator.

Agents see jobs too: `app.stats` over the control port includes them.

## Idle cost

A job wakes the UI only when it starts, moves its progress by more than half a percent, or
finishes. An app with no running jobs does no work for them.

Jobs live in the `ashlar-jobs` crate, which is headless and can be used without the rest. See
[docs.rs/ashlar-jobs](https://docs.rs/ashlar-jobs).
