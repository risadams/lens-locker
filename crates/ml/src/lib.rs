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

pub mod placeholder;

use std::path::{Path, PathBuf};

use ort::ep::DirectML;
use ort::session::Session;

#[derive(Debug, thiserror::Error)]
pub enum MlError {
    #[error("ONNX Runtime dynamic library not found at {0}; see MODELS.md for how to obtain it")]
    DylibNotFound(PathBuf),
    #[error(transparent)]
    Ort(#[from] ort::Error),
}

pub type Result<T> = std::result::Result<T, MlError>;

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
/// registered. Per-node fallback to CPU for any op DirectML doesn't
/// support is ONNX Runtime's own built-in behavior, not something this
/// wrapper implements. [`init`] must have already been called with a real
/// dylib path.
pub fn load_session(model_path: &Path) -> Result<Session> {
    // `with_execution_providers` returns `ort::Error<SessionBuilder>` (it
    // hands the builder back on failure so callers can recover); flatten
    // to the plain `ort::Error` (`R = ()`) `MlError::Ort` wraps, via the
    // `code`/`message` accessors that are generic over `R`.
    let mut builder = Session::builder()?
        .with_execution_providers([DirectML::default().build()])
        .map_err(|e| ort::Error::new_with_code(e.code(), e.message().to_string()))?;
    Ok(builder.commit_from_file(model_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ort::inputs;
    use ort::value::Tensor;

    /// The real end-to-end proof Milestone ML-1 asks for: [`load_session`]
    /// (the actual production function — not an ad-hoc rebuild of its
    /// internals) opens a `Session` at a [`ModelKind::relative_path`]-shaped
    /// path and runs a forward pass, DirectML EP registered. `#[ignore]`d
    /// because it needs the bundled ONNX Runtime DirectML dylib, which
    /// isn't available in this environment (see `MODELS.md`) — run with
    /// `cargo test -p lenslocker-ml -- --ignored` once
    /// `LENSLOCKER_MODELS_DIR` (or the exe-relative `models/` dir) has a
    /// real `onnxruntime.dll` in it.
    ///
    /// Deliberately does NOT need the real YuNet/SFace/SigLIP weights too:
    /// each placeholder graph is written out to its own isolated temp
    /// directory, under the same relative path a real export would use, so
    /// `load_session`'s file-resolution is exercised for real without
    /// touching (or depending on) whatever's actually sitting in the
    /// configured `models_dir()`.
    #[test]
    #[ignore = "needs the bundled ONNX Runtime DirectML dylib — see MODELS.md"]
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
