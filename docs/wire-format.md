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

Replies to ingest are JSON: `{"accepted":n,"duplicates":n,"rejected":n,"need_catalog":bool}`.

| Status | Meaning | Client does |
|---|---|---|
| 200 | Stored (see counts) | Deletes the batch |
| 400, 413, 422 | Malformed or too large | Deletes the batch and counts a drop |
| 401, 403 | Unknown or revoked key | Keeps data, stops sending until restart |
| 429 | Rate limited | Waits for `Retry-After`, then retries |
| 5xx, network error | Server trouble | Retries with backoff |

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

Every item has `id` (unique, client-generated; the server ignores an id it has already stored), `ts` (Unix milliseconds) and `type`.

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

`crash.kind` is one of `panic`, `handled`, `hang`, `unclean_exit`, `child_exit`.

`perf.frames` is a histogram: `count`, `sum_us`, `max_us`, `slow` (over 16.667 ms), `frozen` (over 700 ms) and `buckets`, one count per upper bound in `FRAME_BUCKET_BOUNDS_US` plus an overflow bucket. Histograms merge by adding buckets.

## Names

Event and category names are lower-case `snake_case`, optionally dotted (`file.open`), start with a letter, and are at most 64 bytes. The same rule is checked at build time in the SDK and again by the server.

## Compatibility

- Fields are only added, and new fields are optional.
- Readers ignore unknown fields, and treat an unknown item `type` as something to skip.
- Unknown enum values map to `other` where the enum has one.
- A breaking change increases `schema`.
