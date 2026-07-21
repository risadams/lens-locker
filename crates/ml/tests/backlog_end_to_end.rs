//! Real end-to-end proof of Milestone ML-2's exit criteria: "import a real
//! folder, confirm auto-tags land with correct provenance/confidence,
//! confirm a similarity query against the in-memory mirror returns in well
//! under 200ms." A single synthetic image stands in for "a real folder" —
//! this test is about the pipeline wiring being correct, not about
//! exercising a real photo library's scale (that's a separate, later
//! concern). `#[ignore]`d like this crate's other real-model tests.

use std::time::Instant;

use image::{ImageBuffer, Rgb};
use lenslocker_catalog::VecMirror;
use lenslocker_ml::backlog::TaggingModel;
use rusqlite::Connection;

fn migrated_conn() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    lenslocker_catalog::migrate(&mut conn).unwrap();
    conn
}

fn insert_test_image(conn: &Connection, stored_path: &std::path::Path) -> i64 {
    conn.execute("INSERT INTO libraries (name, root_path) VALUES ('lib', 'A:/lib')", []).unwrap();
    let library_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO images (
            library_id, original_hash, stored_hash, stored_path,
            original_format, stored_format, file_size_bytes
        ) VALUES (?1, x'01', x'01', ?2, 'png', 'png', 0)",
        rusqlite::params![library_id, stored_path.to_string_lossy()],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
#[ignore = "needs the bundled ONNX Runtime dylib + the real SigLIP export + tokenizer.json — see MODELS.md"]
fn backlog_batch_tags_a_real_image_and_the_vec_mirror_finds_it_back() {
    lenslocker_ml::init(&lenslocker_ml::dylib_path()).expect("init the bundled onnxruntime dylib");

    let tmp = tempfile::tempdir().unwrap();
    let image_path = tmp.path().join("test.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(64, 64, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 { Rgb([230u8, 210, 160]) } else { Rgb([40u8, 90, 160]) }
    });
    img.save(&image_path).unwrap();

    let conn = migrated_conn();
    let image_id = insert_test_image(&conn, &image_path);

    let model_id = lenslocker_ml::similarity::resolve_siglip_model_id(&conn).unwrap();
    let mut model = TaggingModel::load(model_id, &lenslocker_ml::models_dir()).expect("load the real SigLIP model + tokenizer");
    let mirror = VecMirror::build(&conn, model.model_id(), lenslocker_ml::tagging::EMBEDDING_DIM).unwrap();

    // Storage threshold of 0.0 — accept every scored label, since this
    // test's synthetic checkerboard image has no real semantic content to
    // reliably clear a real confidence bar; the point here is provenance
    // wiring, not real tagging accuracy.
    let processed = model.process_backlog_batch(&conn, &mirror, 10, 0.0).unwrap();
    assert_eq!(processed, 1);

    // A real embedding row should now exist.
    let embedding_count: i64 = conn.query_row("SELECT COUNT(*) FROM embeddings WHERE image_id = ?1", [image_id], |row| row.get(0)).unwrap();
    assert_eq!(embedding_count, 1);

    // At least one auto-tag should have landed with correct provenance.
    let auto_tag_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM image_tags WHERE image_id = ?1 AND source = 'auto' AND review_state = 'unreviewed'", [image_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(auto_tag_count > 0, "expected at least one auto-tag to land");

    // Re-running the backlog batch must be a no-op (the image no longer
    // needs an embedding) — resumability (§9).
    let processed_again = model.process_backlog_batch(&conn, &mirror, 10, 0.0).unwrap();
    assert_eq!(processed_again, 0);

    // The in-memory mirror finds the same image back as its own nearest
    // neighbor, well under the ~200ms interactive bar (§2).
    let vector: Vec<u8> = conn.query_row("SELECT vector FROM embeddings WHERE image_id = ?1", [image_id], |row| row.get(0)).unwrap();
    let start = Instant::now();
    let results = mirror.query_similar(&vector, 1).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(results[0].0, image_id);
    assert!(elapsed.as_millis() < 200, "similarity query took {elapsed:?}, over the ~200ms interactive bar");
}
