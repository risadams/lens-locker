//! Milestone 4 exit-criteria test, per workplan/SPEC.md's literal wording:
//! "test by deleting the `.sqlite` file and rescanning."
//!
//! This is a real, executed end-to-end test against the actual pipeline —
//! import through `lumenvault-import::import_file`, tag via
//! `lumenvault-catalog`, sync via `lumenvault-xmp::sync_sidecar`, delete the
//! real on-disk `.sqlite` file, then run `rebuild_from_store` against a
//! *fresh* catalog and assert the recovered state against what was true
//! before deletion.

use std::fs;
use std::path::Path;

use image::{ImageBuffer, Rgb};
use lumenvault_import::{
    ImportContext, LibraryPaths, ensure_library, import_file, rebuild_from_store,
    start_or_resume_batch,
};
use rusqlite::Connection;

/// A real baseline JPEG — Milestone 2's conversion path (JPEG -> JPEG XL,
/// jbrd lossless transcode) applies to this.
fn write_jpeg(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(32, 24, |x, y| {
        Rgb([(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8])
    });
    img.save(&path).unwrap();
    path
}

/// WebP has no §4 conversion path at all — stays stored exactly as
/// imported, proving rebuild also handles the "never converted" case.
fn write_webp(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(20, 16, |x, y| Rgb([(x * 11) as u8, (y * 13) as u8, 200]));
    img.save(&path).unwrap();
    path
}

/// A RAW-extension file, recognized by filename only (Milestone 3) — no
/// real pixel data, so both import and rebuild must leave
/// width/height/perceptual_hash NULL without erroring.
fn write_raw(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        b"not real RAW sensor data, just bytes for extension-only recognition",
    )
    .unwrap();
    path
}

#[test]
fn rebuild_from_store_recovers_hashes_perceptual_hashes_and_tags_after_the_catalog_is_deleted() {
    let source_dir = tempfile::tempdir().unwrap();
    let library_dir = tempfile::tempdir().unwrap();
    let paths = LibraryPaths::new(library_dir.path());

    // ── Step 1: import a real fixture set through the real pipeline ──────
    let mut conn = Connection::open(paths.catalog_db()).unwrap();
    lumenvault_catalog::migrate(&mut conn).unwrap();

    let library_id = ensure_library(&conn, library_dir.path()).unwrap();
    let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
    let ctx = ImportContext {
        conn: &conn,
        paths: &paths,
        library_id,
        batch_id,
        conversion_enabled: true,
    };

    let jpeg_path = write_jpeg(source_dir.path(), "converted.jpg");
    let webp_path = write_webp(source_dir.path(), "unconverted.webp");
    let raw_path = write_raw(source_dir.path(), "camera.cr2");

    let jpeg_outcome = import_file(&ctx, &jpeg_path).unwrap();
    let webp_outcome = import_file(&ctx, &webp_path).unwrap();
    let raw_outcome = import_file(&ctx, &raw_path).unwrap();

    let lumenvault_import::FileOutcome::Imported { image_id: jpeg_id } = jpeg_outcome else {
        panic!("expected the JPEG to import fresh, got {jpeg_outcome:?}")
    };
    let lumenvault_import::FileOutcome::Imported { image_id: webp_id } = webp_outcome else {
        panic!("expected the WebP to import fresh, got {webp_outcome:?}")
    };
    let lumenvault_import::FileOutcome::Imported { image_id: raw_id } = raw_outcome else {
        panic!("expected the RAW file to import fresh, got {raw_outcome:?}")
    };

    // Confirm the JPEG really did convert to JXL — otherwise this test
    // wouldn't actually be exercising the new JXL-decode rebuild path.
    let (jpeg_stored_format, jpeg_conversion_status): (String, String) = conn
        .query_row(
            "SELECT stored_format, conversion_status FROM images WHERE id = ?1",
            [jpeg_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(jpeg_stored_format, "jxl");
    assert_eq!(jpeg_conversion_status, "converted");

    // ── Step 2: tag several images, sync sidecars, confirm on disk ──────
    lumenvault_catalog::add_tag(&conn, jpeg_id, "sunset").unwrap();
    lumenvault_catalog::add_tag(&conn, jpeg_id, "vacation").unwrap();
    lumenvault_catalog::add_tag(&conn, webp_id, "keep").unwrap();
    lumenvault_catalog::add_tag(&conn, webp_id, "drop-me").unwrap();

    let jpeg_sidecar = lumenvault_xmp::sync_sidecar(&conn, jpeg_id).unwrap();
    let webp_sidecar = lumenvault_xmp::sync_sidecar(&conn, webp_id).unwrap();

    assert!(
        jpeg_sidecar.exists(),
        "JXL image's sidecar must exist on disk"
    );
    assert!(
        webp_sidecar.exists(),
        "WebP image's sidecar must exist on disk"
    );
    assert_eq!(
        lumenvault_xmp::read_sidecar(&jpeg_sidecar).unwrap(),
        vec!["sunset".to_string(), "vacation".to_string()]
    );
    assert_eq!(
        lumenvault_xmp::read_sidecar(&webp_sidecar).unwrap(),
        vec!["drop-me".to_string(), "keep".to_string()]
    );

    // ── Step 3: remove a tag, re-sync, confirm the sidecar content changes ──
    lumenvault_catalog::remove_tag(&conn, webp_id, "drop-me").unwrap();
    lumenvault_xmp::sync_sidecar(&conn, webp_id).unwrap();
    let webp_tags_after_removal = lumenvault_xmp::read_sidecar(&webp_sidecar).unwrap();
    assert_eq!(
        webp_tags_after_removal,
        vec!["keep".to_string()],
        "sidecar must reflect the tag removal on disk"
    );

    // Snapshot pre-delete truth to compare rebuild's recovered state against.
    let (jpeg_original_hash, jpeg_stored_hash, jpeg_perceptual_hash, jpeg_stored_path): (Vec<u8>, Vec<u8>, i64, String) = conn
        .query_row(
            "SELECT original_hash, stored_hash, perceptual_hash, stored_path FROM images WHERE id = ?1",
            [jpeg_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    let (raw_original_hash, raw_stored_path): (Vec<u8>, String) = conn
        .query_row(
            "SELECT original_hash, stored_path FROM images WHERE id = ?1",
            [raw_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let webp_stored_path: String = conn
        .query_row(
            "SELECT stored_path FROM images WHERE id = ?1",
            [webp_id],
            |row| row.get(0),
        )
        .unwrap();
    let raw_width: Option<i64> = conn
        .query_row("SELECT width FROM images WHERE id = ?1", [raw_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        raw_width.is_none(),
        "sanity check: RAW must have no width at original import time"
    );

    drop(conn);

    // ── Step 4: literally delete the .sqlite catalog file ────────────────
    let catalog_path = paths.catalog_db();
    assert!(catalog_path.exists());
    fs::remove_file(&catalog_path).unwrap();
    assert!(!catalog_path.exists());

    // ── Rebuild against a brand fresh catalog ─────────────────────────────
    let mut fresh_conn = Connection::open(&catalog_path).unwrap();
    lumenvault_catalog::migrate(&mut fresh_conn).unwrap();

    let report = rebuild_from_store(&fresh_conn, &paths).unwrap();

    assert_eq!(
        report.images_recovered, 3,
        "all three blobs (jxl, webp, raw) must be recovered: {report:?}"
    );
    assert!(
        report.skipped.is_empty(),
        "no blob should be unrecoverable in this fixture set: {:?}",
        report.skipped
    );
    // 2 tags survived on the JPEG/JXL image, 1 on the WebP image after the removal.
    assert_eq!(
        report.tags_recovered, 3,
        "expected 2 + 1 tags recovered across both tagged images"
    );

    // ── Assertions: every blob got an images row back ─────────────────────
    let recovered_jpeg: (Vec<u8>, Vec<u8>, Option<i64>) = fresh_conn
        .query_row(
            "SELECT original_hash, stored_hash, perceptual_hash FROM images WHERE stored_path = ?1",
            [&jpeg_stored_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        recovered_jpeg.0, jpeg_original_hash,
        "original_hash must be recovered from the blob path, unchanged"
    );
    assert_eq!(
        recovered_jpeg.1, jpeg_stored_hash,
        "stored_hash must be re-derived from the actual on-disk bytes"
    );
    assert_eq!(
        recovered_jpeg.2,
        Some(jpeg_perceptual_hash),
        "perceptual_hash for the JXL-converted image must be recomputed via the new JXL decode capability and match the pre-delete value"
    );

    let recovered_raw: (Vec<u8>, Option<i64>, Option<i64>, Option<i64>) = fresh_conn
        .query_row(
            "SELECT original_hash, width, height, perceptual_hash FROM images WHERE stored_path = ?1",
            [&raw_stored_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(recovered_raw.0, raw_original_hash);
    assert_eq!(
        recovered_raw.1, None,
        "RAW file must have NULL width after rebuild, same as at import"
    );
    assert_eq!(
        recovered_raw.2, None,
        "RAW file must have NULL height after rebuild, same as at import"
    );
    assert_eq!(
        recovered_raw.3, None,
        "RAW file must have NULL perceptual_hash after rebuild, same as at import"
    );

    // ── Tags recovered from sidecars match what was set (removal included) ──
    let recovered_webp_id: i64 = fresh_conn
        .query_row(
            "SELECT id FROM images WHERE stored_path = ?1",
            [&webp_stored_path],
            |row| row.get(0),
        )
        .unwrap();
    let recovered_webp_tags =
        lumenvault_catalog::tags_for_image(&fresh_conn, recovered_webp_id).unwrap();
    assert_eq!(
        recovered_webp_tags,
        vec!["keep".to_string()],
        "removed tag must genuinely be gone after rebuild"
    );

    let recovered_jpeg_id: i64 = fresh_conn
        .query_row(
            "SELECT id FROM images WHERE stored_path = ?1",
            [&jpeg_stored_path],
            |row| row.get(0),
        )
        .unwrap();
    let recovered_jpeg_tags =
        lumenvault_catalog::tags_for_image(&fresh_conn, recovered_jpeg_id).unwrap();
    assert_eq!(
        recovered_jpeg_tags,
        vec!["sunset".to_string(), "vacation".to_string()]
    );
}
