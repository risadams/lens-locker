//! Milestone 1's actual exit criterion (workplan/SPEC.md Part 2): "import a
//! real folder of JPEGs/PNGs, kill the process mid-batch, confirm resume
//! works correctly." This spawns the real compiled CLI binary as a genuine
//! OS subprocess and hard-kills it — not a simulation.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use image::{ImageBuffer, Rgb};
use rusqlite::Connection;

const FILE_COUNT: usize = 6;
const KILL_AFTER_IMPORTED: usize = 2;

fn write_distinct_png(dir: &std::path::Path, index: u8) -> std::path::PathBuf {
    let path = dir.join(format!("photo-{index}.png"));
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(16, 16, |x, y| {
        Rgb([index, (x as u8).wrapping_add(index), y as u8])
    });
    img.save(&path).unwrap();
    path
}

#[test]
fn killing_the_import_mid_batch_and_resuming_finishes_correctly() {
    let source_dir = tempfile::tempdir().unwrap();
    let library_dir = tempfile::tempdir().unwrap();

    let mut expected_paths = Vec::new();
    for i in 0..FILE_COUNT as u8 {
        expected_paths.push(write_distinct_png(source_dir.path(), i));
    }

    let bin = env!("CARGO_BIN_EXE_lenslocker-import");

    // First run: slow it down so we can observe progress and kill it
    // partway through a batch of six files.
    let mut child = Command::new(bin)
        .arg(source_dir.path())
        .arg(library_dir.path())
        .env("LENSLOCKER_IMPORT_DELAY_MS", "150")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn lenslocker-import");

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();

    let mut completed = 0;
    while completed < KILL_AFTER_IMPORTED {
        let line = lines
            .next()
            .expect("process exited before completing enough files to test a mid-batch kill")
            .unwrap();
        if line.starts_with("imported:") {
            completed += 1;
        }
    }

    child.kill().expect("failed to kill the import process");
    child.wait().expect("failed to reap the killed process");

    // Sanity check this was actually a mid-batch kill, not a race that let
    // the whole batch finish first.
    let journal_conn = Connection::open(library_dir.path().join("catalog.sqlite")).unwrap();
    let done_before_resume: i64 = journal_conn
        .query_row(
            "SELECT COUNT(*) FROM import_journal WHERE step = 'done'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        (1..FILE_COUNT as i64).contains(&done_before_resume),
        "expected the kill to land mid-batch (some but not all files done), got {done_before_resume}/{FILE_COUNT}"
    );
    drop(journal_conn);

    // Second run: same command, no artificial delay — this is the resume.
    let status = Command::new(bin)
        .arg(source_dir.path())
        .arg(library_dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("failed to spawn the resumed lenslocker-import run");
    assert!(status.success(), "the resumed run must exit successfully");

    // Assert on the real, final state.
    let conn = Connection::open(library_dir.path().join("catalog.sqlite")).unwrap();

    let image_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        image_count, FILE_COUNT as i64,
        "every distinct source file must have exactly one images row"
    );

    let done_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM import_journal WHERE step = 'done'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        done_count, FILE_COUNT as i64,
        "every journal entry must reach 'done', including the interrupted one"
    );

    let not_done_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM import_journal WHERE step != 'done'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        not_done_count, 0,
        "nothing should be left half-processed after resume"
    );

    let source_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM image_sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        source_count, FILE_COUNT as i64,
        "no file should be double-counted by the resume"
    );

    let quarantine_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM quarantine", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quarantine_count, FILE_COUNT as i64);

    // Every original must be gone from the source dir, and every image's
    // stored blob must exist on disk with the hash the catalog claims.
    for path in &expected_paths {
        assert!(
            !path.exists(),
            "{path:?} should have been moved into quarantine"
        );
    }

    let mut stmt = conn
        .prepare("SELECT stored_path, stored_hash FROM images")
        .unwrap();
    let rows: Vec<(String, Vec<u8>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), FILE_COUNT);
    for (stored_path, stored_hash) in rows {
        let blob_path = std::path::Path::new(&stored_path);
        assert!(blob_path.exists(), "blob missing on disk: {stored_path}");
        let actual_hash = lenslocker_hash::hash_file(blob_path).unwrap();
        assert_eq!(
            actual_hash.to_vec(),
            stored_hash,
            "blob content doesn't match its recorded hash: {stored_path}"
        );
    }

    let mut quarantine_stmt = conn
        .prepare("SELECT quarantined_path FROM quarantine")
        .unwrap();
    let quarantine_paths: Vec<String> = quarantine_stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for path in quarantine_paths {
        assert!(
            std::path::Path::new(&path).exists(),
            "quarantined original missing on disk: {path}"
        );
    }
}
