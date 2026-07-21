//! ONNX Runtime + DirectML integration scaffolding.
//!
//! ML-SPEC.md Milestone ML-1: proves the `Session`/DirectML plumbing works
//! — no inference pipeline logic and no UI yet (that's ML-2/ML-3). One
//! shared `ort` environment, `Session`s created per model on demand (§2:
//! "the natural shape of the `ort` API itself, avoiding redundant DirectML
//! device initialization").
//!
//! Uses `ort`'s `load-dynamic` feature (dlopen the runtime at process
//! start) rather than linking against it or using `download-binaries`:
//! the DirectML `onnxruntime.dll` is a bundled installer asset (§10, and
//! see `MODELS.md`), never fetched at build or run time — `zero network
//! access, ever` is this repo's first non-negotiable constraint, and
//! `download-binaries` would also pull in `ureq` as an optional
//! dependency, which `deny.toml` bans outright.

pub mod backlog;
pub mod faces;
pub mod labels;
pub mod placeholder;
pub mod similarity;
pub mod tagging;

use std::path::{Path, PathBuf};

use ort::ep::DirectML;
use ort::session::Session;
use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum MlError {
    #[error("ONNX Runtime dynamic library not found at {0}; see MODELS.md for how to obtain it")]
    DylibNotFound(PathBuf),
    #[error(transparent)]
    Ort(#[from] ort::Error),
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    #[error(transparent)]
    Catalog(#[from] rusqlite::Error),
    #[error("could not decode {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: lenslocker_decode::ProbeError,
    },
    #[error("could not write face crop thumbnail to {path}: {source}")]
    ThumbnailWrite {
        path: PathBuf,
        #[source]
        source: lenslocker_decode::ThumbnailError,
    },
}

pub type Result<T> = std::result::Result<T, MlError>;

/// Encodes values as little-endian `f32` bytes — how every embedding
/// column in this workspace (`embeddings.vector`, `face_detections.embedding`)
/// is stored; shared by [`tagging`] and [`faces`] rather than each
/// re-deriving its own copy.
pub fn encode_embedding(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Decodes little-endian `f32` bytes back into values — the inverse of
/// [`encode_embedding`].
pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// The three bundled models (§2), by their expected path within
/// [`models_dir`]. Matches the upstream source layout exactly — OpenCV
/// Zoo's own per-model subdirectories for the face pair, `optimum-cli`'s
/// own `<name>/model.onnx` export layout for SigLIP (its sibling
/// `model.onnx_data` external-weights file must sit alongside it, per
/// ONNX's own external-data convention — see `MODELS.md`) — so a real
/// export can be dropped in with no code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Yunet,
    Sface,
    Siglip,
}

impl ModelKind {
    pub fn relative_path(self) -> &'static str {
        match self {
            ModelKind::Yunet => "face_detection_yunet/face_detection_yunet_2023mar.onnx",
            ModelKind::Sface => "face_recognition_sface/face_recognition_sface_2021dec.onnx",
            ModelKind::Siglip => "siglip-so400m-onnx/model.onnx",
        }
    }
}

/// SigLIP's tokenizer, sibling to [`ModelKind::Siglip`]'s own `model.onnx`
/// (same directory — see [`ModelKind::relative_path`]) — every SigLIP text
/// encoding call site (`backlog.rs`'s batch tagging, `similarity.rs`'s
/// live text-query embedding) needs this same path, so it's one function
/// rather than each call site re-joining the same two path segments.
pub fn siglip_tokenizer_path(models_dir: &Path) -> PathBuf {
    models_dir.join("siglip-so400m-onnx").join("tokenizer.json")
}

/// Shared body of `similarity::resolve_siglip_model_id`/
/// `backlog::resolve_sface_model_id`: find-or-create the `models` row,
/// and — only on a genuine version bump (a brand new row for a name that
/// already had a prior, different version) — record a
/// [`lenslocker_catalog::create_model_upgrade_notice`] (ticket 030
/// decision #4, ML-SPEC.md §9). First-ever install never creates a
/// notice: [`lenslocker_catalog::most_recent_other_model_version`]
/// returns `None` when there's nothing to be "upgrading" from.
pub(crate) fn resolve_model_id_and_maybe_notice(conn: &Connection, name: &str, version: &str, dimension: i64, license_note: &str) -> rusqlite::Result<i64> {
    let (model_id, is_new) = lenslocker_catalog::find_or_create_model(conn, name, version, dimension, license_note)?;
    if is_new {
        if let Some(old_version) = lenslocker_catalog::most_recent_other_model_version(conn, name, model_id)? {
            lenslocker_catalog::create_model_upgrade_notice(conn, model_id, name, &old_version, version)?;
        }
    }
    Ok(model_id)
}

/// Resolves the directory holding the bundled ONNX Runtime dylib and the
/// three model files. Checks `LENSLOCKER_MODELS_DIR` first (test/dev
/// override — real installs don't set it), then falls back to a `models/`
/// directory next to the running executable, matching §10's "bundled
/// directly in the NSIS installer, extracted... into the Program Files
/// install directory."
pub fn models_dir() -> PathBuf {
    resolve_models_dir(std::env::var("LENSLOCKER_MODELS_DIR").ok(), std::env::current_exe().ok())
}

/// The pure decision behind [`models_dir`], factored out so tests can
/// exercise both branches without mutating process-global environment
/// state (which edition 2024 makes `unsafe`, and this workspace's lint
/// table denies `unsafe_code` outright — see `CLAUDE.md`).
fn resolve_models_dir(env_override: Option<String>, exe_path: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = env_override {
        return PathBuf::from(dir);
    }
    exe_path
        .and_then(|exe| exe.parent().map(|dir| dir.join("models")))
        .unwrap_or_else(|| PathBuf::from("models"))
}

/// The bundled ONNX Runtime DirectML build's path within [`models_dir`].
pub fn dylib_path() -> PathBuf {
    models_dir().join(if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else {
        // Not a shipping target (SPEC.md §2/§4 is Windows-only) — only
        // relevant for running this crate's own tests on a dev machine.
        "libonnxruntime.so"
    })
}

/// One-time process-wide ONNX Runtime init, dynamically loading the
/// bundled DirectML build from `dylib_path` (never linked, never
/// downloaded — see module docs). Idempotent: `ort::init_from(...).commit()`
/// returns `false` if an environment is already configured by an earlier
/// call; that's not an error, per `EnvironmentBuilder::commit`'s own
/// documented contract.
///
/// Telemetry is disabled explicitly rather than left at `ort`'s own
/// default (`true`) — §12's offline-enforcement criterion, applied here at
/// first use rather than deferred to Milestone ML-6's re-verification
/// pass, matching `SPEC.md` §8's WebView2-hardening precedent of not
/// leaving a known phone-home-shaped default in place "for now."
pub fn init(dylib_path: &Path) -> Result<()> {
    if !dylib_path.is_file() {
        return Err(MlError::DylibNotFound(dylib_path.to_path_buf()));
    }
    ort::init_from(dylib_path)?
        .with_name("lenslocker")
        .with_telemetry(false)
        .commit();
    Ok(())
}

/// Opens `model_path` as a `Session` with the DirectML execution provider
/// registered. [`init`] must have already been called with a real dylib
/// path.
///
/// **Budget: at most one call to this function may ever succeed per
/// process — not "at most one at a time," literally at most one, full
/// stop, for the process's entire lifetime, regardless of whether earlier
/// DirectML sessions have long since been dropped.** Confirmed
/// empirically (a throwaway diagnostic harness, not committed): a second
/// `Session::builder()...with_execution_providers([DirectML...])` call
/// crashes the whole process with `STATUS_ACCESS_VIOLATION` even when the
/// first session was already out of scope, on the official stable
/// `onnxruntime` v1.27.1 DirectML build. This is *stricter* than this
/// crate's earlier documented finding ("never hold two DirectML sessions
/// open concurrently") — that workaround was itself untested for the
/// sequential load→drop→load-again case, which also crashes; corrected
/// here rather than left wrong (see `ML-SPEC.md`'s Milestone ML-1
/// addendum for the fuller history). Plain CPU sessions
/// ([`load_session_cpu`]) are unaffected — any number, any order, before
/// or after the one DirectML session — so the practical rule for this
/// codebase is: **exactly one model gets DirectML per process; every
/// other model in that same process must use [`load_session_cpu`].**
/// Milestone ML-2's `TaggingModel` claims that one slot for SigLIP;
/// Milestone ML-3's YuNet/SFace both use [`load_session_cpu`] instead.
pub fn load_session(model_path: &Path) -> Result<Session> {
    // `with_execution_providers`/`with_memory_pattern` return
    // `ort::Error<SessionBuilder>` (they hand the builder back on failure
    // so callers can recover); flatten to the plain `ort::Error` (`R = ()`)
    // `MlError::Ort` wraps, via the `code`/`message` accessors that are
    // generic over `R`.
    let flatten = |e: ort::Error<ort::session::builder::SessionBuilder>| ort::Error::new_with_code(e.code(), e.message().to_string());

    // Memory-pattern optimization must be off for the DirectML EP — its
    // own docs require this (onnxruntime.ai/docs/execution-providers/
    // DirectML-ExecutionProvider.html: "execution_mode must be set to
    // ORT_SEQUENTIAL, and enable_mem_pattern must be false"; execution mode
    // is already sequential by `ort`'s own default). Set explicitly here
    // for the record even though ONNX Runtime turns out to force this off
    // itself the moment a DML EP is registered regardless (logged as
    // `inference_session.cc: Having memory pattern enabled is not
    // supported while using the DML Execution Provider... disabling it`)
    // — so this did **not** turn out to be the fix for the crash this
    // function's own doc comment now documents precisely.
    let mut builder = Session::builder()?
        .with_execution_providers([DirectML::default().build()])
        .map_err(flatten)?
        .with_memory_pattern(false)
        .map_err(flatten)?;
    Ok(builder.commit_from_file(model_path)?)
}

/// Opens `model_path` as a plain CPU `Session` — no execution provider
/// registered. Safe to call any number of times, in any order relative to
/// the one [`load_session`] (DirectML) call a process may make — see that
/// function's doc comment for why this distinction exists at all.
pub fn load_session_cpu(model_path: &Path) -> Result<Session> {
    Ok(Session::builder()?.commit_from_file(model_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ort::inputs;
    use ort::value::Tensor;

    /// The real end-to-end proof Milestone ML-1 asks for: [`load_session`]
    /// (the actual production function — not an ad-hoc rebuild of its
    /// internals) opens a `Session` at a [`ModelKind::relative_path`]-shaped
    /// path and runs a forward pass, DirectML EP registered. `#[ignore]`d by
    /// default since it needs a real `onnxruntime.dll` at
    /// `LENSLOCKER_MODELS_DIR` (or the exe-relative `models/` dir) — see
    /// `MODELS.md`.
    ///
    /// Deliberately does NOT need the real YuNet/SFace/SigLIP weights too:
    /// each placeholder graph is written out to its own isolated temp
    /// directory, under the same relative path a real export would use, so
    /// `load_session`'s file-resolution is exercised for real without
    /// touching (or depending on) whatever's actually sitting in the
    /// configured `models_dir()`.
    ///
    /// **Deliberately still reproduces the known DirectML budget** (see
    /// [`load_session`]'s doc comment for the fully-characterized finding:
    /// at most one DirectML session ever succeeds per process, confirmed
    /// against the official stable `onnxruntime` v1.27.1 DirectML build,
    /// not a nightly quirk as first suspected). Calling [`load_session`]
    /// in a loop for all three model slots is exactly the crash case —
    /// this test exists to keep that fact honest and reproducible in the
    /// test suite rather than only in a doc comment, not because
    /// Milestone ML-2/ML-3's real pipelines actually do this (they don't:
    /// `crates/ml::backlog::TaggingModel` uses [`load_session`] for
    /// SigLIP only; `crates/ml::backlog::process_face_backlog_batch` uses
    /// [`load_session_cpu`] for both YuNet and SFace).
    #[test]
    #[ignore = "known: only one DirectML session ever succeeds per process — see load_session's doc comment"]
    fn sessions_load_and_run_a_forward_pass_for_each_model_slot() {
        init(&dylib_path()).expect("init the bundled onnxruntime dylib");

        // Shapes are illustrative placeholders standing in for each real
        // model's slot, not confirmed real input shapes (that's ML-2/ML-3
        // pipeline-logic work, once the real files are in hand).
        for (kind, dims) in [
            (ModelKind::Yunet, vec![1i64, 3, 120, 160]),
            (ModelKind::Sface, vec![1i64, 3, 112, 112]),
            (ModelKind::Siglip, vec![1i64, 3, 384, 384]),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let model_path = tmp.path().join(kind.relative_path());
            std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
            std::fs::write(&model_path, placeholder::identity_graph_model("input", "output", &dims)).unwrap();

            let mut session = load_session(&model_path).unwrap();

            let element_count: usize = dims.iter().product::<i64>() as usize;
            let input = Tensor::from_array((dims.clone(), vec![0f32; element_count])).unwrap();
            let outputs = session.run(inputs!["input" => input]).unwrap();

            assert!(
                outputs.get("output").is_some(),
                "{kind:?}: expected an `output` tensor back from the placeholder graph"
            );
        }
    }

    #[test]
    fn resolve_models_dir_prefers_the_env_override() {
        assert_eq!(
            resolve_models_dir(Some("A:/wherever".to_string()), Some(PathBuf::from("A:/app/lenslocker.exe"))),
            PathBuf::from("A:/wherever")
        );
    }

    #[test]
    fn resolve_models_dir_falls_back_to_exe_relative_models() {
        assert_eq!(
            resolve_models_dir(None, Some(PathBuf::from("A:/app/lenslocker.exe"))),
            PathBuf::from("A:/app/models")
        );
    }

    #[test]
    fn model_kind_relative_paths_match_upstream_export_layouts() {
        assert_eq!(
            ModelKind::Yunet.relative_path(),
            "face_detection_yunet/face_detection_yunet_2023mar.onnx"
        );
        assert_eq!(
            ModelKind::Sface.relative_path(),
            "face_recognition_sface/face_recognition_sface_2021dec.onnx"
        );
        assert_eq!(ModelKind::Siglip.relative_path(), "siglip-so400m-onnx/model.onnx");
    }

    #[test]
    fn init_reports_a_clear_error_when_the_dylib_is_missing() {
        let err = init(Path::new("A:/definitely/does/not/exist/onnxruntime.dll")).unwrap_err();
        assert!(matches!(err, MlError::DylibNotFound(_)));
    }
}
