-- Tracer server schema, version 2: indexes for the dashboard's read queries. No data changes.

-- Issue list filtered by version, and the per-issue breakdowns.
CREATE INDEX crashes_by_issue_version ON crashes (issue_id, version);

-- Issue list filtered by kind and status.
CREATE INDEX issues_by_kind ON issues (app_id, kind, last_seen);

-- "Declared but never fired" looks events up by name across all days.
CREATE INDEX daily_events_by_name ON daily_events (app_id, name);
