-- Migration 6 (ML-SPEC.md §9, ticket 030 decision #4): tracks a detected
-- model-version bump so re-analysis can be prompted, not silently
-- automatic and not silently manual-only. A row exists only for a genuine
-- upgrade (a new `models` row for a name that already had a prior,
-- already-used version) — first-ever install of a model never creates one.

CREATE TABLE model_upgrade_notices (
    id           INTEGER PRIMARY KEY,
    model_id     INTEGER NOT NULL REFERENCES models(id),  -- the new version's row
    model_name   TEXT NOT NULL,
    old_version  TEXT NOT NULL,
    new_version  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','accepted')),
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    resolved_at  TEXT
);

CREATE UNIQUE INDEX idx_model_upgrade_notices_model ON model_upgrade_notices(model_id);
