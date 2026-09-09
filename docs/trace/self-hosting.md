# Self-hosting the server

`ashlar-trace-server` is one binary and one data folder. It serves the ingest API, stores everything in a SQLite file, computes daily rollups, and renders the dashboard. There is no other service to run.

## 1. Run it

### The binary

```
cargo build --release -p ashlar-trace-server
./target/release/ashlar-trace-server serve
```

With no settings it listens on `127.0.0.1:8090` and keeps its database in `./data`. It logs to stderr at `info` level and stops on ctrl-c, letting in-flight requests finish.

### The container

`deploy/trace/Dockerfile` builds a small image (build from the repository root) and `deploy/trace/compose.yml` runs it with its data in a named volume:

```
docker compose -f deploy/trace/compose.yml up -d
```

The image tags (`rust:1-slim-bookworm`, `debian:bookworm-slim`) are left as they are so that security updates arrive on rebuild. If you need reproducible builds, pin the base images by digest (`FROM debian:bookworm-slim@sha256:...`) and update the digests on a schedule.

The image runs as an unprivileged user (uid 10001), listens on `0.0.0.0:8090` inside the container, and keeps its data in `/data`. The compose file publishes the port on `127.0.0.1:8090` only, on purpose: put a TLS proxy in front of it (section 5) instead of exposing it directly.

Check that it is up:

```
curl http://127.0.0.1:8090/v1/health
```

## 2. Settings

All settings are environment variables and all are optional.

| Variable | Default | Meaning |
|---|---|---|
| `TRACER_BIND` | `127.0.0.1:8090` | Listen address |
| `TRACER_DATA_DIR` | `./data` | Folder for the database |
| `TRACER_RETENTION_DAYS` | `90` | Age at which raw rows are pruned |
| `TRACER_RATE_PER_MIN` | `120` | Requests per minute per client address on the public routes (ingest, forget and health, unknown keys included); `0` switches the limit off. Dashboard sign-in has its own limit (10 a minute per address) that stays on |
| `TRACER_KEY_RATE_PER_MIN` | `6000` | Ingest requests per minute for one write key over all addresses. A capacity guard, far above the per-address limit, so that one client cannot use up a key's allowance for everyone; `0` switches it off |
| `TRACER_MAX_DB_MB` | `10240` | Ingest answers `507 Insufficient Storage` once the database is larger than this many MiB; clients keep their data and retry. The dashboard and sign-in keep working so you can fix it (lower the retention, then `prune`); `0` switches the ceiling off |
| `TRACER_TRUST_PROXY` | unset | When `1`, take the client address from the last entry of the last `X-Forwarded-For` header line |
| `TRACER_ADMIN_PASSWORD` | unset | Password for `admin create`. Read by that command only; the server never reads it |
| `TRACER_SECURE_COOKIES` | on | Dashboard cookies are `Secure`, and the session cookie is named `__Host-tracer_session`. Set `0` to sign in over plain HTTP, for example on `localhost` without a proxy |

A value that cannot be parsed stops the command with a message naming the variable. An empty value counts as unset. The repository's `.env.example` lists the same names with placeholders; copy it, fill it in, and keep the real file out of version control.

With compose, `TRACER_RETENTION_DAYS`, `TRACER_RATE_PER_MIN`, `TRACER_KEY_RATE_PER_MIN`, `TRACER_MAX_DB_MB`, `TRACER_TRUST_PROXY` and `TRACER_SECURE_COOKIES` are passed through from the shell or from a `.env` file next to the compose file. The bind address and data folder are fixed by the image.

## 3. First setup: an app, a key and an admin

Every command below works on the same data folder as `serve`. Run them with the same `TRACER_DATA_DIR`. They can run while the server is running.

**Create an app.** The slug is lower-case letters, digits, `-` or `_`. The command prints the app's first write key:

```
ashlar-trace-server app create my-app "My App"
created app my-app (My App)
write key: tk_...
```

Put that key into the host app's `Tracer::builder`. Write keys are public by nature (they ship inside the app) and only allow sending data. More keys:

```
ashlar-trace-server key create my-app
ashlar-trace-server key revoke tk_...      # stops it working at once
ashlar-trace-server app list
```

Revoking a key makes the server answer 401 to it. The SDK keeps its data and stops sending until the app restarts, so rotate keys by shipping a new one first, then revoking the old one when most installs have updated.

**Create a dashboard login.** The password is read from `TRACER_ADMIN_PASSWORD` or, if that is unset, from one line of standard input. It must be at least 8 characters:

```
TRACER_ADMIN_PASSWORD='a long passphrase' ashlar-trace-server admin create alice
```

Then open the server's address in a browser and sign in. The dashboard has an overview, crashes, performance, usage, release reports (downloadable as HTML, Markdown or JSON) and a settings page for keys.

**With compose**, the container's entrypoint is the binary, so:

```
docker compose -f deploy/trace/compose.yml exec tracer ashlar-trace-server app create my-app "My App"
docker compose -f deploy/trace/compose.yml exec -e TRACER_ADMIN_PASSWORD='a long passphrase' tracer ashlar-trace-server admin create alice
```

## 4. Point a host app at it

In the host app:

```rust
Tracer::builder("tk_...", "my-app", env!("CARGO_PKG_VERSION")).endpoint("https://telemetry.example.com")
```

The endpoint is the base URL with no path; the SDK appends `/v1/ingest` and `/v1/forget`. See [integration.md](integration.md) for everything else.

## 5. TLS and a reverse proxy

The server speaks plain HTTP. Terminate TLS in front of it with any proxy. The SDK uses `https` and verifies certificates, so use a real certificate.

Caddy, which obtains certificates itself:

```
telemetry.example.com {
    reverse_proxy 127.0.0.1:8090
}
```

nginx:

```
server {
    listen 443 ssl;
    server_name telemetry.example.com;
    # ssl_certificate and ssl_certificate_key as usual

    location / {
        proxy_pass http://127.0.0.1:8090;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
```

Then set:

- `TRACER_TRUST_PROXY=1`, so the rate limiter sees each client's address instead of the proxy's. Without it every request appears to come from the proxy and shares one bucket, and busy installs will see 429. The server uses the **last** `X-Forwarded-For` entry, the one the proxy itself appended, so earlier entries a client forges are ignored. Only turn this on when a proxy you control is the only way to reach the server.
- Leave `TRACER_SECURE_COOKIES` unset (cookies are `Secure` by default, which needs HTTPS). Only set it to `0` for plain-HTTP use on a local machine.

The server never stores client addresses; it uses them for the rate limit and nothing else.

Do not expose the port directly. Bind to `127.0.0.1` (the default) or to a private interface, and let the proxy be the public side. The server logs a warning at startup when it is bound beyond loopback without `TRACER_TRUST_PROXY=1`: behind a proxy that would make every client look like the proxy, so the per-address and sign-in limits would be shared by all of them.

The server itself cuts off clients that take longer than 15 seconds to send request headers (this also closes idle keep-alive connections), and serves at most 1,024 connections at once, waiting for a slot beyond that. Request bodies must arrive within 30 seconds. You can still add stricter limits in the proxy (`client_header_timeout`, `limit_conn`, `client_max_body_size 1m` in nginx), but the server no longer depends on it for slow clients.

## 6. Backups

Everything is in `TRACER_DATA_DIR`: the database `tracer.db`, and while the server runs, two companion files `tracer.db-wal` and `tracer.db-shm`. SQLite runs in WAL mode, so recent writes may live only in the `-wal` file. **Copying `tracer.db` alone while the server is running can give you a stale or inconsistent copy.** Use one of these:

1. **Ask SQLite for a snapshot.** Safe while the server runs:

   ```
   sqlite3 /path/to/data/tracer.db ".backup '/backups/tracer-$(date +%F).db'"
   ```

   or `VACUUM INTO '/backups/tracer-2026-01-01.db'` for a compacted copy. The result is a single, self-contained file.
2. **Stop the server, then copy the folder.** After a clean stop the `-wal` file is empty or gone, and copying the whole folder is fine. If you copy while stopped, copy all three files together.
3. **Snapshot the volume** (LVM, ZFS, a cloud disk snapshot). These capture the three files at one instant and are safe.

For compose, option 3 or a stopped copy is simplest:

```
docker compose -f deploy/trace/compose.yml stop
docker run --rm -v tracer_tracer-data:/data -v "$PWD":/backup debian:bookworm-slim tar czf /backup/tracer-data.tgz -C /data .
docker compose -f deploy/trace/compose.yml start
```

(The volume is named `<project>_tracer-data`; `docker volume ls` shows the real name.) Test a restore now and then: stop the server, put the files back into the data folder, start it.

What is lost if you lose the database: history. Clients keep unsent data on their own disks and resend it, and the server ignores anything it has already stored, so a restore plus a few days of resends is not harmful.

## 7. Retention and housekeeping

- **Raw rows** (events, performance windows, crashes, sessions) are deleted when they are older than `TRACER_RETENTION_DAYS` (default 90). The ids used to ignore duplicates are kept for 35 days. After pruning, SQLite reclaims the space.
- **Daily totals** (opens, active installs, event counts, frame histograms, drops) are kept indefinitely, so long-term charts survive pruning. Lowering the retention period does not remove them.
- **Timers.** `serve` recomputes the rollups for the last three days every five minutes and runs retention hourly. A session that is still open a day after its last item is closed as `abnormal`.
- **By hand**, for example from cron on a machine that runs the binary directly:

  ```
  ashlar-trace-server rollup --days 7     # recompute recent rollups now
  ashlar-trace-server prune               # run retention once
  ```

## 8. Upgrading

1. Take a backup (section 6).
2. Build or pull the new version and restart. Database migrations are applied at startup and tracked in the file (`PRAGMA user_version`). They only ever add; a shipped migration is never edited.
3. Check `/v1/health`, which reports the server version.

Run one server process per data folder. Do not point an older binary at a database that a newer one has migrated. The wire format only grows, so older apps keep working against a newer server, and an app newer than the server may send fields the old server ignores. If a client sends a `schema` higher than the server understands it gets 422 and drops that batch, so upgrade the server before shipping an app that depends on a new schema.

## 9. Troubleshooting

**The SDK stops sending, the server answers 401.** The key is missing, mistyped or revoked. The SDK keeps its data and stops sending until the host app restarts. Check the key on the dashboard's settings page, or create a new one (`key create`). The key header is `x-tracer-key`.

**429 Too Many Requests.** Requests are limited per client address (`TRACER_RATE_PER_MIN`, default 120 a minute), and each write key also has a much larger overall allowance (`TRACER_KEY_RATE_PER_MIN`, default 6000). Behind a proxy, set `TRACER_TRUST_PROXY=1`, otherwise all clients share the proxy's address. The SDK honours `Retry-After` and keeps its data until the server accepts it, so an occasional 429 loses nothing. Sustained 429 usually means the proxy setting above, or a test loop. Raise the limit, or set `0` to switch it off.

**507 Insufficient Storage.** The database passed `TRACER_MAX_DB_MB`. The SDK keeps its data and retries. Lower `TRACER_RETENTION_DAYS` and run `ashlar-trace-server prune`, or raise the ceiling.

**400, 413 or 422.** The batch was malformed, over 1 MiB after decompression, or from a newer schema. The SDK deletes that batch and counts a `server_rejected` drop, shown on the dashboard. A proxy that rewrites or truncates bodies (or a body-size limit smaller than 1 MiB) causes this too.

**Times look wrong, or events land on the wrong day.** The server compares the client's `sent_at` with its own clock. If they differ by more than five minutes, item times are shifted by the difference, and no time is accepted more than an hour in the future or older than the retention period. Make sure the *server's* clock is right (use NTP); the client's can be wrong without harm.

**Nothing arrives.**

1. `curl https://your-host/v1/health` from outside. If that fails, it is the network, DNS, TLS or the proxy.
2. In the host app: is `endpoint` set (the SDK records locally and sends nothing without it), and is it the base URL with no path?
3. Is the user's `DO_NOT_TRACK` or `TRACER_DISABLED` set? Then nothing is written or sent, by design. Are all three consent categories switched off?
4. The SDK does not send at once: it sends when 50 items are waiting, every minute while items are waiting, and at the end of the session. A short run that was killed is sent by the next launch.
5. Look at the `tracer` folder inside the host's data directory. `spool/` holds what has not been delivered; `outbox.jsonl` (inspect mode) shows what would be sent.
6. The dashboard has a version filter and a date range; check them. Events that are not in the app's declared catalog are kept, but listed on the usage page under "Undeclared events" rather than among the declared ones.
7. Watch the server's stderr while the host app starts.

**Sessions show up as "abnormal".** A process ended without closing its session (killed, crashed natively, power loss). The next launch of the app reports it. A session that never ends and never reports again is closed as `abnormal` after a day.

**The container cannot write its data.** The image runs as uid 10001. A bind-mounted data folder must be writable by that user; the named volume in `compose.yml` already is.

## 10. Capacity and tuning

Numbers below were measured with `examples/trace-loadgen` on one machine (15 cores, an NVMe disk, release build, client and server on the same host over loopback, other jobs running on the machine). Treat them as an order of magnitude for your own hardware and re-measure with the commands at the end. A "typical" envelope in the generator carries about 40 items (a session start with a catalog hash, heartbeats, 20 to 50 events, a performance window with a real histogram, now and then a crash with a 30-frame backtrace, a session end) and is about 1.2 KB gzipped.

**Ingest.**

| Load (64 connections) | Result |
|---|---|
| Flat out | about 4,300 envelopes a second (170,000 items a second), p50 13 ms, p99 about 100 ms |
| Fixed 2,000 a second | p50 8 ms, p99 about 60 ms, no errors |
| Fixed 500 a second | p50 7 ms, p99 about 14 ms |
| 1,000 a second for 5 minutes | no errors; p50 steady at 9 ms; p99 drifts from 70 to 230 ms as the database grows to 2.7 GB; server memory 28 to 38 MiB |
| 2,000 a second for 5 minutes | held for about 3 minutes (a 3 GB database), then latency climbs: see "When the database outgrows memory" |
| 9,000 a second for 15 seconds (over twice the capacity) | the server answers `503` with `Retry-After: 1` to what it cannot take, keeps accepting the rest at its normal rate, answers `/v1/health` and the dashboard in under 5 ms, and is back to no errors within a second of the load dropping |

Why it is fast: ingest requests are queued to one writer thread that stores up to 64 envelopes per transaction and commits once, a separate thread checkpoints the write-ahead log, statements are prepared once, and duplicate detection is one statement per item. The queue holds 256 envelopes; when it is full the server answers `503` and the SDK retries later, so memory stays bounded however hard clients push.

**Durability.** The database runs in WAL mode with `synchronous = NORMAL`. A request is acknowledged only after its data is committed to the log, so stopping the process, even with `kill -9`, loses nothing that was acknowledged (a test does exactly this under load). What `NORMAL` does not do is `fsync` each commit, so a power failure or operating-system crash can lose the last moments of acknowledged data. The database is never left corrupt. If you cannot accept that, run the database on a volume with a battery-backed cache or a replicated disk.

**The dashboard on a large database.** On a database with 1,000,000 sessions, 10,000,000 events, 2,000,000 performance windows, 100,000 crashes in 5,000 issues, 50 versions, 200,000 installs and 90 days (2.5 GB):

| | Before | After |
|---|---|---|
| Slowest page, warm | 2.1 s (reports), 1.4 s (overview) | 18 ms |
| Slowest page, first request after start | 2.2 s | 290 ms |
| Rollup of 3 days (writer held) | 2.3 s | 3.5 s (a few ms of it holding the writer) |
| Full 90-day recompute | 60 s | 64 s |

Ingest at 300 envelopes a second while the 3-day rollup runs as another process: p99 58 ms against 47 ms without it. While retention deletes half the data it never stops ingest, but it does hurt the tail (p99 about 300 ms against about 40 ms).

**When the database outgrows memory.** Each stored item touches several B-tree indexes in random order, so once the database is much larger than the memory the operating system can use for caching, every commit reads and writes random pages and throughput falls. On the test machine that started at roughly 3 GB at 2,000 envelopes a second. The remedies are the usual ones: more memory, a shorter `TRACER_RETENTION_DAYS`, and a fast local disk. Dropping the four per-install indexes delayed the problem a little but did not remove it, so they are kept. `TRACER_MAX_DB_MB` stops ingest with `507` before the disk fills.

**Settings that matter for capacity.** `TRACER_RETENTION_DAYS` (database size is about 220 bytes per stored item including its indexes and dedup record: 12 million items took 2.7 GB); `TRACER_RATE_PER_MIN` (per client address) and `TRACER_KEY_RATE_PER_MIN` (per key, a capacity guard); `TRACER_MAX_DB_MB`. Retention runs hourly in small chunks (100 rows) that yield the writer, so the first run on a very large database can take a long while; that is harmless, and later runs only remove a day's worth.

**Measuring your own server.**

```
cargo xtask trace-load                                   # a short standard profile, prints a table
cargo build --release -p ashlar-trace-server -p loadgen
target/release/loadgen run --spawn --concurrency 64 --duration 30             # flat out
target/release/loadgen run --spawn --rate 500 --duration 60                   # fixed rate, open loop
target/release/loadgen run --url http://host:8090 --key tk_... --db /data/tracer.db --rate 200 --duration 60
target/release/loadgen run --spawn --phases 1500:10,9000:15,1500:10 --concurrency 600 --probe   # overload
target/release/loadgen seed --db /tmp/big/tracer.db --scale 1.0               # 10M events, 1M sessions, ... (2 min, 2.5 GB)
target/release/loadgen read --dir /tmp/big --mode rollup                      # rollup timings
target/release/loadgen read --dir /tmp/big --mode pages                       # every page, cold and warm
```

The generator is open loop with `--rate`: requests leave on a clock and latency is measured from when they were due, so a slow server shows up as latency. It ends with a check that the number of items the server stored equals the number it acknowledged. Do not point it at a server you care about: it writes a lot.
