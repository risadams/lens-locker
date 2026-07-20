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
//! of functions. Milestone ML-3's face pipeline ([`process_face_backlog_batch`])
//! turned out to fit the same file/crate fine, despite its different
//! shape (detection *and* embedding, clustering) — it just needed its
//! own function, not a new module boundary.
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

/// This project's own name/version for the bundled SFace checkpoint —
/// same judgment call as [`SIGLIP_MODEL_VERSION`], flagged rather than
/// sourced from anywhere upstream.
const SFACE_MODEL_NAME: &str = "sface-2021dec";
const SFACE_MODEL_VERSION: &str = "v1";
const SFACE_LICENSE_NOTE: &str = "Apache-2.0 (OpenCV Zoo face_recognition_sface — MODELS.md §3)";

/// Score/NMS thresholds for [`crate::faces::detect_faces`] — OpenCV's own
/// published `FaceDetectorYN` defaults (`crate::faces`'s module doc), not
/// re-derived here.
const YUNET_SCORE_THRESHOLD: f32 = 0.6;
const YUNET_NMS_THRESHOLD: f32 = 0.3;

struct PendingFaceCrop {
    image_id: i64,
    bbox: crate::faces::FaceBox,
    detection_confidence: f32,
    crop: image::DynamicImage,
}

/// Processes up to `batch_size` images from
/// [`lenslocker_catalog::images_needing_embedding`] (reusing the exact
/// same backlog mechanism [`TaggingModel::process_backlog_batch`] uses,
/// keyed on SFace's own `model_id` instead of SigLIP's — a deliberate,
/// flagged reuse: an image with zero detected faces would otherwise never
/// get an `embeddings` row and would look permanently "unprocessed", so
/// every backlogged image gets a marker row here regardless of face
/// count — the mean of its faces' embeddings, or an all-zero vector if
/// none were found).
///
/// Two passes, both CPU-only ([`crate::load_session_cpu`] — SigLIP already
/// claims this process's one-ever DirectML session, per `load_session`'s
/// doc comment): **Pass 1** loads YuNet, detects faces in every backlogged
/// image, crops each one, and drops the session. **Pass 2** loads SFace,
/// embeds each crop, and — for every face, in detection order, re-querying
/// [`lenslocker_catalog::clustered_face_embeddings`] after each insert —
/// runs [`crate::faces::match_face`] and applies its decision. Re-querying
/// per face (not once for the whole batch) is deliberate, not an
/// oversight: two repeated faces within the *same* batch (e.g. the same
/// person across several photos in one import) must still cluster
/// together, which only works if each face sees the clusters its own
/// batch-mates already created.
pub fn process_face_backlog_batch(conn: &Connection, models_dir: &Path, batch_size: i64, thresholds: &crate::faces::FaceThresholds) -> Result<usize> {
    let model_id = lenslocker_catalog::find_or_create_model(conn, SFACE_MODEL_NAME, SFACE_MODEL_VERSION, crate::faces::EMBED_DIM as i64, SFACE_LICENSE_NOTE)?;

    let image_ids = lenslocker_catalog::images_needing_embedding(conn, model_id, batch_size)?;
    if image_ids.is_empty() {
        return Ok(0);
    }

    let mut pending_crops: Vec<PendingFaceCrop> = Vec::new();
    {
        let yunet_path = models_dir.join(ModelKind::Yunet.relative_path());
        // CPU, not DirectML: SigLIP already claims this process's one
        // DirectML-session budget (crate::load_session's doc comment).
        let mut yunet_session = crate::load_session_cpu(&yunet_path)?;

        for &image_id in &image_ids {
            let stored_path: String = conn.query_row("SELECT stored_path FROM images WHERE id = ?1", [image_id], |row| row.get(0))?;
            let stored_path = PathBuf::from(stored_path);
            let probe = lenslocker_decode::probe(&stored_path).map_err(|source| crate::MlError::Decode { path: stored_path, source })?;

            let detections = crate::faces::detect_faces(&mut yunet_session, &probe.image, YUNET_SCORE_THRESHOLD, YUNET_NMS_THRESHOLD)?;
            for detection in detections {
                let crop = crate::faces::crop_face_for_embedding(&probe.image, &detection.bbox);
                pending_crops.push(PendingFaceCrop { image_id, bbox: detection.bbox, detection_confidence: detection.score, crop });
            }
        }
    }

    {
        let sface_path = models_dir.join(ModelKind::Sface.relative_path());
        let mut sface_session = crate::load_session_cpu(&sface_path)?;

        let mut per_image_embeddings: std::collections::HashMap<i64, Vec<Vec<f32>>> = std::collections::HashMap::new();

        for crop in &pending_crops {
            let embedding = crate::faces::embed_face(&mut sface_session, &crop.crop)?;

            let new_detection = lenslocker_catalog::NewFaceDetection {
                image_id: crop.image_id,
                model_id,
                detection_confidence: crop.detection_confidence as f64,
                bbox_x: crop.bbox.x as i64,
                bbox_y: crop.bbox.y as i64,
                bbox_width: crop.bbox.width as i64,
                bbox_height: crop.bbox.height as i64,
                embedding: crate::encode_embedding(&embedding),
                dimension: crate::faces::EMBED_DIM as i64,
            };
            let detection_id = lenslocker_catalog::insert_face_detection(conn, &new_detection)?;

            let existing_members = lenslocker_catalog::clustered_face_embeddings(conn, model_id)?;
            match crate::faces::match_face(&embedding, &existing_members, thresholds) {
                crate::faces::FaceMatchDecision::AutoAttribute { cluster_id } => {
                    lenslocker_catalog::attach_face_detection_to_cluster(conn, detection_id, cluster_id)?;
                }
                crate::faces::FaceMatchDecision::ReviewQueue { suggested_person_id, similarity } => {
                    lenslocker_catalog::queue_face_match_review(conn, detection_id, suggested_person_id, similarity as f64)?;
                }
                crate::faces::FaceMatchDecision::JoinCluster { cluster_id } => {
                    lenslocker_catalog::attach_face_detection_to_cluster(conn, detection_id, cluster_id)?;
                }
                crate::faces::FaceMatchDecision::NewCluster => {
                    lenslocker_catalog::create_cluster_with_member(conn, detection_id)?;
                }
            }

            per_image_embeddings.entry(crop.image_id).or_default().push(embedding);
        }

        for &image_id in &image_ids {
            let marker = match per_image_embeddings.get(&image_id) {
                Some(embeddings) => crate::faces::centroid(embeddings),
                None => vec![0f32; crate::faces::EMBED_DIM],
            };
            lenslocker_catalog::upsert_embedding(conn, image_id, model_id, &crate::encode_embedding(&marker), crate::faces::EMBED_DIM as i64)?;
        }
    }

    Ok(image_ids.len())
}
