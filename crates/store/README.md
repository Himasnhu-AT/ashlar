# ashlar-store

Documents for ashlar apps: `Arc` snapshots, revisions, undo history, versioned file formats with migrations, and autosave with crash recovery. Headless, no UI dependencies, builds for `wasm32-unknown-unknown` (the background autosave writer is native only).

## Store

```rust
let mut store: Store<Drawing> = Store::new(Drawing::default());
store.edit("Add Shape", |d| d.shapes.push(shape));
store.edit_merged("Move", "drag-42", |d| d.move_by(dx, dy)); // a whole drag is one undo step
store.try_edit("Delete", |d| d.delete(id))?;                   // on error nothing changes
store.commit("Import", None, built);                            // an Arc<D> built elsewhere, one step
store.undo();
let snap: Arc<Drawing> = store.snapshot();                       // free; send it to a job
if store.is_dirty() { save(&snap)?; store.mark_saved(); }
```

- Edits are copy-on-write: the closure gets a copy (`Arc::make_mut`), so an edit costs `D::clone`. Keep large parts of `D` in `Arc`s (chunks, tracks, pages) and an edit copies only what it touches. A failed or panicking edit leaves document, revision and history untouched.
- Each state has a `Revision` from one process-wide counter. Undo/redo restore the revision of the state they bring back, so revision-keyed caches stay valid and `is_dirty()` is exact (undoing back to the saved state is clean).
- `Store<D, X>` carries extra state `X` (selection, caret) recorded with each step and restored by undo/redo; `extra_mut()` changes it without a step.
- `subscribe_snapshots()` returns a `SnapshotCell<D>` that every change is published to. Audio, render and worker threads read it without locks (`arc-swap`).

## History

`History<D, X>` is usable alone: labelled snapshot stacks, merge keys (`can_merge(label, key)`: same key and same label as the last step), a step limit, an approximate byte limit with a caller-supplied size function (the newest step is always kept), `squash_since(depth, label)` to fold a multi-edit command into one step, and optional branching: undone steps are kept as `Branch`es instead of being dropped, listed with `branches()`, and re-entered with `Store::redo_branch(i)` from the state they grew from.

With branching off and no byte limit, `Store` follows the hand-written linear histories apps use today step for step; `tests/linear_history_parity.rs` checks this against a reference model over 200 random sequences of 300 operations.

## Tracked and Memo

`Tracked<T>` (alias `Versioned<T>`) is a value with its own revision, bumped by `get_mut`/`set`/`make_mut`. Put it on the parts of a document a view caches; key the cache by the part's revision. `Memo<K, V>` is a small LRU of computed values keyed by such revisions.

## Format

```rust
let format = Format::<Drawing>::new("acme.drawing", "Acme Draw 1.2")
    .migrations(vec![v1_to_v2, v2_to_v3]) // schema_version = 3
    .legacy(|v| v.get("shapes").map(|_| 1)); // bare files from before the envelope
format.save(&path, &doc)?;                 // durable atomic write
let loaded = format.load(&path)?;          // migrated, checked
```

Files are `{"format", "schema_version", "generator", "document"}` JSON. Migrations are `fn(Value) -> Result<Value, String>` on the whole envelope, one per schema step. Files from a newer schema are refused (`TooNew`), files of another format too (`WrongFormat`). Corrupt, truncated, oversized and deeply nested input is an error, never a panic. Other envelope keys (view state) come back in `Loaded::extras` and go out with `encode_with`.

## Autosave

`Autosave::start(AutosaveConfig::new(dirs.data.join("Recovery")))` creates this session's directory with an OS lock, a `session.json`, and a ring of `slots` autosave files, and returns the `RecoveryCandidate`s left by sessions that died with unsaved work. Call `tick(now, revision, dirty, || format.encode(&snap))` every frame: it writes only when dirty, the revision is new and `interval` has passed. `mark_clean()` after a save, `close(false)` on a clean exit. `BackgroundWriter` does the encode and write on its own thread.

## Performance

`cargo test -p ashlar-store --release --test perf -- --ignored --nocapture --test-threads=1` (Apple Silicon, release):

| | |
|---|---|
| edit, 100k elements in 1024-element `Arc` chunks | ~1.0 µs |
| edit, 100k elements in one flat `Vec` | ~31 µs |
| undo / redo (1000-step history) | ~0.17 µs |
| `SnapshotCell::publish` | ~160 ns |
| `SnapshotCell::load` / `read` | ~4 ns / ~14 ns |
