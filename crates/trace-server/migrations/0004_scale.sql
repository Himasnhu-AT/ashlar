-- Tracer server schema, version 4: read-path and rollup scalability, from measurements on a database with
-- 1M sessions, 10M events and 2M performance rows.
--
--   * Per-version distinct-install counts (reports, version-filtered pages) took 1.9 s each: a covering index
--     on (app, version, day, install) makes them an index-only scan of one version.
--   * The performance page's custom-metrics query scanned every recent performance row (2.2 s): a partial index
--     holds only the rows that carry counters or gauges.
--   * Weekly and monthly active installs were a distinct count over up to 30 days of `daily_installs`
--     (230 ms): they are now stored per day in `daily_active` by the rollup.
--   * `dirty_days` records which (app, day) pairs received data since they were last rolled up, so the rollup
--     timer recomputes only those instead of rescanning a fixed window (and also catches late data).

CREATE INDEX sessions_by_version ON sessions (app_id, version, day, install_id);

CREATE INDEX perf_with_metrics ON perf (app_id, ts) WHERE counters IS NOT NULL OR gauges IS NOT NULL;

CREATE TABLE daily_active (
    app_id INTEGER NOT NULL,
    day    INTEGER NOT NULL,
    dau    INTEGER NOT NULL,
    wau    INTEGER NOT NULL,
    mau    INTEGER NOT NULL,
    PRIMARY KEY (app_id, day)
) WITHOUT ROWID;

CREATE TABLE dirty_days (
    app_id    INTEGER NOT NULL,
    day       INTEGER NOT NULL,
    -- Raised on every mark; the rollup deletes the row only if it still holds the value it read.
    marked_at INTEGER NOT NULL,
    PRIMARY KEY (app_id, day)
) WITHOUT ROWID;

-- An existing database needs every rolled-up day recomputed once to fill `daily_active`, and the days that
-- have raw data not yet rolled up.
INSERT OR IGNORE INTO dirty_days (app_id, day, marked_at)
    SELECT app_id, day, 1 FROM daily_sessions UNION SELECT app_id, day, 1 FROM daily_events UNION SELECT app_id, day, 1 FROM daily_perf;
INSERT OR IGNORE INTO dirty_days (app_id, day, marked_at)
    SELECT DISTINCT app_id, day, 1 FROM sessions WHERE day >= (SELECT COALESCE(max(day), 0) - 3 FROM sessions);
