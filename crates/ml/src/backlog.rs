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

        let tokenizer_path = crate::siglip_tokenizer_path(models_dir);
        let text_encoder = TextEncoder::load(&tokenizer_path)?;

        let mut label_embeddings = Vec::with_capacity(STARTER_LABELS.len());
        for &label in STARTER_LABELS {
            let ids = text_encoder.tokenize(label)?;
            let embedding = tagging::text::embed_text(&mut session, &ids)?;
            label_embeddings.push((label.to_string(), embedding));
        }

        let model_id = crate::similarity::resolve_siglip_model_id(conn)?;

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

/// Where a face detection's crop thumbnail lives on disk, keyed by the
/// detection's own row id (only known post-insert, unlike the managed
/// blob/grid-thumbnail stores which key by content hash). Mirrors
/// `lenslocker_import::LibraryPaths`'s hex-sharded layout shape rather than
/// depending on that crate for it — `ml` and `import` don't depend on each
/// other today (`CLAUDE.md`'s one-direction-no-cycles rule), and adding
/// either edge for a two-line path join isn't worth it.
fn face_crop_path(library_root: &Path, detection_id: i64) -> PathBuf {
    let shard = format!("{:02x}", (detection_id.rem_euclid(256)) as u8);
    library_root.join("thumbnails").join("faces").join(shard).join(format!("{detection_id}.jpg"))
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
/// **Landmine for a future generic feature, flagged explicitly**: §11
/// describes `embeddings` in SigLIP terms only ("already fit the
/// one-embedding-per-image-per-model shape"); repurposing it here as a
/// bare processed-marker means any future feature that queries
/// `embeddings` generically across models (e.g. a cross-model similarity
/// view) must filter out or specially handle SFace-model rows for
/// faceless images — they are not a real face embedding, they're a
/// zero-vector placeholder. Not an issue for anything built so far
/// ([`lenslocker_catalog::VecMirror::build`] already filters by
/// `model_id`, so it never mixes SigLIP and SFace rows), but a real trap
/// for anyone assuming "an `embeddings` row means a real embedding" later.
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
///
/// Also writes each detection's already-cropped/resized (112x112, SFace's
/// own input size) embedding crop to `<library_root>/thumbnails/faces/...`
/// and records the path (ticket 028 decision #4) — a second use of the
/// same crop already produced for embedding, not a second decode.
pub fn process_face_backlog_batch(conn: &Connection, models_dir: &Path, library_root: &Path, batch_size: i64, thresholds: &crate::faces::FaceThresholds) -> Result<usize> {
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

            let crop_path = face_crop_path(library_root, detection_id);
            lenslocker_decode::write_jpeg_thumbnail(&crop.crop, &crop_path, crate::faces::EMBED_INPUT_SIZE)
                .map_err(|source| crate::MlError::ThumbnailWrite { path: crop_path.clone(), source })?;
            lenslocker_catalog::set_face_crop_thumbnail_path(conn, detection_id, &crop_path.to_string_lossy())?;

            let existing_members = lenslocker_catalog::clustered_face_embeddings(conn, model_id)?;
            let decision = crate::faces::match_face(&embedding, &existing_members, thresholds);
            apply_face_match_decision(conn, detection_id, decision)?;

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

/// Applies a [`crate::faces::FaceMatchDecision`] to the catalog — the
/// dispatch table between §6's three-tier decision and the CRUD it
/// implies. Factored out of [`process_face_backlog_batch`]'s inline loop
/// so it's unit-testable against a real (in-memory) catalog without
/// needing a real model to produce the decision — `match_face` itself
/// already has thorough synthetic-embedding tests (`crates/ml/src/faces.rs`);
/// this is what was previously untested: whether each of its four
/// outcomes actually reaches the right catalog call.
fn apply_face_match_decision(conn: &Connection, detection_id: i64, decision: crate::faces::FaceMatchDecision) -> Result<()> {
    match decision {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::OptionalExtension;

    use super::*;

    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        lenslocker_catalog::migrate(&mut conn).unwrap();
        conn
    }

    fn insert_test_image(conn: &Connection) -> i64 {
        let library_id: i64 = conn
            .query_row("SELECT id FROM libraries WHERE root_path = 'A:/lib'", [], |row| row.get(0))
            .optional()
            .unwrap()
            .unwrap_or_else(|| {
                conn.execute("INSERT INTO libraries (name, root_path) VALUES ('lib', 'A:/lib')", []).unwrap();
                conn.last_insert_rowid()
            });
        conn.execute(
            "INSERT INTO images (library_id, original_hash, stored_hash, stored_path, original_format, stored_format, file_size_bytes)
             VALUES (?1, randomblob(8), x'00', 'x', 'jpeg', 'jpeg', 0)",
            [library_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_test_detection(conn: &Connection, model_id: i64) -> i64 {
        let image_id = insert_test_image(conn);
        lenslocker_catalog::insert_face_detection(
            conn,
            &lenslocker_catalog::NewFaceDetection {
                image_id,
                model_id,
                detection_confidence: 0.9,
                bbox_x: 0,
                bbox_y: 0,
                bbox_width: 10,
                bbox_height: 10,
                embedding: crate::encode_embedding(&[0.0; 4]),
                dimension: 4,
            },
        )
        .unwrap()
    }

    #[test]
    fn auto_attribute_attaches_the_detection_to_the_named_cluster() {
        let conn = migrated_conn();
        let model_id = lenslocker_catalog::find_or_create_model(&conn, "sface", "v1", 4, "Apache-2.0").unwrap();
        let anchor = insert_test_detection(&conn, model_id);
        let cluster_id = lenslocker_catalog::create_cluster_with_member(&conn, anchor).unwrap();

        let detection_id = insert_test_detection(&conn, model_id);
        apply_face_match_decision(&conn, detection_id, crate::faces::FaceMatchDecision::AutoAttribute { cluster_id }).unwrap();

        let stored: i64 = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(stored, cluster_id);
    }

    #[test]
    fn review_queue_inserts_a_pending_row_without_touching_the_detections_cluster() {
        let conn = migrated_conn();
        let model_id = lenslocker_catalog::find_or_create_model(&conn, "sface", "v1", 4, "Apache-2.0").unwrap();
        let detection_id = insert_test_detection(&conn, model_id);
        conn.execute("INSERT INTO persons (name) VALUES ('Alex')", []).unwrap();
        let person_id = conn.last_insert_rowid();

        apply_face_match_decision(&conn, detection_id, crate::faces::FaceMatchDecision::ReviewQueue { suggested_person_id: person_id, similarity: 0.4 }).unwrap();

        let queued_count: i64 = conn.query_row("SELECT COUNT(*) FROM face_match_review_queue WHERE face_detection_id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(queued_count, 1);
        let cluster_id: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(cluster_id, None, "a review-queued detection must not jump straight into the named cluster");
    }

    #[test]
    fn join_cluster_attaches_the_detection_to_the_unnamed_cluster() {
        let conn = migrated_conn();
        let model_id = lenslocker_catalog::find_or_create_model(&conn, "sface", "v1", 4, "Apache-2.0").unwrap();
        let anchor = insert_test_detection(&conn, model_id);
        let cluster_id = lenslocker_catalog::create_cluster_with_member(&conn, anchor).unwrap();

        let detection_id = insert_test_detection(&conn, model_id);
        apply_face_match_decision(&conn, detection_id, crate::faces::FaceMatchDecision::JoinCluster { cluster_id }).unwrap();

        let stored: i64 = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(stored, cluster_id);
    }

    #[test]
    fn new_cluster_creates_and_attaches_a_fresh_unnamed_cluster() {
        let conn = migrated_conn();
        let model_id = lenslocker_catalog::find_or_create_model(&conn, "sface", "v1", 4, "Apache-2.0").unwrap();
        let detection_id = insert_test_detection(&conn, model_id);

        apply_face_match_decision(&conn, detection_id, crate::faces::FaceMatchDecision::NewCluster).unwrap();

        let cluster_id: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        let cluster_id = cluster_id.expect("NewCluster must attach the detection to a freshly created cluster");
        let person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(person_id, None, "a freshly created cluster must be unnamed");
    }
}
