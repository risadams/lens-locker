//! Real end-to-end proof of Milestone ML-3's plumbing: a synthetic image
//! (no real face — none available in this environment, see
//! `yunet_real_model.rs`) goes through the full two-pass backlog batch
//! without crashing, gets marked processed (the embeddings-table backlog
//! marker), and re-running the batch is a verified no-op. Real clustering
//! behavior ("import a folder with repeated faces, confirm clustering
//! groups them reasonably" — Milestone ML-3's actual exit criteria) is
//! covered separately, with real detections, entirely in-memory/no-model
//! (`crate::faces::match_face`'s own tests) — that's the part no
//! synthetic image can exercise for real without an actual photographed
//! face. `#[ignore]`d like this crate's other real-model tests.

use image::{ImageBuffer, Rgb};
use lenslocker_ml::backlog::process_face_backlog_batch;
use lenslocker_ml::faces::FaceThresholds;
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
        ) VALUES (?1, x'02', x'02', ?2, 'png', 'png', 0)",
        rusqlite::params![library_id, stored_path.to_string_lossy()],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn thresholds() -> FaceThresholds {
    FaceThresholds::schema_defaults()
}

#[test]
#[ignore = "needs the bundled ONNX Runtime dylib + the real YuNet/SFace exports — see MODELS.md"]
fn face_backlog_batch_marks_a_faceless_image_processed_and_is_resumable() {
    lenslocker_ml::init(&lenslocker_ml::dylib_path()).expect("init the bundled onnxruntime dylib");

    let tmp = tempfile::tempdir().unwrap();
    let image_path = tmp.path().join("noise.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(300, 200, |x, y| Rgb([((x * 7) % 256) as u8, ((y * 11) % 256) as u8, 90]));
    img.save(&image_path).unwrap();

    let conn = migrated_conn();
    let image_id = insert_test_image(&conn, &image_path);

    let processed = process_face_backlog_batch(&conn, &lenslocker_ml::models_dir(), tmp.path(), 10, &thresholds()).expect("the batch should run without error end to end");
    assert_eq!(processed, 1);

    // No real face in synthetic noise, but the image must still be marked
    // processed (an sface-model embeddings row must exist) so the backlog
    // doesn't loop on it forever.
    let embedding_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM embeddings e JOIN models m ON m.id = e.model_id
             WHERE e.image_id = ?1 AND m.name = 'sface-2021dec'",
            [image_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(embedding_count, 1);

    // Re-running must be a no-op — resumability (§9), same pattern as
    // Milestone ML-2's tagging backlog.
    let processed_again = process_face_backlog_batch(&conn, &lenslocker_ml::models_dir(), tmp.path(), 10, &thresholds()).unwrap();
    assert_eq!(processed_again, 0);
}
