-- Migration 4 (ML-SPEC.md Milestone ML-2): tag confidence thresholds.
--
-- §4: "Two confidence thresholds: a low storage floor (keeps image_tags
-- from filling with near-zero scores across every starter label) and a
-- higher display floor (what surfaces as a visible chip by default)." —
-- never pinned to real numbers on any closed ticket, unlike
-- face_review_threshold's real sourced LFW figure (migration 0003).
-- Both values below are illustrative placeholders, preserving the
-- required ordering (storage <= display) — same treatment migration
-- 0003 already gave face_cluster_threshold/face_auto_attribute_threshold:
-- needs real calibration against actual SigLIP output on real photos
-- before ship, flagged rather than presented as researched numbers.
--
-- Per crates/catalog/schema.sql's own migration discipline comment: do
-- not hand-edit an existing migration once it has shipped; add a new one
-- instead.

ALTER TABLE app_settings ADD COLUMN tag_storage_threshold REAL NOT NULL DEFAULT 0.1;
ALTER TABLE app_settings ADD COLUMN tag_display_threshold REAL NOT NULL DEFAULT 0.5;
