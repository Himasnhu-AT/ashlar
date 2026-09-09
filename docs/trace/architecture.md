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

**Nothing in the SDK's folder is trusted.** Every file is read as possibly damaged or planted: a state file that does not parse is treated as absent and replaced; a file is read only if it is a regular file of plausible size (state files up to 1 MiB, segments up to 8 MiB), so a symbolic link is never followed out of the folder and an oversized file is never loaded into memory; a session folder is only ever a real folder, never a link; a segment far larger than the SDK writes is deleted unread when its folder is adopted and counted as `spool_damaged`. If the SDK's folder, or the running session's folder, is deleted under a running session, the worker puts back the folder, lock, header, marker and the install and consent files on its next pass; what was recorded in between is lost uncounted.

**Durability.** Nothing waits for the disk on a host thread. Spool segments are appended with one write per item and are never flushed with `fsync`: a crash of the process loses nothing that was written, but a power loss or a crash of the operating system can lose the last seconds of items, or leave a cut-off last line, which readers skip. `consent.json` and `install.json` are written by temporary file and rename on the calling thread and then flushed to the disk, with their folder, by the worker within a tick; until then a power loss can bring back the previous consent choice or install id, or an empty file, which reads as absent (consent then starts from the defaults). The marker and `selection.json` are not flushed.

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

Install and session ids are 16 random bytes from `getrandom`, as 32 lower-case hex characters. If the random source fails, the SDK falls back to a mix of time, process id and a counter rather than failing.

Item ids have the same form but are not drawn from the system each time, because that is a system call per item: the first half is random and fixed for the process, the second half is a per-process counter passed through a mixing function that gives every count a different value. They are unique, which is all the server's duplicate check needs. The two items that report an adopted session's unclean end are the exception: their ids are derived from the session id (§3.5).

### 3.5 Session

- One session per process run. `session_start` is submitted during `init`.
- The worker emits `session_heartbeat` every `heartbeat_interval` with running totals, and rewrites the marker, `spool/<session_id>/session.json`. A heartbeat is never written after the session's end.
- Time accounting uses a monotonic clock, sampled once a second by the worker: a second counts as *focused* if the host last said `set_focused(true)` (hosts that never call it count as focused), as *active* if focused and `note_input()` was called within `idle_after`, otherwise *idle*. `note_input` and `set_focused` are single atomic stores.
- Clean shutdown (last `Tracer` clone dropped) writes `session_end` on the calling thread, removes the marker, tries one final send, and releases the session folder. If everything was sent the folder is deleted; otherwise it stays, without a marker, as an orphan that the next process to look simply sends. Status is `crashed` if a panic was seen and the thread dropping the tracer is panicking, otherwise `exited`.
- `errors` counts handled errors and survived panics. The panic hook counts every panic when it happens, because it cannot know then whether the process will survive it. When a session ends `crashed`, by clean shutdown while unwinding or as reported from a marker with `panicked` set, the panic that ended it is taken off again: `crashed` already says so, and it is not counted twice.
- The time one tick may add to the session's totals is capped at five tick intervals, and at no less than a minute. A longer gap means the machine was asleep (some monotonic clocks keep counting through sleep), and that time is not credited as focused, active or idle.
- An orphan (§3.2) that still has its marker belonged to a process that ended uncleanly. The process that adopts it writes into that folder, under **that** session's header: a `session_end` with status `crashed` if the marker's `panicked` flag was set, else `abnormal`, with the last saved totals; and, when not panicked, a `crash` of kind `unclean_exit` carrying `signals::saved_breadcrumbs(<that folder>)`. Both carry the time of the marker's last rewrite, or the present if the marker claims a later time. It then removes the marker, then the saved breadcrumbs, and sends the folder's items like any other. The breadcrumbs are deleted last on purpose: if the adopter dies after reading them and before the report is on disk, the next adopter still finds them.
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

- `submit` does `try_send` on a bounded channel of `queue_capacity`. When full it drops the item and counts `queue_full` with one atomic add. It never blocks and takes no lock.
- `track` hands the worker exactly what it was given: the two `&'static str` names, a timestamp, and the properties copied into one boxed slice (no allocation at all without properties). Checking the limits, building the wire event, assigning the id, serialising and writing are done by the worker. `frame` is three atomic operations on a histogram of atomics. No call on a host thread takes a lock the worker holds across disk or network I/O.
- `Builder::init` does only what must precede the first call: creates the folders, reads the three small state files, takes the session folder's lock, writes header and marker and the `session_start`, and starts the worker. Looking through what earlier runs left behind, however much, is the worker's first job.
- When the worker drains the queue for a flush or at shutdown it takes at most one queue's worth, so host threads that keep the queue full cannot keep it from sending, ticking or answering the flush. At shutdown the drop counts are written to the spool whether or not anything can be sent, so they are not lost with the process.
- One worker thread drains the queue, appends items to its own session's spool segment, and ticks every `tick_interval` (1 s by default): session accounting, re-reading the shared consent and install files, `Signals::tick`. Every other interval is effectively rounded up to a multiple of the tick.
- `submit_now` serialises the item and appends it to the spool on the calling thread, without going through the queue. It is used from the panic hook, so it must not panic, must tolerate a poisoned lock, and gives up (counting `spool_busy`) rather than wait on a lock the panicking thread may itself hold.
- The spool is capped by `spool_max_bytes` and `spool_max_age`, across all session folders: the folders of other live processes count towards the size, as of the last look at them. To make room a process evicts the oldest segments it may touch (adopted orphans first, oldest session first, then its own oldest) and never anything of another live process; if that is not enough, the new item is dropped. Evicted and expired items are counted as `spool_evicted`.
- The worker sends when `batch_items` are waiting or `flush_interval` has passed, and once at startup for anything earlier runs left. It sends the orphans it has adopted, oldest session first, then its own session. One request per session folder, at most `MAX_ENVELOPE_BYTES` after serialisation; larger backlogs are split. What a request carried is deleted when the server has answered for it, and an orphan's folder is deleted when it is empty.
- Bodies are gzip-compressed JSON, at the fastest level: on a 1 MiB batch of events it takes an eighth of the time of the default level for 5% more bytes.
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
| `TRACER_RATE_PER_MIN` | `120` | Requests per minute per client address on the public routes (ingest, forget, health), unknown keys included; `0` switches the limit off. Dashboard sign-in has its own limit, which stays on |
| `TRACER_KEY_RATE_PER_MIN` | `6000` | Ingest requests per minute for one write key over all addresses: a capacity guard far above the per-address limit; `0` switches it off |
| `TRACER_MAX_DB_MB` | `10240` | Ingest answers `507` once the database is larger than this many MiB; the dashboard and sign-in keep working; `0` switches the ceiling off |
| `TRACER_TRUST_PROXY` | unset | When `1`, take the client address from the last `X-Forwarded-For` entry of the last such header line |
| `TRACER_ADMIN_PASSWORD` | unset | Password for `admin create`; read by that command only |
| `TRACER_SECURE_COOKIES` | on | Cookies are `Secure` and the session cookie uses the `__Host-` prefix. Set `0` to allow plain-HTTP use on a local machine |

### 4.2 Storage

- SQLite in WAL mode, schema in `migrations/`, applied at startup and tracked with `PRAGMA user_version`. `0001_init` is the schema; `0002_dashboard` adds indexes for the dashboard queries; `0003_hardening` makes catalogs additive (`catalog_events.hash`), adds `events.declared`, `daily_events.declared`, `sessions.catalog_hash` and `issues.created_day`; `0004_scale` adds the indexes the large-database measurements called for (`sessions_by_version`, the partial `perf_with_metrics`), `daily_active` (daily, weekly and monthly active installs) and `dirty_days`.
- All access goes through a `Store` trait so a second backend can be added.
- **Writes: group commit.** Ingest requests go through a bounded queue (256 envelopes) to one `tracer-writer` thread. It takes whatever has queued up (at most 64 envelopes), stores them in one transaction, commits once and only then answers each caller, so a request is acknowledged only after its data is committed. When the queue is full the answer is `503` with `Retry-After: 1`: memory cannot grow, and the client keeps its data. If a group fails, its envelopes are stored one by one, so a bad envelope cannot take the others down; a failure that concerns the whole database (locked past the busy timeout, disk full, read-only) is reported at once instead (`503` for a lock, `500` otherwise).
- **Durability.** WAL with `synchronous = NORMAL`: a commit is in the log file but not fsynced. A crash or `kill -9` of the process loses nothing that was acknowledged; a power failure or operating-system crash can lose the last moments of acknowledged data (the database stays consistent, never half-written). A separate `tracer-checkpoint` thread folds the log into the database file every second, so the writer never pays for that fsync inside a commit; the log is allowed to grow to about 200 MiB before the writer would checkpoint it itself.
- **Reads** use a small pool of `query_only` connections and never wait for the writer (WAL).
- **Signals.** `SIGINT` and `SIGTERM` both stop the server gracefully: it stops accepting, lets in-flight requests finish (up to 10 s) and exits with status 0.
- Conventions are in the header of `0001_init.sql`.

### 4.3 Ingest

`POST /v1/ingest`, in order:

1. Take one request from the client address's token bucket (in memory, bounded, IPv6 by /64) before anything else, so unknown keys and the health route are limited too; over the limit is 429 with `Retry-After`. The address is used here, for the sign-in limit and nowhere else. A request the address bucket refuses does not touch the key bucket.
2. Look up `x-tracer-key`; unknown or revoked is 401. Then take one request from the key's own bucket (`TRACER_KEY_RATE_PER_MIN`, a capacity guard); over it is 429.
3. When the database is over `TRACER_MAX_DB_MB`, answer 507. The client keeps its data and retries.
4. Read at most `MAX_ENVELOPE_BYTES` after gunzip; larger is 413. Parse; failure is 400. A `schema` above the server's is 422.
5. In one transaction:
   - Upsert the install (first/last seen, versions, device).
   - Upsert the session **only when the envelope carries a session item** (`session_start`, `session_heartbeat` or `session_end`). Crashes, performance windows and events keep their `session_id` as plain text on their own rows, so a user who switched session reporting off never creates a session that the stale closer would turn `abnormal`.
   - For each item, skip it if `(app_id, id)` is in `items_seen` (a duplicate), else store the item by type and record the id:
     - `session_start`: remember the named `catalog_hash` on the session. A carried catalog is stored **next to** the others, keyed by `(app, version, hash)`, never in place of one (at most 8 per version and 200 per app); a hash the server does not hold and did not receive sets `need_catalog`.
     - `session_heartbeat`: raise the session totals (never lower them) and `last_seen`.
     - `session_end`: raise the totals and set the status, unless the stored status is already terminal. When the status is `crashed`, the session's most recent `panic` crash is marked `fatal`.
     - `event`: validate the name. **An event is never refused for being undeclared**: the catalog is client input, and a refused event is deleted by the client. A non-dynamic name is stored with `declared = 1` when it is in any catalog held for that app version (which includes the one the session's `session_start` named), else `declared = 0`. With no catalog held it is `declared = 1` and `need_catalog` is set.
     - `perf`: store.
     - `crash`: compute the fingerprint, upsert the issue, store. At most 20 crashes are taken per envelope (the rest count as rejected). An app creates at most 200 new issues per day; further new fingerprints share one "Too many distinct crashes" issue for that day, so nothing is dropped silently. A `panic` is marked `fatal` when its session has ended `crashed` (in either arrival order; the SDK cannot know at capture time whether the host survives). A fatal panic from the client also marks an `ok` session `crashed`.
     - `drops`: add to `daily_drops`. Each count is clamped to 1,000,000, reasons must be valid names, at most 64 distinct reasons are kept per app per day (the rest count as `other`) and a stored total is capped.
     - unknown types: skip, count as rejected.
6. Item times are corrected for clock drift by `received_at - sent_at` when the difference is over five minutes, then clamped to the retention window in the past and to one hour in the future. Item ids longer than 64 bytes are rejected.

**Fingerprint:** kind, plus the first frames of the backtrace that belong to the app (skipping `std`, `core`, `alloc`, panic and backtrace machinery, and with hex addresses and hash suffixes removed); when there is no usable backtrace, the location; when there is neither, the message with digits removed. The issue title is the message's first line, or the kind.

`POST /v1/forget` deletes the install's rows from `installs`, `sessions`, `events`, `perf`, `crashes` and `daily_installs`, and removes issues left with no crashes. It answers `200 {"ok":true}` whether or not the install existed, so it cannot be used to probe for install ids.

**Connections.** The server runs hyper directly. A client has 15 seconds to send a request's headers, which also bounds an idle keep-alive connection, and at most 1,024 connections are served at once; further clients wait in the accept queue. The body of a request must arrive within 30 seconds.

### 4.4 Rollups and retention

- Ingest marks every (app, day) it stores data for in `dirty_days`. A timer (every five minutes) recomputes the `daily_*` tables for exactly those days, oldest first, and clears each mark unless new data arrived meanwhile; `rollup --days N` recomputes a fixed window by hand. Late data for an old day (a client replaying a spool) is therefore rolled up too.
- One day of one app is built on a private connection that only reads the raw tables and collects the results in an in-memory scratch database; the write is then one short transaction that replaces that day's rows (for the install list only the difference is applied). Ingest is never held up by the aggregation itself.
- `daily_active` holds the daily, weekly and monthly active installs per day. The weekly and monthly figures cost a distinct count over up to 30 days, so they are computed for the newest two days only (older days store `-1`, and readers fall back to counting).
- Per app, only the 50 busiest versions, 20 busiest operating systems and 500 busiest event names keep their own rollup rows; the long tail is folded into `other`, so made-up values cannot grow the rollups.
- `daily_events` is keyed by `declared` as well: declared and undeclared uses of one name are separate rows.
- Retention deletes raw rows older than `TRACER_RETENTION_DAYS`, `daily_drops` and catalogs not seen within that time, and `items_seen` older than 35 days, then lets SQLite reclaim space. Rollups other than drops are kept. Big tables are deleted in chunks of 2,000 rows, each its own short transaction with a pause of three times the chunk's duration, so ingest keeps getting the writer; space is returned 2,000 pages at a time.
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
