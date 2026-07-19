//! Milestone 3's actual exit criterion (workplan/SPEC.md Part 2): "import a
//! folder with known dupes and a RAW+JPEG pair, confirm the queue and the
//! pairing table are both correct." Run for real against a fixture folder,
//! then verified via direct SQLite queries (no UI exists yet, per the exit
//! criteria's own wording) — not a description of what the test would do.

use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, ImageEncoder, Rgb};
use lumenvault_import::{ImportContext, LibraryPaths, ensure_library, import_directory, start_or_resume_batch};
use rusqlite::Connection;

/// A smooth gradient (see `lumenvault-hash`'s own tests for why smooth, not
/// fixed-period) — the base image both near-duplicate fixtures re-encode.
fn gradient(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    ImageBuffer::from_fn(width, height, |x, y| {
        let r = (x as f32 / width as f32 * 255.0) as u8;
        let g = (y as f32 / height as f32 * 255.0) as u8;
        let b = ((x + y) as f32 / (width + height) as f32 * 255.0) as u8;
        Rgb([r, g, b])
    })
}

/// A visually distinct checkerboard — the control fixture, and also the
/// base pattern for the two exact-duplicate files (distinct from the
/// gradient/near-duplicate pair so no fixture accidentally shares a
/// perceptual hash with another).
fn checkerboard(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    ImageBuffer::from_fn(width, height, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 { Rgb([20, 200, 40]) } else { Rgb([220, 30, 180]) }
    })
}

/// A third, independent pattern for the control image — must not
/// perceptually match the gradient pair, the checkerboard exact-dupes, or
/// the RAW+JPEG fixture.
fn radial(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    ImageBuffer::from_fn(width, height, |x, y| {
        let dx = x as f32 - cx;
        let dy = y as f32 - cy;
        let d = (dx * dx + dy * dy).sqrt();
        let v = ((d * 6.0) as u32 % 256) as u8;
        Rgb([v, 255 - v, (v / 2).wrapping_add(64)])
    })
}

/// A fourth, independent pattern (vertical stripes) — used only for the
/// RAW+JPEG fixture's JPEG side, so it can't accidentally perceptually
/// match the control image's own distinct pattern.
fn stripes(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    ImageBuffer::from_fn(width, height, |x, _y| {
        if (x / 3) % 2 == 0 { Rgb([250, 250, 10]) } else { Rgb([10, 10, 250]) }
    })
}

fn save_jpeg(img: &ImageBuffer<Rgb<u8>, Vec<u8>>, path: &Path, quality: u8) {
    let file = std::fs::File::create(path).unwrap();
    let encoder = JpegEncoder::new_with_quality(file, quality);
    encoder.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8).unwrap();
}

#[test]
fn importing_a_folder_with_known_dupes_and_a_raw_jpeg_pair_produces_a_correct_queue_and_pairing_table() {
    let source_dir = tempfile::tempdir().unwrap();
    let library_dir = tempfile::tempdir().unwrap();

    // 1. Exact duplicate: identical JPEG content under two filenames.
    let exact_base = checkerboard(48, 48);
    save_jpeg(&exact_base, &source_dir.path().join("dup_a.jpg"), 90);
    std::fs::copy(source_dir.path().join("dup_a.jpg"), source_dir.path().join("dup_b.jpg")).unwrap();

    // 2. Genuine near-duplicate: same base gradient, one saved as-is at high
    // quality, the other given a small 2px crop (then resized back to the
    // original dimensions) and re-saved at a much lower JPEG quality —
    // visually the same subject, genuinely different bytes and a nonzero
    // (but small) perceptual distance.
    let near_base = gradient(64, 64);
    save_jpeg(&near_base, &source_dir.path().join("near_1.jpg"), 95);
    let cropped = image::imageops::crop_imm(&near_base, 6, 6, 52, 52).to_image();
    let near_2_img = image::imageops::resize(&cropped, 64, 64, image::imageops::FilterType::Nearest);
    save_jpeg(&near_2_img, &source_dir.path().join("near_2.jpg"), 5);

    // 3. Control: a clearly different image (must not match anything above).
    let control = radial(64, 64);
    save_jpeg(&control, &source_dir.path().join("control.jpg"), 90);

    // 4. RAW+JPEG pair, sharing filename stem "IMG_0001". The RAW file is
    // dummy bytes with a recognized extension — this milestone recognizes
    // the extension only, it doesn't decode RAW pixels (see
    // lumenvault-decode's module doc for why no milestone schedules that).
    std::fs::write(source_dir.path().join("IMG_0001.cr2"), b"not a real RAW file, dummy bytes for extension recognition")
        .unwrap();
    let raw_jpeg_side = stripes(32, 32); // distinct pattern, not the control image
    save_jpeg(&raw_jpeg_side, &source_dir.path().join("IMG_0001.jpg"), 90);

    // Independently confirm, before trusting the pipeline's own perceptual
    // hashing, that near_1/near_2 really do land within the default
    // Hamming threshold and that the control image really is far from
    // everything — this is the "actually compute it, don't guess" check.
    let near_1_decoded = image::open(source_dir.path().join("near_1.jpg")).unwrap();
    let near_2_decoded = image::open(source_dir.path().join("near_2.jpg")).unwrap();
    let control_decoded = image::open(source_dir.path().join("control.jpg")).unwrap();
    let hash_near_1 = lumenvault_hash::perceptual_hash(&near_1_decoded);
    let hash_near_2 = lumenvault_hash::perceptual_hash(&near_2_decoded);
    let hash_control = lumenvault_hash::perceptual_hash(&control_decoded);
    let observed_near_distance = lumenvault_hash::hamming_distance(hash_near_1, hash_near_2);
    println!("observed near-duplicate Hamming distance: {observed_near_distance}");
    assert!(
        observed_near_distance <= 5,
        "fixture must genuinely land within the default threshold, got {observed_near_distance}"
    );
    assert!(
        lumenvault_hash::hamming_distance(hash_near_1, hash_control) > 5,
        "control image must not accidentally match the near-duplicate pair"
    );

    // Run the real import pipeline, once, over the whole fixture folder.
    let paths = LibraryPaths::new(library_dir.path());
    let mut conn = Connection::open(paths.catalog_db()).unwrap();
    lumenvault_catalog::migrate(&mut conn).unwrap();
    let library_id = ensure_library(&conn, library_dir.path()).unwrap();
    let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
    let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
    import_directory(&ctx, source_dir.path(), |_, _| true).unwrap();

    // ── Exact-duplicate collapse (Milestone 1 behavior, re-verified under
    // Milestone 3's added logic) ────────────────────────────────────────
    let dup_image_ids: Vec<i64> = conn
        .prepare("SELECT DISTINCT image_id FROM image_sources WHERE original_filename IN ('dup_a.jpg', 'dup_b.jpg')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(dup_image_ids.len(), 1, "both exact-duplicate filenames must resolve to the same images row");
    let dup_image_id = dup_image_ids[0];
    let dup_source_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM image_sources WHERE image_id = ?1", [dup_image_id], |row| row.get(0))
        .unwrap();
    assert_eq!(dup_source_count, 2, "the exact-duplicate image must have more than one image_sources row");

    // ── dedupe_review_queue: exactly one row, the real computed distance ─
    let near_1_id: i64 = conn
        .query_row(
            "SELECT image_id FROM image_sources WHERE original_filename = 'near_1.jpg'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let near_2_id: i64 = conn
        .query_row(
            "SELECT image_id FROM image_sources WHERE original_filename = 'near_2.jpg'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let control_id: i64 = conn
        .query_row(
            "SELECT image_id FROM image_sources WHERE original_filename = 'control.jpg'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let queue_rows: Vec<(i64, i64, i64, String)> = conn
        .prepare("SELECT image_a_id, image_b_id, hamming_distance, status FROM dedupe_review_queue")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(queue_rows.len(), 1, "expected exactly one dedupe_review_queue row, got {queue_rows:?}");
    let (a, b, distance, status) = &queue_rows[0];
    assert_eq!(
        (a.min(b), a.max(b)),
        (&near_1_id.min(near_2_id), &near_1_id.max(near_2_id)),
        "the queued pair must be the near-duplicate images"
    );
    assert_eq!(*distance, observed_near_distance as i64, "queued distance must match the real computed Hamming distance");
    assert_eq!(status, "pending");

    // ── Precision: the control image must not appear in the queue at all ─
    let control_in_queue: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dedupe_review_queue WHERE image_a_id = ?1 OR image_b_id = ?1",
            [control_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(control_in_queue, 0, "the control image must never appear in the review queue");

    // ── RAW+JPEG pairing ──────────────────────────────────────────────
    let raw_id: i64 = conn
        .query_row("SELECT image_id FROM image_sources WHERE original_filename = 'IMG_0001.cr2'", [], |row| row.get(0))
        .unwrap();
    let raw_jpeg_id: i64 = conn
        .query_row("SELECT image_id FROM image_sources WHERE original_filename = 'IMG_0001.jpg'", [], |row| row.get(0))
        .unwrap();

    let (raw_format, raw_width, raw_height, raw_perceptual_hash, raw_conversion_status): (
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        String,
    ) = conn
        .query_row(
            "SELECT original_format, width, height, perceptual_hash, conversion_status FROM images WHERE id = ?1",
            [raw_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(raw_format, "cr2");
    assert_eq!(raw_width, None, "a recognized-RAW file must have NULL width (no pixel decode attempted)");
    assert_eq!(raw_height, None);
    assert_eq!(raw_perceptual_hash, None, "a recognized-RAW file must have NULL perceptual_hash");
    assert_eq!(raw_conversion_status, "not_applicable", "RAW has no defined §4 conversion path");

    let pair_rows: Vec<(i64, i64)> = conn
        .prepare("SELECT raw_image_id, jpeg_image_id FROM raw_jpeg_pairs")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(pair_rows.len(), 1, "expected exactly one raw_jpeg_pairs row, got {pair_rows:?}");
    assert_eq!(pair_rows[0], (raw_id, raw_jpeg_id), "the pairing must link the RAW image to its JPEG counterpart");

    // Because perceptual_hash is NULL for the RAW side, it's automatically
    // excluded from perceptual dedupe candidacy — asserted directly here,
    // not just assumed from the NULL check above.
    let raw_in_queue: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dedupe_review_queue WHERE image_a_id = ?1 OR image_b_id = ?1",
            [raw_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_in_queue, 0, "the RAW file must never appear in the perceptual review queue");
    let raw_jpeg_side_in_queue: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dedupe_review_queue WHERE image_a_id = ?1 OR image_b_id = ?1",
            [raw_jpeg_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_jpeg_side_in_queue, 0, "the RAW file's paired JPEG must not independently land in the review queue either");
}
