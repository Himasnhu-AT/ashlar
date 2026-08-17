# Wire format

Schema version **1**. The types live in `crates/tracer-proto`; the fixtures in its `tests/fixtures/` are the reference examples.

## Transport

| | |
|---|---|
| Ingest | `POST /v1/ingest`, body is one envelope as JSON, usually `Content-Encoding: gzip` |
| Forget | `POST /v1/forget`, body `{"schema":1,"install_id":"…"}` |
| Health | `GET /v1/health` |
| Auth | `x-tracer-key: <app write key>` on ingest and forget |
| Limits | 1 MiB per body after decompression |

Replies to ingest are JSON: `{"accepted":n,"duplicates":n,"rejected":n,"need_catalog":bool}`. `rejected` counts items the server refused as malformed (a bad name, an unknown type, an over-long id, more than 20 crashes in one envelope); an event is **never** refused for not being in the catalog. `POST /v1/forget` replies `200 {"ok":true}` whether or not the install existed.

The server limits requests per client address (`429`), and the address is used for that and nothing else.

| Status | Meaning | Client does |
|---|---|---|
| 200 | Stored (see counts) | Deletes the batch |
| 400, 413, 422 | Malformed or too large | Deletes the batch and counts a drop |
| 401, 403 | Unknown or revoked key | Keeps data, stops sending until restart |
| 429 | Rate limited | Waits for `Retry-After`, then retries |
| 507 | Server storage is full | Keeps data, retries with backoff (like any 5xx) |
| other 5xx, network error | Server trouble | Retries with backoff |

## Envelope

One envelope carries items from **one session of one install**. Data from an earlier session, such as a crash found on the next launch, goes in its own envelope with that session's header.

| Field | Type | Notes |
|---|---|---|
| `schema` | integer | `1` |
| `app` | object | `name`, `version`, optional `build`, `process` (`gui`, `cli`, `service`, `child`) |
| `install_id` | string | Random, resettable. Never derived from hardware. |
| `session_id` | string | One per process run |
| `device` | object, optional | `os_name`, `os_version`, `arch`, `cpu_brand`, `cpu_cores`, `ram_mb`, `gpu`, `locale` |
| `sent_at` | integer | Client clock in Unix milliseconds, for drift correction |
| `items` | array | See below |

Every item has `id` (unique, client-generated, at most 64 bytes; the server ignores an id it has already stored), `ts` (Unix milliseconds) and `type`. The server moves `ts` onto its own clock when the client clock is more than five minutes off, and clamps it to the server's retention window in the past and one hour ahead.

The server creates or updates the session record only for envelopes that carry a `session_start`, `session_heartbeat` or `session_end`. Events, `perf`, `crash` and `drops` in an envelope without one are stored with their `session_id` as plain text, so a client that has switched session reporting off creates no session.

## Item types

| `type` | Fields |
|---|---|
| `session_start` | `catalog_hash`, `catalog` (`events[]` of `name`, `category`), `listening[]`, `signals[]` |
| `session_heartbeat` | `seq`, `duration_s`, `focused_s`, `active_s`, `idle_s` — running totals |
| `session_end` | `status` (`exited`, `crashed`, `abnormal`), the same totals, `errors` |
| `event` | `name`, `category`, `dynamic`, `props` (bool, integer, float or short string values) |
| `perf` | `window_s`, `frames`, `cpu_pct`, `rss_mb`, `rss_peak_mb`, `startup_ms`, `counters`, `gauges` |
| `crash` | `kind`, `fatal`, `message`, `location`, `thread`, `backtrace`, `blocked_ms`, `exit_code`, `signal`, `context`, `breadcrumbs[]`, `perf` |
| `drops` | `reasons` — what the client discarded, by reason |

**`crash.fatal`.** A client cannot know at capture time whether a panic ends the host, so it sends `fatal: false` for `panic`, `handled` and `hang`, and `fatal: true` for `unclean_exit` and `child_exit` (which are found after the fact). The server decides for panics: when a session ends with `status: "crashed"` (a `session_end`, in the same or a later envelope, before or after the crash item arrives), that session's most recent `panic` is stored as fatal. A `fatal: true` panic from a client also marks its `ok` session `crashed`.

**Catalog.** `session_start.catalog` is client input and is treated as unverified: catalogs are stored next to each other by `catalog_hash`, never replacing one. Each stored event carries a `declared` flag, set when its name is in a catalog held for the app version. `declared = false` events are kept and shown separately; they are not an error.

`crash.kind` is one of `panic`, `handled`, `hang`, `unclean_exit`, `child_exit`.

`perf.frames` is a histogram: `count`, `sum_us`, `max_us`, `slow` (over 16.667 ms), `frozen` (over 700 ms) and `buckets`, one count per upper bound in `FRAME_BUCKET_BOUNDS_US` plus an overflow bucket. Histograms merge by adding buckets.

## Names

Event and category names are lower-case `snake_case`, optionally dotted (`file.open`), start with a letter, and are at most 64 bytes. The same rule is checked at build time in the SDK and again by the server.

## Compatibility

- Fields are only added, and new fields are optional.
- Readers ignore unknown fields, and treat an unknown item `type` as something to skip.
- Unknown enum values map to `other` where the enum has one.
- A breaking change increases `schema`.
