//! Sandboxed camera-RAW decode worker, run as a separate process so a
//! malformed RAW file (rawler is not hardened against untrusted input)
//! can't take down the main app. Per workplan/SPEC.md §5.
//!
//! Not yet implemented; scaffolded in Milestone 0.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("lumenvault-raw-worker: not yet implemented (Milestone 0 stub)");
    ExitCode::FAILURE
}
