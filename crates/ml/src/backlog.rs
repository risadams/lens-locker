//! Milestone ML-2's background backlog worker: ties [`tagging`] (model
//! inference), `lenslocker-decode` (image decode), and `lenslocker-catalog`
//! (the backlog query, `image_tags`/`embeddings` writes) into one
//! process-a-batch function.
//!
//! **Crate-boundary judgment call, flagged rather than silently
//! decided**: no closed ticket settled where orchestration spanning
//! decode + catalog + ML inference should live. `crates/import` already
//! owns exactly this *shape* of orchestration for the import pipeline
//! (`CLAUDE.md`: "`import` orchestrates `hash`/`decode`/`convert`/`xmp`
//! but owns no format-specific logic itself"), which made it the other
//! candidate — but ML-SPEC.md's own milestone name is literally "ML-2 —
//! Tagging pipeline", and this crate already houses the tagging-specific
//! pipeline logic ([`tagging`]) this orchestration calls into, so keeping
//! it here avoids a third crate boundary for what's currently a handful
//! of functions. Revisit if Milestone ML-3's face pipeline (a
//! structurally different shape — detection *and* embedding, clustering)
//! needs something this doesn't fit.
//!
//! **Not yet wired to the real background execution model (§9)** — no
//! `AnalysisLock`, no ambient progress, no per-image connection-lock
//! discipline beyond what [`process_backlog_batch`]'s caller chooses to
//! do. §9's own text assigns the ambient-UI half of this to Milestone
//! ML-6; this only provides the batch-processing function that milestone
//! will call into.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::labels::STARTER_LABELS;
use crate::tagging::text::TextEncoder;
use crate::{ModelKind, Result, tagging};

/// This project's own name/version for the bundled SigLIP checkpoint —
/// not upstream's own versioning (SigLIP doesn't publish one in a form
/// suited to `models.version`), chosen so a future deliberate model swap
/// (e.g. a different checkpoint or a re-export) can bump this string to
/// trigger §9's "re-analysis on a model upgrade" path. A judgment call,
/// flagged rather than sourced from anywhere.
const SIGLIP_MODEL_NAME: &str = "siglip-so400m-patch14-384";
const SIGLIP_MODEL_VERSION: &str = "v1";
const SIGLIP_LICENSE_NOTE: &str = "Apache-2.0 (google/siglip-so400m-patch14-384, self-converted — MODELS.md §4)";

/// A loaded SigLIP session plus every starter label's pre-computed text
/// embedding (§4: "adding a custom label is a cheap backfill... not a
/// re-embed" — the same principle applies across a single backlog pass:
/// encode each label once per [`TaggingModel::load`], not once per image).
pub struct TaggingModel {
    session: ort::session::Session,
    label_embeddings: Vec<(String, Vec<f32>)>,
    model_id: i64,
}

impl TaggingModel {
    /// Loads the real SigLIP session and tokenizer from `models_dir`, and
    /// embeds every [`STARTER_LABELS`] entry once. `conn` is used only to
    /// find-or-create the `models` row this session's embeddings will be
    /// stored against — no backlog work happens here.
    pub fn load(conn: &Connection, models_dir: &Path) -> Result<Self> {
        let model_path = models_dir.join(ModelKind::Siglip.relative_path());
        let mut session = crate::load_session(&model_path)?;

        let tokenizer_path = models_dir.join("siglip-so400m-onnx").join("tokenizer.json");
        let text_encoder = TextEncoder::load(&tokenizer_path)?;

        let mut label_embeddings = Vec::with_capacity(STARTER_LABELS.len());
        for &label in STARTER_LABELS {
            let ids = text_encoder.tokenize(label)?;
            let embedding = tagging::text::embed_text(&mut session, &ids)?;
            label_embeddings.push((label.to_string(), embedding));
        }

        let model_id = lenslocker_catalog::find_or_create_model(
            conn,
            SIGLIP_MODEL_NAME,
            SIGLIP_MODEL_VERSION,
            tagging::EMBEDDING_DIM as i64,
            SIGLIP_LICENSE_NOTE,
        )?;

        Ok(Self { session, label_embeddings, model_id })
    }

    pub fn model_id(&self) -> i64 {
        self.model_id
    }

    /// Processes up to `batch_size` images from [`lenslocker_catalog::images_needing_embedding`]:
    /// decode, embed, store the embedding (durable row + live
    /// [`lenslocker_catalog::VecMirror`]), score against every starter
    /// label, and apply auto-tags that clear `thresholds.tag_storage_threshold`
    /// (§4). Returns how many images were processed — `0` means the
    /// backlog is empty, the caller's signal to stop polling for now.
    ///
    /// One `Connection`/one `VecMirror` for the whole batch — per-image
    /// lock acquisition (§9's "never blocks interactive commands"
    /// requirement) is the caller's job once this is wired behind
    /// `AppState.conn`'s mutex; this function has no opinion on locking.
    pub fn process_backlog_batch(
        &mut self,
        conn: &Connection,
        vec_mirror: &lenslocker_catalog::VecMirror,
        batch_size: i64,
        tag_storage_threshold: f64,
    ) -> Result<usize> {
        let image_ids = lenslocker_catalog::images_needing_embedding(conn, self.model_id, batch_size)?;
        for &image_id in &image_ids {
            self.process_one_image(conn, vec_mirror, image_id, tag_storage_threshold)?;
        }
        Ok(image_ids.len())
    }

    fn process_one_image(&mut self, conn: &Connection, vec_mirror: &lenslocker_catalog::VecMirror, image_id: i64, tag_storage_threshold: f64) -> Result<()> {
        let stored_path: String = conn.query_row("SELECT stored_path FROM images WHERE id = ?1", [image_id], |row| row.get(0))?;
        let stored_path = PathBuf::from(stored_path);

        let probe = lenslocker_decode::probe(&stored_path).map_err(|source| crate::MlError::Decode { path: stored_path, source })?;
        let pixel_values = tagging::preprocess_image(&probe.image);
        let image_embedding = tagging::embed_image(&mut self.session, &pixel_values)?;

        let vector_bytes = crate::encode_embedding(&image_embedding);
        lenslocker_catalog::upsert_embedding(conn, image_id, self.model_id, &vector_bytes, tagging::EMBEDDING_DIM as i64)?;
        vec_mirror.upsert(image_id, &vector_bytes)?;

        for (label, label_embedding) in &self.label_embeddings {
            let probability = tagging::zero_shot_probability(&image_embedding, label_embedding);
            if f64::from(probability) >= tag_storage_threshold {
                lenslocker_catalog::apply_auto_tag(conn, image_id, label, f64::from(probability))?;
            }
        }

        Ok(())
    }
}
