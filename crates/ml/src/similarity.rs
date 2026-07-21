//! Live, query-time similarity-search support (ML-SPEC.md §8, ticket 034,
//! Milestone ML-5) — distinct from [`crate::backlog`]'s batch
//! orchestration: this module has no catalog-writing side effects, just
//! two small building blocks a live Tauri command needs. Text-query
//! embedding runs CPU-only ([`crate::load_session_cpu`]), never DirectML:
//! [`crate::load_session`]'s own doc comment already establishes that at
//! most one DirectML session succeeds per process, ever — the backlog's
//! own SigLIP session may already have claimed and dropped that one slot
//! earlier in the process's life, so a second, separate `load_session`
//! call here later would crash exactly the way ML-3's diagnosis found.
//! CPU sessions carry no such limit.

use std::path::Path;

use rusqlite::Connection;

use crate::{ModelKind, Result, tagging};

/// This project's own name/version for the bundled SigLIP checkpoint —
/// not upstream's own versioning (SigLIP doesn't publish one in a form
/// suited to `models.version`), chosen so a future deliberate model swap
/// (e.g. a different checkpoint or a re-export) can bump this string to
/// trigger §9's "re-analysis on a model upgrade" path. A judgment call,
/// flagged rather than sourced from anywhere. The same identity
/// [`crate::backlog::TaggingModel::load`] resolves, now shared through
/// this one function so the two callers can't drift.
const SIGLIP_MODEL_NAME: &str = "siglip-so400m-patch14-384";
const SIGLIP_MODEL_VERSION: &str = "v1";
const SIGLIP_LICENSE_NOTE: &str = "Apache-2.0 (google/siglip-so400m-patch14-384, self-converted — MODELS.md §4)";

/// Finds (or creates, if the backlog has never run yet) the `models` row
/// identifying SigLIP — the id both the backlog's batch processing and
/// live similarity-search queries need to key `embeddings`/`VecMirror`
/// lookups against. Creating one lazily on a query-time lookup (rather
/// than requiring the backlog to have run first) is harmless: an empty
/// model row with zero embeddings just means `VecMirror::build` loads
/// nothing, which the caller already has to handle as "not analyzed yet."
pub fn resolve_siglip_model_id(conn: &Connection) -> rusqlite::Result<i64> {
    lenslocker_catalog::find_or_create_model(conn, SIGLIP_MODEL_NAME, SIGLIP_MODEL_VERSION, tagging::EMBEDDING_DIM as i64, SIGLIP_LICENSE_NOTE)
}

/// Embeds one typed search phrase for text-to-image search — a single,
/// lightweight CPU inference (loads a fresh session, tokenizes, runs the
/// text tower once, drops the session), not
/// [`crate::backlog::TaggingModel::load`]'s much heavier DirectML session
/// + every-starter-label setup, which is built for a long batch, not a
/// one-off interactive query.
pub fn embed_text_query(models_dir: &Path, query: &str) -> Result<Vec<f32>> {
    let model_path = models_dir.join(ModelKind::Siglip.relative_path());
    let mut session = crate::load_session_cpu(&model_path)?;

    let tokenizer_path = crate::siglip_tokenizer_path(models_dir);
    let text_encoder = tagging::text::TextEncoder::load(&tokenizer_path)?;

    let ids = text_encoder.tokenize(query)?;
    tagging::text::embed_text(&mut session, &ids)
}
