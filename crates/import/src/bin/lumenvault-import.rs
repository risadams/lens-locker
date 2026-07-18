//! Milestone 1 import CLI: `lumenvault-import <source-dir> <library-root>`.
//! No flags — deliberately minimal for a milestone with no UI yet.
//!
//! Prints one flushed line per file processed, so a test harness (or a
//! human) can observe progress without waiting for the whole batch. An
//! optional `LUMENVAULT_IMPORT_DELAY_MS` env var sleeps that long after each
//! file — used by the crash-recovery integration test to reliably kill the
//! process partway through a batch; unset (0) in normal use.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use lumenvault_import::{
    FileOutcome, ImportContext, LibraryPaths, conversion_enabled, ensure_library, import_directory,
    start_or_resume_batch, sweep_expired_quarantine,
};
use rusqlite::Connection;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, source_dir, library_root] = args.as_slice() else {
        eprintln!("usage: lumenvault-import <source-dir> <library-root>");
        return std::process::ExitCode::FAILURE;
    };

    if let Err(e) = run(PathBuf::from(source_dir), PathBuf::from(library_root)) {
        eprintln!("lumenvault-import: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn run(source_dir: PathBuf, library_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let delay = std::env::var("LUMENVAULT_IMPORT_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    std::fs::create_dir_all(&library_root)?;
    let paths = LibraryPaths::new(&library_root);

    let mut conn = Connection::open(paths.catalog_db())?;
    lumenvault_catalog::migrate(&mut conn)?;

    let purged = sweep_expired_quarantine(&conn)?;
    if purged > 0 {
        println!("retention sweep: purged {purged} expired quarantine entr{}", if purged == 1 { "y" } else { "ies" });
    }

    let library_id = ensure_library(&conn, &library_root)?;
    let batch_id = start_or_resume_batch(&conn, library_id, &source_dir)?;
    let ctx = ImportContext {
        conn: &conn,
        paths: &paths,
        library_id,
        batch_id,
        conversion_enabled: conversion_enabled(&conn, library_id)?,
    };

    let mut imported = 0;
    let mut collapsed = 0;
    let mut failed = 0;
    let mut already_done = 0;

    import_directory(&ctx, &source_dir, |path, outcome| {
        match outcome {
            FileOutcome::Imported { image_id } => {
                imported += 1;
                println!("imported: {} (image_id={image_id})", path.display());
            }
            FileOutcome::Collapsed { image_id } => {
                collapsed += 1;
                println!("collapsed (duplicate): {} (image_id={image_id})", path.display());
            }
            FileOutcome::Failed => {
                failed += 1;
                println!("failed (undecodable): {}", path.display());
            }
            FileOutcome::AlreadyDone { image_id } => {
                already_done += 1;
                println!("already done (resumed): {} (image_id={image_id:?})", path.display());
            }
        }
        let _ = std::io::stdout().flush();

        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
    })?;

    println!(
        "batch complete: {imported} imported, {collapsed} collapsed, {failed} failed, {already_done} already done"
    );
    Ok(())
}
