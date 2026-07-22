-- Migration 7 (LOCK-SPEC.md §8, ticket 050): per-file journal for
-- migrating an existing unencrypted vault into a Locking-enabled one.
-- Mirrors import_journal's shape and crash-safety discipline exactly — a
-- crash at any point is resumable in full by re-running migration, since
-- every step here is idempotent given the row's current `step`.
--
-- `old_path`/`new_path` are both absolute — `new_path` is always
-- `old_path` with the vault root prefix replaced by the mount point
-- (`<root>` -> `<root>/live`), computed once per row up front so a resume
-- never needs to re-derive it. `original_removed` is the last step,
-- mirroring the import pipeline's "verified copy exists, then delete" —
-- the one genuinely irreversible action, done last.

CREATE TABLE migration_journal (
    id         INTEGER PRIMARY KEY,
    library_id INTEGER NOT NULL REFERENCES libraries(id),
    kind       TEXT NOT NULL CHECK (kind IN ('blob','sidecar','quarantine','thumbnail','catalog')),
    old_path   TEXT NOT NULL,
    new_path   TEXT NOT NULL,
    step       TEXT NOT NULL DEFAULT 'pending'
        CHECK (step IN ('pending','copied','verified','original_removed')),
    error      TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_migration_journal_library_step ON migration_journal(library_id, step);
