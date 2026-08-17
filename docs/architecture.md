# Architecture

This is the spec. Code follows it; when they disagree, fix one of them in the same change.

```
host app ──► tracer-sdk ──► bounded queue ──► worker thread ──► disk spool ──► HTTPS batch (gzip)
                                                                                    │
                                              tracer-server: ingest ► SQLite ► rollups ► dashboard + reports
```

## 1. What the SDK must fit

The host apps set these constraints:

- Rust edition 2024, minimum Rust 1.90, `unsafe` forbidden, no `unwrap`/`expect`/`panic` outside tests.
- Immediate-mode GUI with a per-frame callback. Some hosts repaint only on input, so frame *cost* matters more than frame *rate*.
- The `log` facade only. A host may have no logger, or its own. Only one global logger can exist, so the SDK must be able to wrap one.
- No async runtime in the GUI process. Background work is a plain thread; HTTP is blocking.
- Each host also ships a short-lived command-line binary, a stdio protocol server (stdout must stay clean), a web build, and sometimes child processes that can abort.
- One host has a realtime audio thread, where the SDK must never lock, allocate or make a system call.
- Each host chooses its own data directory, so the SDK takes it as a parameter.

## 2. Crates

| Crate | Role | May depend on |
|---|---|---|
| `tracer-proto` | Wire types, constants, name validation. No I/O. | `serde` |
| `tracer-sdk` | The client. | `tracer-proto`, `serde_json`, `log`, `thiserror`, `flate2`, `getrandom`, optional `ureq`, optional `sysinfo` |
| `tracer-server` | The backend binary. | `tracer-proto`, `axum`, `tokio`, `rusqlite`, `maud`, `argon2`, and small helpers |

## 3. SDK

### 3.1 Shape

`client.rs` holds the public `Tracer` handle and `Builder`. Behind it are two halves that meet only at the `Sink` trait (`sink.rs`):

- **`pipeline`** implements `Sink`. It owns consent, identity, the session, the queue, the spool, the worker thread and sending.
- **`signals`** produces data: crash, hang, perf, sys, logs, realtime counters, scrubbing, the local report. It calls `Sink` and nothing else.

The public API in `client.rs` and the `Sink` trait are frozen. Changing either needs a note in `PROGRESS.md`.

`Builder::init` never fails. On the web target, with no data directory, or when the SDK's folder cannot be created, it returns a disabled tracer whose methods do nothing.

### 3.2 Files on disk

Everything lives in `<host data dir>/tracer/`:

| Path | Owner | Content |
|---|---|---|
| `install.json` | pipeline | `{"install_id": "…", "created": ms, "pending_forget": ["…"]}`. `pending_forget` lists earlier install ids whose forget request has not been delivered yet (at most 16); it is absent when empty. |
| `install.lock` | pipeline | Empty. Locked for the moment it takes to change `install.json`. |
| `consent.json` | pipeline | `{"crash": bool, "performance": bool, "usage": bool, "notice_shown": bool}` |
| `selection.json` | pipeline | Catalog hash, listened categories and events, enabled signals for the last run. Also the catalog hashes the server has acknowledged, per app version. |
| `spool/<session_id>/lock` | pipeline | Empty. The process that runs the session holds an exclusive advisory lock on it for its whole life. |
| `spool/<session_id>/header.json` | pipeline | The envelope header for that session (app, install id, device), plus `created`, which orders sessions. |
| `spool/<session_id>/session.json` | pipeline | Marker for the running session: envelope header, start time, time of the last rewrite, last heartbeat totals, error count, `panicked` flag. Removed on clean shutdown. |
| `spool/<session_id>/<seq>.jsonl` | pipeline | Items in segments, one per line: a consent-category byte (`c`, `p`, `u`, or `-` for none) followed by the item as JSON. |
| `spool/<session_id>/breadcrumbs.jsonl` | signals | Recent log lines of that session, rewritten periodically. |
| `outbox.jsonl` | pipeline | Inspect mode only: a capped mirror of every item that was accepted for sending. |

Files are written to a temporary name and renamed. A file that fails to parse is treated as absent, never as an error.

**Several processes, one directory.** A host's GUI, command-line tool and child processes may all run on the same data directory at once, so every session folder has exactly one owner:

- A running process owns `spool/<its session id>/`. It creates the folder at `init`, takes the lock, and appends only there. The operating system releases the lock when the process ends, however it ends.
- A folder whose lock is held belongs to a live process and is left completely alone by everyone else: not sent, not evicted, not purged, not deleted.
- A folder whose lock can be taken is an *orphan*. The process that takes the lock adopts it and keeps the lock until the folder is gone, so no two processes work on the same orphan. Orphans are looked for at startup, before every send, and every `flush_interval`.
- An orphan recorded under an install id other than the current one was forgotten by the user and is deleted unsent. A folder with neither header nor marker is deleted once it is a minute old (younger, it may belong to a process that is still starting). A scan never creates a lock file in a folder younger than that, since the folder may be one its owner is deleting at that moment.
- `install.json`, `consent.json`, `selection.json` and `outbox.jsonl` are shared. Processes that start together on a fresh directory agree on the install id: the file is created with create-if-absent, and the losers read what the winner wrote. The worker re-reads `consent.json` and `install.json` on every tick, so a consent change or a `forget` in one process reaches the others within a tick. Changes to `install.json` are read-modify-write under `install.lock`; changes to `consent.json` start from the file's current content, without a lock.
- Where the filesystem does not support locks (`try_lock` fails with an error other than "would block"), a folder counts as an orphan only when nothing in it has been written for 24 hours, and no unclean exit is ever reported from it: its marker is discarded and its items are sent. A running session rewrites its marker every `heartbeat_interval`, which keeps its folder fresh.

### 3.3 Consent and kill switches

- Default is on for all three categories. `Signal::consent()` maps each signal to its category.
- `DO_NOT_TRACK` (any value other than empty, `0` or `false`) or `TRACER_DISABLED` set the same way disables everything for the process: `allows` is false for every signal, nothing is written to the spool, and no request is made. The consent file is not changed.
- `Sink::allows(signal)` is true only when the host enabled the signal, the kill switches are off, and the category is allowed.
- `set_consent(category, false)` takes effect at once and also deletes that category's unsent items from the spool.
- A choice made in this process is the authority in memory until it has been saved. If `consent.json` cannot be written, the worker tries again on every tick and does not read the file in the meantime, so a file that could not be updated never switches an opt-out back on.
- `notice_pending()` is true until `notice_shown()`. Until then the SDK records and spools as usual (opt-out model) but **sends nothing**: no ingest request and no forget request leaves the machine before the user has seen the host's notice. `notice_shown()` is saved in `consent.json`, so later runs send from the start, and other running processes of the host see it within a tick. A category the user switches off before the notice is acknowledged is deleted from the spool and so is never sent. `flush_blocking` returns false while sending is held and data is waiting. The first time that happens the worker logs one `log::warn!` with target `tracer_sdk`, so a host that forgot to call `notice_shown()` can find out why nothing arrives; the SDK's own logger forwards lines with that target but never keeps them as breadcrumbs.
- `Builder::send_before_notice(true)` lifts the hold, for hosts that show their notice somewhere else, such as an installer. It does not mark the notice as shown.
- Under a kill switch `set_consent` and `notice_shown` change nothing on disk.
- `forget()` deletes this process's unsent items, every orphan, its breadcrumbs and the outbox, sends a `ForgetRequest` for the old install id, and creates a new install id. It also calls `Signals::forget()`, which empties what the signal modules hold in memory from before: the breadcrumb ring, the last crash and the last performance window. Other running processes of the host see the new id on their next tick and do the same to their own session folder and memory; items they had queued under the old id are discarded, which is decided under the spool lock so that none can land under the new header.
- The rotated install record is the authority in memory. If `install.json` cannot be written, the new id and the pending forget stay in memory, the worker tries the save again on every tick, and a file that still names an id this process has rotated away from is treated as behind and overwritten, never taken back.
- The forget request is best effort and never holds up ingest. A network error, a 5xx or a 429 is retried later with its own backoff, in this run and after later launches. A 401 or 403 stops all sending, as for ingest, and keeps the request. Any other 4xx is final: the request is given up and counted as `forget_rejected`.

### 3.4 Identity and ids

Install, session and item ids are 16 random bytes from `getrandom`, as 32 lower-case hex characters. If the random source fails, the SDK falls back to a mix of time, process id and a counter rather than failing.

### 3.5 Session

- One session per process run. `session_start` is submitted during `init`.
- The worker emits `session_heartbeat` every `heartbeat_interval` with running totals, and rewrites the marker, `spool/<session_id>/session.json`. A heartbeat is never written after the session's end.
- Time accounting uses a monotonic clock, sampled once a second by the worker: a second counts as *focused* if the host last said `set_focused(true)` (hosts that never call it count as focused), as *active* if focused and `note_input()` was called within `idle_after`, otherwise *idle*. `note_input` and `set_focused` are single atomic stores.
- Clean shutdown (last `Tracer` clone dropped) writes `session_end` on the calling thread, removes the marker, tries one final send, and releases the session folder. If everything was sent the folder is deleted; otherwise it stays, without a marker, as an orphan that the next process to look simply sends. Status is `crashed` if a panic was seen and the thread dropping the tracer is panicking, otherwise `exited`.
- `errors` counts handled errors and survived panics. The panic hook counts every panic when it happens, because it cannot know then whether the process will survive it. When a session ends `crashed`, by clean shutdown while unwinding or as reported from a marker with `panicked` set, the panic that ended it is taken off again: `crashed` already says so, and it is not counted twice.
- The time one tick may add to the session's totals is capped at five tick intervals, and at no less than a minute. A longer gap means the machine was asleep (some monotonic clocks keep counting through sleep), and that time is not credited as focused, active or idle.
- An orphan (§3.2) that still has its marker belonged to a process that ended uncleanly. The process that adopts it writes into that folder, under **that** session's header: a `session_end` with status `crashed` if the marker's `panicked` flag was set, else `abnormal`, with the last saved totals; and, when not panicked, a `crash` of kind `unclean_exit` carrying `signals::take_saved_breadcrumbs(<that folder>)`. Both carry the time of the marker's last rewrite. It then removes the marker and sends the folder's items like any other.
- An unclean end is reported once: only the lock holder reports, and the marker is removed after the report is on disk. The ids of the two items are derived from the session id, so if the adopter itself dies between those two steps and the report is written again, the server stores it once.
- Short-lived processes (`ProcessKind::Cli`, `Child`) behave the same; they should call `flush_blocking` before exit.
- `Sink::session_summary()` gives the local report the install id, session id, start time, running totals and error count.

### 3.6 Event catalog

The host declares events once:

```rust
tracer_sdk::events! {
    pub enum Category { File = "file", Edit = "edit" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        Undo = "edit.undo" in Edit,
    }
}
```

- A declarative macro; no procedural macro crate.
- It generates both enums (`Copy`, `Clone`, `Debug`, `PartialEq`, `Eq`, `Hash`), and implements `EventCategory` and `Event` with `ALL` lists.
- At build time, through `const` assertions using `tracer_proto::validate_name` and `const_str_eq`, it rejects an invalid name and a duplicate name within either enum. `trybuild` compile-fail tests cover both.
- `listen(category)`, `listen_events(&[..])` and `listen_all::<AppEvent>()` choose what is recorded, and each declares the catalog itself, so their order does not matter and no separate call is needed. `listen(category)` reaches the events through `EventCategory::EVENTS`, which the macro fills in. `Builder::catalog::<AppEvent>()` declares the events without listening to any. A hand-written `EventCategory` that leaves `EVENTS` empty needs that call; `init` logs one warning when categories are listened to and no event is declared.
- `track` drops an event that is not listened to before it reaches the queue. The check must be cheap: a lookup in a small set built at startup.
- `track_id(category, id)` records an id that is not in the catalog, marked `dynamic`, when its category is listened to. The id is validated with `validate_name` at run time; an invalid one is dropped and counted.
- Properties: at most `MAX_PROPS`, keys validated like names, string values `&'static str` of at most `MAX_PROP_STR_LEN` bytes. Anything over a limit is dropped and counted.
- `session_start` always carries `catalog_hash` (a stable hash of the sorted catalog), `listening` and `signals`. It carries the full `catalog` until the server has acknowledged that hash for this app version; an ingest reply with `need_catalog: true` clears the acknowledgement so the next session resends it.

### 3.7 Queue, spool and sending

- `submit` does `try_send` on a bounded channel of `queue_capacity`. When full it drops the item and counts `queue_full`. It never blocks.
- One worker thread drains the queue, appends items to its own session's spool segment, and ticks every `tick_interval` (1 s by default): session accounting, re-reading the shared consent and install files, `Signals::tick`. Every other interval is effectively rounded up to a multiple of the tick.
- `submit_now` serialises the item and appends it to the spool on the calling thread, without going through the queue. It is used from the panic hook, so it must not panic, must tolerate a poisoned lock, and gives up (counting `spool_busy`) rather than wait on a lock the panicking thread may itself hold.
- The spool is capped by `spool_max_bytes` and `spool_max_age`, across all session folders: the folders of other live processes count towards the size, as of the last look at them. To make room a process evicts the oldest segments it may touch (adopted orphans first, oldest session first, then its own oldest) and never anything of another live process; if that is not enough, the new item is dropped. Evicted and expired items are counted as `spool_evicted`.
- The worker sends when `batch_items` are waiting or `flush_interval` has passed, and once at startup for anything earlier runs left. It sends the orphans it has adopted, oldest session first, then its own session. One request per session folder, at most `MAX_ENVELOPE_BYTES` after serialisation; larger backlogs are split. What a request carried is deleted when the server has answered for it, and an orphan's folder is deleted when it is empty.
- Bodies are gzip-compressed JSON.
- Reply handling follows the table in `docs/wire-format.md`. Retries use exponential backoff with jitter, from `retry_min` (5 s) doubling up to `retry_max` (15 min), each wait a random point in the upper half of its step. A `Retry-After` given in seconds is honoured, never for less than `retry_min` (so `Retry-After: 0` does not mean "on every tick") and up to a day; an explicit flush skips the backoff but never the server's `Retry-After`.
- Only an ingest reply that parses says anything about the catalog. A 2xx with an empty or foreign body counts as delivered but neither acknowledges the catalog nor asks for it.
- A segment is read as bytes and each line is judged on its own. A tail without a newline (a write cut short by a crash) and a line that is not valid UTF-8 are skipped and counted as `spool_damaged`; the lines around them are sent. A segment that cannot be read at all is left out of the batch and stays on disk; it does not hold up the sessions after it.
- No interval can overflow the clock: `tick_interval` is clamped to at most an hour and the retry waits to at most a week when the worker starts, and a `flush_blocking` timeout or `shutdown_timeout` too long to add to the clock means "no deadline".
- Drop counts are sent as a `drops` item with the next batch, while at least one consent category is allowed. The reasons are: `queue_full`, `spool_evicted`, `spool_busy`, `item_too_large`, `io_error`, `server_rejected` (a batch the server refused with 400, 413 or 422), `spool_damaged` (a stored line that was cut short or is not valid UTF-8), `forget_rejected`, `invalid_id`, `prop_dropped`, `tick_panicked`, and from the signal modules `breadcrumbs_write`.
- With no endpoint configured the SDK still spools (within the caps), still adopts orphans, and inspect mode still works, but nothing is sent. This holds even when the host supplies its own `Transport`.
- `flush_blocking(timeout)` asks the worker to drain and send now and waits for it. It is true when this process has nothing left that it could send. `shutdown` does the same with `shutdown_timeout`, then stops the worker.
- The built-in transport is `ureq` 3 with rustls, behind the `http` feature, with connect and overall timeouts and redirects off. Hosts and tests can supply their own `Transport`.

### 3.8 Signals

| Module | Behaviour |
|---|---|
| `crash` | A panic hook that chains to the previous hook. It captures message, location, thread name and `Backtrace::force_capture`, scrubs them, attaches breadcrumbs and the last perf window, calls `submit_now` with a `crash` of kind `panic`, then `count_error` and `mark_panicked`. It must never panic itself and must guard against re-entry. `report_handled` submits kind `handled` and calls `count_error`. `report_child_exit` submits kind `child_exit` with exit code or signal, only for an unsuccessful status. **`fatal`** is set where it can be known: `true` on a `child_exit` and on the `unclean_exit` the pipeline writes for a dead session; `false` on `handled` and `hang`; and `false` on a `panic` at capture, because the hook cannot know whether the host catches it. The server marks a panic fatal when its session ends `crashed`. |
| `hang` | A watchdog thread, driven in one of two ways. **Turns** (preferred): `begin_turn()` when the main loop starts handling something and `end_turn()` when it is done, or `turn()` for a guard that does both. A turn open for longer than `hang_threshold` is a hang; the time between turns, when the loop waits for work, never is. This is the only way that works for a loop that runs only on input or that a GUI framework owns. **Heartbeats**: `heartbeat()` once per iteration of a loop that runs continuously; a last beat older than the threshold is a hang. Once a host has begun a turn, heartbeats no longer count for that process. Each call is one atomic update. A second `begin_turn` while a turn is open keeps the original start; `end_turn` without an open turn does nothing. Either way one `crash` of kind `hang` is submitted per hang, with `blocked_ms` as observed so far, and nothing more until the next turn or beat. Watching starts with the first turn or beat, so hosts that call neither get no reports. `pause`/`resume` nest; resuming restarts the open turn or the last beat, and stores that before lifting the pause. If the watchdog finds far more time has passed than its own poll interval, it was suspended with the rest of the machine: it restarts the measurement instead of reporting. |
| `perf` | `frame(duration)` records into a `FrameStats` histogram behind a lock that is held only for the update. `first_frame()` records startup time since `init` once. Every `perf_window` the tick submits one `perf` item if it has anything to report, and keeps a copy as "last perf" for crash reports. |
| `sys` | With the `sysinfo` feature: this process's CPU and memory every `sys_interval`, folded into the perf window (mean CPU, last and peak memory), and static device details once for the envelope header. Without the feature, those fields are absent. |
| `logs` | A `log::Log` that keeps the last `breadcrumb_capacity` lines at or above `breadcrumb_level` in a ring, scrubbed, and forwards every record to the wrapped logger if there is one. `LoggerMode::Install` installs it globally; otherwise the host wraps its own logger with `Tracer::logger`. The ring is saved to `breadcrumbs.jsonl` in the session folder on the tick when it changed. Log lines leave the device only inside a crash. Lines with the SDK's own target, `tracer_sdk`, are forwarded but not kept. |
| `rt` | `Counter` and `Gauge` are `Arc<AtomicU64>`. Registration allocates and locks; recording is one atomic operation. The tick reads them into the perf window (counters as the increase since the last window). |
| `scrub` | Replaces the home directory with `~`, removes the account name from paths, truncates long values (messages 2 KiB, backtraces 32 KiB, log lines 512 bytes). |
| `report` | `write_report(path)` writes a plain-text report: app, version, device, session ids and totals, last perf window, recent breadcrumbs. |

Known limit for 0.1: a backtrace from a release build stripped of symbols contains unresolved frames. Symbolication arrives with native crash support.

### 3.9 Threads

| Thread | Does |
|---|---|
| Host threads | Call `Tracer` methods; each is an atomic store, a short lock, or a `try_send`. |
| `tracer-worker` | Drains the queue, writes the spool, ticks, adopts orphaned session folders, sends. The only thread that touches the network. |
| `tracer-watchdog` | Hang detection only. Started when the `Hang` signal is enabled. |

## 4. Server

### 4.1 Shape

One binary, `tracer-server`, with subcommands:

| Command | Does |
|---|---|
| `serve` | Runs the HTTP server, the rollup timer and the retention timer. |
| `app create <slug> <name>` | Creates an app and prints its first write key. |
| `app list` | Lists apps. |
| `key create <slug>` / `key revoke <key>` | Manages write keys. |
| `admin create <username>` | Creates a dashboard login; reads the password from `TRACER_ADMIN_PASSWORD` or stdin. |
| `prune` | Runs retention once. |
| `rollup [--days N]` | Recomputes rollups once. |

Settings come from the environment (all optional):

| Variable | Default | Meaning |
|---|---|---|
| `TRACER_BIND` | `127.0.0.1:8090` | Listen address |
| `TRACER_DATA_DIR` | `./data` | Folder for the database |
| `TRACER_RETENTION_DAYS` | `90` | Age at which raw rows are pruned |
| `TRACER_RATE_PER_MIN` | `120` | Ingest requests per minute per key and per address; `0` switches the limit off |
| `TRACER_TRUST_PROXY` | unset | When `1`, take the client address from `X-Forwarded-For` |
| `TRACER_ADMIN_PASSWORD` | unset | Password for `admin create` |
| `TRACER_SECURE_COOKIES` | unset | When `1`, mark the login cookie `Secure` |

### 4.2 Storage

- SQLite in WAL mode, schema in `migrations/`, applied at startup and tracked with `PRAGMA user_version`. `0001_init` is the schema; `0002_dashboard` adds indexes for the dashboard queries.
- All access goes through a `Store` trait so a second backend can be added. The SQLite implementation runs on a dedicated writer thread plus read connections; async handlers reach it through `spawn_blocking` or a channel.
- Conventions are in the header of `0001_init.sql`.

### 4.3 Ingest

`POST /v1/ingest`, in order:

1. Look up `x-tracer-key`; unknown or revoked is 401.
2. Rate limit per key and per client address (token bucket, in memory); over the limit is 429 with `Retry-After`. The address is used here and nowhere else.
3. Read at most `MAX_ENVELOPE_BYTES` after gunzip; larger is 413. Parse; failure is 400. A `schema` above the server's is 422.
4. In one transaction:
   - Upsert the install (first/last seen, versions, device).
   - Upsert the session from the envelope header.
   - For each item, skip it if `(app_id, id)` is in `items_seen` (a duplicate), else record the id and store the item by type:
     - `session_start`: store a carried catalog. If the server holds no catalog with this hash for this version, set `need_catalog` in the reply.
     - `session_heartbeat`: raise the session totals (never lower them) and `last_seen`.
     - `session_end`: set totals and status, unless the stored status is already terminal.
     - `event`: validate the name. A non-dynamic name must be in the catalog for that app version when the server holds one; if it holds none, accept and set `need_catalog`. Otherwise reject and count it.
     - `perf`: store.
     - `crash`: compute the fingerprint, upsert the issue, store. A fatal panic also marks the session `crashed` if it is still `ok`.
     - `drops`: add to `daily_drops`.
     - unknown types: skip, count as rejected.
5. Item times are corrected for clock drift by `received_at - sent_at` when the difference is over five minutes, and clamped to at most one day in the future.

**Fingerprint:** kind, plus the first frames of the backtrace that belong to the app (skipping `std`, `core`, `alloc`, panic and backtrace machinery, and with hex addresses and hash suffixes removed); when there is no usable backtrace, the location; when there is neither, the message with digits removed. The issue title is the message's first line, or the kind.

`POST /v1/forget` deletes the install's rows from `installs`, `sessions`, `events`, `perf`, `crashes` and `daily_installs`, and removes issues left with no crashes.

### 4.4 Rollups and retention

- A timer (every five minutes, and on demand) recomputes the `daily_*` tables for the last three days from the raw tables, by delete and insert inside a transaction.
- Retention deletes raw rows older than `TRACER_RETENTION_DAYS` and `items_seen` older than 35 days, then lets SQLite reclaim space. Rollups are kept.
- Sessions still `ok` whose `last_seen` is over a day old are closed as `abnormal`.

### 4.5 Dashboard

Server-rendered HTML with `maud`, charts as inline SVG generated on the server, one small embedded stylesheet, no script build. Login is a username and password (argon2), with a random session token in an `HttpOnly`, `SameSite=Lax` cookie; only its hash is stored. Every state-changing form carries a CSRF token. The login cookie lasts 14 days. The login form itself is protected by a short-lived `SameSite=Strict` cookie paired with a hidden field, and by a limit of 10 attempts a minute per address. Dashboard responses carry `Content-Security-Policy: default-src 'self'` (no inline script or style), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and `Cache-Control: no-store`.

| Page | Shows |
|---|---|
| Overview | Per app: crash-free sessions and installs, daily opens, daily / weekly / monthly active installs, newest issues. |
| Crashes | Issue list with counts, affected installs and versions; filter by kind, version, status. Issue page with backtrace, breadcrumbs, device, last perf window, and resolve / ignore. |
| Performance | Frame-time percentiles (from merged histograms), slow and frozen frame rates, startup time, CPU and memory, by version and by operating system. |
| Usage | Active installs, sessions per day, session length, focused / active / idle time, events ranked by use, and declared events that were never fired. |
| Reports | A stability and usage report for one release: summary, top issues, performance, feature use. Download as HTML, Markdown or JSON. |
| Settings | Apps and write keys. |

Definitions:

- **Crash-free sessions** = 1 − (crashed + abnormal) ÷ sessions that have ended.
- **Crash-free installs** = 1 − installs with a crashed or abnormal session ÷ active installs. Counted per day and summed over the selected range (install-days), because rollups cannot give distinct installs across a range.
- **Daily opens** = sessions started that day with process kind `gui`. Kept in the rollup as `daily_sessions.opens`, since raw sessions are pruned.

## 5. Later phases

See `ROADMAP.md`. The designs above leave room for them: the `Store` trait for Postgres, the `Transport` trait for the web target, and an attachment route for minidumps.
