-- Tracer server schema, version 3: hostile-input hardening.
--
--   * Catalogs are additive: events are stored per catalog hash, and an event the catalog does not list is
--     stored with `declared = 0` instead of being rejected.
--   * Sessions remember which catalog hash they named, so "never fired" can use the catalog most installs of
--     a version reported.
--   * Issues remember the day they were created, to cap new issues per app per day.

ALTER TABLE sessions ADD COLUMN catalog_hash TEXT;
ALTER TABLE events ADD COLUMN declared INTEGER NOT NULL DEFAULT 1;
ALTER TABLE issues ADD COLUMN created_day INTEGER NOT NULL DEFAULT 0;
CREATE INDEX issues_by_created ON issues (app_id, created_day);

CREATE TABLE catalog_events_new (
    app_id   INTEGER NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    version  TEXT NOT NULL,
    hash     TEXT NOT NULL,
    name     TEXT NOT NULL,
    category TEXT NOT NULL,
    PRIMARY KEY (app_id, version, hash, name)
) WITHOUT ROWID;
INSERT OR IGNORE INTO catalog_events_new (app_id, version, hash, name, category)
    SELECT ce.app_id, ce.version, c.hash, ce.name, ce.category
    FROM catalog_events ce JOIN catalogs c ON c.app_id = ce.app_id AND c.version = ce.version;
DROP TABLE catalog_events;
ALTER TABLE catalog_events_new RENAME TO catalog_events;

-- `declared` joins the key: the same event name can be declared for some installs and not others.
CREATE TABLE daily_events_new (
    app_id   INTEGER NOT NULL,
    day      INTEGER NOT NULL,
    version  TEXT NOT NULL,
    name     TEXT NOT NULL,
    declared INTEGER NOT NULL DEFAULT 1,
    category TEXT NOT NULL,
    count    INTEGER NOT NULL,
    installs INTEGER NOT NULL,
    PRIMARY KEY (app_id, day, version, name, declared)
) WITHOUT ROWID;
INSERT INTO daily_events_new (app_id, day, version, name, declared, category, count, installs)
    SELECT app_id, day, version, name, 1, category, count, installs FROM daily_events;
DROP TABLE daily_events;
ALTER TABLE daily_events_new RENAME TO daily_events;
CREATE INDEX daily_events_by_name ON daily_events (app_id, name);
