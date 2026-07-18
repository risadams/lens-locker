-- Migration 2 (Milestone 5): replace the contentless `images_fts` FTS5
-- table declared in schema.sql with a standard (self-contained) one.
--
-- schema.sql originally declared `images_fts` with `content=''`
-- (contentless) but no milestone before this one ever populated it. A
-- contentless FTS5 table cannot be updated or deleted by rowid alone — its
-- own "delete"/"delete-all" special commands require re-supplying the exact
-- column values that were previously indexed, which this crate has no
-- reason to keep a shadow copy of just to satisfy FTS5's contentless
-- bookkeeping. Milestone 5 needs `images_fts` to actually support
-- insert/update/delete as tags and filenames change, so this migration
-- switches it to FTS5's default (self-contained) mode, which stores its own
-- copy of the indexed text and supports normal `DELETE FROM images_fts
-- WHERE rowid = ?` — see `lumenvault_catalog::sync_fts_row`.
--
-- Per crates/catalog/src/lib.rs's own comment: "do not hand-edit an
-- existing migration once it has shipped — add a new one instead." This is
-- that new one, not an edit to schema.sql's migration.

DROP TABLE images_fts;

CREATE VIRTUAL TABLE images_fts USING fts5(
    original_filename, camera_make, camera_model, tag_names,
    tokenize='porter unicode61'
);
