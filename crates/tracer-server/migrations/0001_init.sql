-- Tracer server schema, version 1.
--
-- Conventions:
--   * Times are milliseconds since the Unix epoch, UTC, as INTEGER.
--   * `day` is whole days since the Unix epoch, UTC: ts / 86400000.
--   * Raw tables (events, perf, crashes, sessions, items_seen) are pruned by age.
--     The daily_* tables are rollups, recomputed for recent days, and are kept.
--   * The client IP address is never stored anywhere.

CREATE TABLE apps (
    id         INTEGER PRIMARY KEY,
    slug       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- Write keys. Public by nature (they ship inside the app), so they only allow ingest.
CREATE TABLE app_keys (
    key        TEXT PRIMARY KEY,
    app_id     INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
) WITHOUT ROWID;

CREATE TABLE admins (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

-- Dashboard login sessions. Only a hash of the cookie token is stored.
CREATE TABLE admin_sessions (
    token_hash TEXT PRIMARY KEY,
    admin_id   INTEGER NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE installs (
    app_id        INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    install_id    TEXT NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    first_version TEXT NOT NULL,
    last_version  TEXT NOT NULL,
    os_name       TEXT,
    os_version    TEXT,
    arch          TEXT,
    cpu_brand     TEXT,
    cpu_cores     INTEGER,
    ram_mb        INTEGER,
    gpu_name      TEXT,
    gpu_backend   TEXT,
    locale        TEXT,
    PRIMARY KEY (app_id, install_id)
) WITHOUT ROWID;

CREATE TABLE sessions (
    app_id      INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    session_id  TEXT NOT NULL,
    install_id  TEXT NOT NULL,
    version     TEXT NOT NULL,
    build       TEXT,
    process     TEXT NOT NULL,
    os_name     TEXT,
    started_at  INTEGER NOT NULL,
    day         INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL,
    -- ok | exited | crashed | abnormal. A terminal status is never overwritten.
    status      TEXT NOT NULL DEFAULT 'ok',
    duration_s  INTEGER NOT NULL DEFAULT 0,
    focused_s   INTEGER NOT NULL DEFAULT 0,
    active_s    INTEGER NOT NULL DEFAULT 0,
    idle_s      INTEGER NOT NULL DEFAULT 0,
    errors      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (app_id, session_id)
) WITHOUT ROWID;
CREATE INDEX sessions_by_day ON sessions (app_id, day, version);
CREATE INDEX sessions_by_install ON sessions (app_id, install_id, started_at);

-- Item ids already stored, for dedup when a client resends a batch.
CREATE TABLE items_seen (
    app_id      INTEGER NOT NULL,
    item_id     TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    PRIMARY KEY (app_id, item_id)
) WITHOUT ROWID;
CREATE INDEX items_seen_by_age ON items_seen (received_at);

CREATE TABLE events (
    id         INTEGER PRIMARY KEY,
    app_id     INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    install_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    ts         INTEGER NOT NULL,
    day        INTEGER NOT NULL,
    version    TEXT NOT NULL,
    name       TEXT NOT NULL,
    category   TEXT NOT NULL,
    dynamic    INTEGER NOT NULL DEFAULT 0,
    props      TEXT
);
CREATE INDEX events_by_day ON events (app_id, day, name);
CREATE INDEX events_by_install ON events (app_id, install_id);

CREATE TABLE perf (
    id           INTEGER PRIMARY KEY,
    app_id       INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    install_id   TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    ts           INTEGER NOT NULL,
    day          INTEGER NOT NULL,
    version      TEXT NOT NULL,
    os_name      TEXT,
    window_s     INTEGER NOT NULL,
    frame_count  INTEGER NOT NULL DEFAULT 0,
    frame_sum_us INTEGER NOT NULL DEFAULT 0,
    frame_max_us INTEGER NOT NULL DEFAULT 0,
    slow         INTEGER NOT NULL DEFAULT 0,
    frozen       INTEGER NOT NULL DEFAULT 0,
    buckets      TEXT,
    cpu_pct      REAL,
    rss_mb       INTEGER,
    rss_peak_mb  INTEGER,
    startup_ms   INTEGER,
    counters     TEXT,
    gauges       TEXT
);
CREATE INDEX perf_by_day ON perf (app_id, day, version);
CREATE INDEX perf_by_install ON perf (app_id, install_id);

-- Crashes that share a fingerprint are one issue.
CREATE TABLE issues (
    id            INTEGER PRIMARY KEY,
    app_id        INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    fingerprint   TEXT NOT NULL,
    kind          TEXT NOT NULL,
    title         TEXT NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    first_version TEXT NOT NULL,
    last_version  TEXT NOT NULL,
    count         INTEGER NOT NULL DEFAULT 0,
    -- open | resolved | ignored
    status        TEXT NOT NULL DEFAULT 'open',
    UNIQUE (app_id, fingerprint)
);
CREATE INDEX issues_by_recency ON issues (app_id, status, last_seen);

CREATE TABLE crashes (
    id          INTEGER PRIMARY KEY,
    app_id      INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    issue_id    INTEGER NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    install_id  TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    ts          INTEGER NOT NULL,
    day         INTEGER NOT NULL,
    version     TEXT NOT NULL,
    build       TEXT,
    os_name     TEXT,
    os_version  TEXT,
    arch        TEXT,
    kind        TEXT NOT NULL,
    fatal       INTEGER NOT NULL DEFAULT 0,
    message     TEXT,
    location    TEXT,
    thread      TEXT,
    backtrace   TEXT,
    blocked_ms  INTEGER,
    exit_code   INTEGER,
    signal      INTEGER,
    context     TEXT,
    breadcrumbs TEXT,
    perf        TEXT,
    device      TEXT
);
CREATE INDEX crashes_by_issue ON crashes (issue_id, ts);
CREATE INDEX crashes_by_day ON crashes (app_id, day, version);
CREATE INDEX crashes_by_install ON crashes (app_id, install_id);

-- Declared event catalogs, per app version.
CREATE TABLE catalogs (
    app_id      INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    version     TEXT NOT NULL,
    hash        TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    PRIMARY KEY (app_id, version, hash)
) WITHOUT ROWID;

CREATE TABLE catalog_events (
    app_id   INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    version  TEXT NOT NULL,
    name     TEXT NOT NULL,
    category TEXT NOT NULL,
    PRIMARY KEY (app_id, version, name)
) WITHOUT ROWID;

-- ---- rollups ----

-- One row per install per day it was seen: exact daily, weekly and monthly active counts.
CREATE TABLE daily_installs (
    app_id     INTEGER NOT NULL,
    day        INTEGER NOT NULL,
    install_id TEXT NOT NULL,
    PRIMARY KEY (app_id, day, install_id)
) WITHOUT ROWID;

CREATE TABLE daily_sessions (
    app_id     INTEGER NOT NULL,
    day        INTEGER NOT NULL,
    version    TEXT NOT NULL,
    os_name    TEXT NOT NULL,
    sessions   INTEGER NOT NULL,
    installs   INTEGER NOT NULL,
    exited     INTEGER NOT NULL,
    crashed    INTEGER NOT NULL,
    abnormal   INTEGER NOT NULL,
    errored    INTEGER NOT NULL,
    -- Installs with at least one crashed or abnormal session that day.
    crashed_installs INTEGER NOT NULL,
    duration_s INTEGER NOT NULL,
    focused_s  INTEGER NOT NULL,
    active_s   INTEGER NOT NULL,
    idle_s     INTEGER NOT NULL,
    PRIMARY KEY (app_id, day, version, os_name)
) WITHOUT ROWID;

CREATE TABLE daily_events (
    app_id   INTEGER NOT NULL,
    day      INTEGER NOT NULL,
    version  TEXT NOT NULL,
    name     TEXT NOT NULL,
    category TEXT NOT NULL,
    count    INTEGER NOT NULL,
    installs INTEGER NOT NULL,
    PRIMARY KEY (app_id, day, version, name)
) WITHOUT ROWID;

CREATE TABLE daily_perf (
    app_id       INTEGER NOT NULL,
    day          INTEGER NOT NULL,
    version      TEXT NOT NULL,
    os_name      TEXT NOT NULL,
    frame_count  INTEGER NOT NULL,
    frame_sum_us INTEGER NOT NULL,
    frame_max_us INTEGER NOT NULL,
    slow         INTEGER NOT NULL,
    frozen       INTEGER NOT NULL,
    -- Bucket counts added across the day, as a JSON array.
    buckets      TEXT NOT NULL,
    cpu_sum      REAL NOT NULL,
    cpu_n        INTEGER NOT NULL,
    rss_sum      INTEGER NOT NULL,
    rss_n        INTEGER NOT NULL,
    rss_peak_mb  INTEGER NOT NULL,
    startup_sum  INTEGER NOT NULL,
    startup_n    INTEGER NOT NULL,
    PRIMARY KEY (app_id, day, version, os_name)
) WITHOUT ROWID;

CREATE TABLE daily_drops (
    app_id INTEGER NOT NULL,
    day    INTEGER NOT NULL,
    reason TEXT NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (app_id, day, reason)
) WITHOUT ROWID;
