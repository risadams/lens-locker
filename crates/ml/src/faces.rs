//! YuNet face detection + SFace face embedding (ML-SPEC.md Milestone ML-3).
//!
//! **YuNet's postprocessing is real CV code, not model-config numbers** —
//! unlike SigLIP's preprocessing (§10's flagged-but-plausible HF defaults),
//! there's no artifact to read this from at all; OpenCV's own Python
//! wrapper (`opencv_zoo/models/face_detection_yunet/yunet.py`) does no
//! decoding itself, it delegates entirely to `cv::FaceDetectorYN`'s C++
//! implementation. The anchor-center/bbox/keypoint decode formulas and
//! score computation below are transcribed from OpenCV's real source
//! (`opencv/modules/objdetect/src/face_detect.cpp`, `4.x` branch,
//! `postProcess`), fetched and read directly rather than reconstructed
//! from memory — this session already caught two cases (an ONNX field
//! number, a graph-pruning assumption) where memory alone was wrong.
//!
//! **One real, disclosed difference from OpenCV's own usage**: OpenCV's
//! `cv::dnn` backend can resize the network's *input* to roughly match
//! each source image (padding up to a multiple of 32), so it rarely
//! downscales a photo much. This crate's bundled export has a **fixed**
//! `[1,3,640,640]` input (confirmed, `MODELS.md` §2) — no such
//! flexibility — so every image is letterboxed (aspect-preserving resize
//! + pad, not a distorting squash) into 640×640 regardless of its native
//! resolution. A very high-resolution photo with small faces will detect
//! worse here than under OpenCV's own reference usage; flagged rather
//! than silently accepted as equivalent.
//!
//! **Face crop uses a simple margin-padded bounding-box crop, not SFace's
//! "real" 5-point similarity-transform alignment** (the eyes/nose/mouth
//! keypoints YuNet also outputs would support proper alignment, matching
//! ArcFace/SFace's own training-time preprocessing). A deliberate scope
//! simplification, not an oversight — flagged for a future pass if
//! embedding quality on real photos proves it matters; [`DetectedFace`]
//! still carries the keypoints so a later alignment step has what it needs.

use image::{DynamicImage, GenericImageView, imageops::FilterType};
use ort::inputs;
use ort::session::{OutputSelector, RunOptions, Session};
use ort::value::Tensor;

use crate::{MlError, Result, decode_embedding};

/// YuNet's fixed export input size (`MODELS.md` §2) — both dimensions.
pub const DETECT_INPUT_SIZE: u32 = 640;
/// The three detection-head strides YuNet's graph exposes
/// (`cls_8`/`cls_16`/`cls_32`, etc.) — order matters, matches the order
/// output names are iterated in.
const STRIDES: [i64; 3] = [8, 16, 32];

/// SFace's fixed export input size (`MODELS.md` §3).
pub const EMBED_INPUT_SIZE: u32 = 112;
/// SFace's output dimension (`MODELS.md` §3, confirmed via the real
/// export's `fc1` output shape).
pub const EMBED_DIM: usize = 128;

#[derive(Debug, Clone, Copy)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct DetectedFace {
    pub bbox: FaceBox,
    /// OpenCV's own scoring: `sqrt(cls_score * obj_score)`, both taken
    /// directly from the graph's output tensors with no extra sigmoid
    /// applied in postprocessing (per the real OpenCV source) — the
    /// export's own `cls`/`obj` heads already produce probability-range
    /// values.
    pub score: f32,
    /// 5 keypoints in the order YuNet's `kps` output packs them (right
    /// eye, left eye, nose tip, right mouth corner, left mouth corner,
    /// per the network's training convention) — not used by
    /// [`crop_face_for_embedding`] yet (see module doc), carried for a
    /// future alignment pass.
    pub keypoints: [(f32, f32); 5],
}

struct Letterbox {
    scale: f32,
}

fn letterbox_to_square(image: &DynamicImage, target: u32) -> (DynamicImage, Letterbox) {
    let (w, h) = image.dimensions();
    let scale = (target as f32 / w as f32).min(target as f32 / h as f32);
    let new_w = ((w as f32 * scale).round() as u32).max(1).min(target);
    let new_h = ((h as f32 * scale).round() as u32).max(1).min(target);

    let resized = image.resize_exact(new_w, new_h, FilterType::CatmullRom).to_rgb8();
    let mut canvas = image::RgbImage::from_pixel(target, target, image::Rgb([0, 0, 0]));
    image::imageops::overlay(&mut canvas, &resized, 0, 0);

    (DynamicImage::ImageRgb8(canvas), Letterbox { scale })
}

fn image_to_chw_f32(image: &DynamicImage) -> Vec<f32> {
    let rgb = image.to_rgb8();
    let (w, h) = rgb.dimensions();
    let plane_len = (w * h) as usize;
    let mut chw = vec![0f32; 3 * plane_len];
    for (i, pixel) in rgb.pixels().enumerate() {
        for channel in 0..3 {
            chw[channel * plane_len + i] = pixel.0[channel] as f32;
        }
    }
    chw
}

fn sigmoid_free_score(cls: f32, obj: f32) -> f32 {
    // OpenCV's own formula (face_detect.cpp postProcess): raw tensor
    // values multiplied directly, then sqrt — no extra activation.
    (cls.max(0.0) * obj.max(0.0)).sqrt()
}

/// Intersection-over-union of two axis-aligned boxes.
fn iou(a: &FaceBox, b: &FaceBox) -> f32 {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;

    let inter_x1 = a.x.max(b.x);
    let inter_y1 = a.y.max(b.y);
    let inter_x2 = ax2.min(bx2);
    let inter_y2 = ay2.min(by2);
    let inter_area = (inter_x2 - inter_x1).max(0.0) * (inter_y2 - inter_y1).max(0.0);

    let union_area = a.width * a.height + b.width * b.height - inter_area;
    if union_area <= 0.0 { 0.0 } else { inter_area / union_area }
}

/// Greedy NMS: sort by score descending, keep a box, drop any remaining
/// box whose IoU against it exceeds `nms_threshold`, repeat.
fn non_max_suppression(mut faces: Vec<DetectedFace>, nms_threshold: f32) -> Vec<DetectedFace> {
    faces.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<DetectedFace> = Vec::new();
    'candidates: for face in faces {
        for k in &kept {
            if iou(&face.bbox, &k.bbox) > nms_threshold {
                continue 'candidates;
            }
        }
        kept.push(face);
    }
    kept
}

/// Detects faces in `image`, returning boxes/keypoints in `image`'s own
/// original pixel coordinates (the letterbox transform is inverted before
/// returning). `score_threshold`/`nms_threshold` match OpenCV's own
/// `FaceDetectorYN` parameter names and typical defaults (`0.6`, `0.3`
/// respectively, per its own docs/samples) — not hardcoded here, left to
/// the caller.
pub fn detect_faces(session: &mut Session, image: &DynamicImage, score_threshold: f32, nms_threshold: f32) -> Result<Vec<DetectedFace>> {
    let (letterboxed, transform) = letterbox_to_square(image, DETECT_INPUT_SIZE);
    let chw = image_to_chw_f32(&letterboxed);
    let input = Tensor::from_array((vec![1i64, 3, DETECT_INPUT_SIZE as i64, DETECT_INPUT_SIZE as i64], chw))?;

    let output_names = ["cls_8", "cls_16", "cls_32", "obj_8", "obj_16", "obj_32", "bbox_8", "bbox_16", "bbox_32", "kps_8", "kps_16", "kps_32"];
    let mut selector = OutputSelector::no_default();
    for name in output_names {
        selector = selector.with(name);
    }
    let run_options = RunOptions::new()?.with_outputs(selector);
    let outputs = session.run_with_options(inputs!["input" => input], &run_options)?;

    let mut candidates = Vec::new();
    for (stage, &stride) in STRIDES.iter().enumerate() {
        let cols = (DETECT_INPUT_SIZE as i64) / stride;
        let rows = cols;

        let (_s, cls) = outputs.get(format!("cls_{stride}")).ok_or_else(|| missing_output(stride, "cls"))?.try_extract_tensor::<f32>()?;
        let (_s, obj) = outputs.get(format!("obj_{stride}")).ok_or_else(|| missing_output(stride, "obj"))?.try_extract_tensor::<f32>()?;
        let (_s, bbox) = outputs.get(format!("bbox_{stride}")).ok_or_else(|| missing_output(stride, "bbox"))?.try_extract_tensor::<f32>()?;
        let (_s, kps) = outputs.get(format!("kps_{stride}")).ok_or_else(|| missing_output(stride, "kps"))?.try_extract_tensor::<f32>()?;
        let _ = stage;

        for r in 0..rows {
            for c in 0..cols {
                let idx = (r * cols + c) as usize;
                let score = sigmoid_free_score(cls[idx], obj[idx]);
                if score < score_threshold {
                    continue;
                }

                let stride_f = stride as f32;
                let cx = (c as f32 + bbox[idx * 4]) * stride_f;
                let cy = (r as f32 + bbox[idx * 4 + 1]) * stride_f;
                let w = bbox[idx * 4 + 2].exp() * stride_f;
                let h = bbox[idx * 4 + 3].exp() * stride_f;
                let x = cx - w / 2.0;
                let y = cy - h / 2.0;

                let mut keypoints = [(0f32, 0f32); 5];
                for (n, kp) in keypoints.iter_mut().enumerate() {
                    let kx = (kps[idx * 10 + 2 * n] + c as f32) * stride_f;
                    let ky = (kps[idx * 10 + 2 * n + 1] + r as f32) * stride_f;
                    *kp = (kx, ky);
                }

                candidates.push(DetectedFace { bbox: FaceBox { x, y, width: w, height: h }, score, keypoints });
            }
        }
    }

    let kept = non_max_suppression(candidates, nms_threshold);

    // Invert the letterbox: top-left-aligned padding means only the scale
    // needs undoing, no offset subtraction.
    let inv_scale = 1.0 / transform.scale;
    Ok(kept
        .into_iter()
        .map(|mut face| {
            face.bbox.x *= inv_scale;
            face.bbox.y *= inv_scale;
            face.bbox.width *= inv_scale;
            face.bbox.height *= inv_scale;
            for kp in &mut face.keypoints {
                kp.0 *= inv_scale;
                kp.1 *= inv_scale;
            }
            face
        })
        .collect())
}

fn missing_output(stride: i64, kind: &str) -> MlError {
    MlError::Ort(ort::Error::new(format!("YuNet session did not return {kind}_{stride}")))
}

/// Crops `bbox` out of `image` with a 20% margin on each side (clamped to
/// image bounds) and resizes to SFace's fixed `112x112` input — a
/// simplified stand-in for real keypoint-based alignment (module doc).
pub fn crop_face_for_embedding(image: &DynamicImage, bbox: &FaceBox) -> DynamicImage {
    let (img_w, img_h) = image.dimensions();
    let margin_x = bbox.width * 0.2;
    let margin_y = bbox.height * 0.2;

    let x0 = (bbox.x - margin_x).max(0.0) as u32;
    let y0 = (bbox.y - margin_y).max(0.0) as u32;
    let x1 = ((bbox.x + bbox.width + margin_x).min(img_w as f32)) as u32;
    let y1 = ((bbox.y + bbox.height + margin_y).min(img_h as f32)) as u32;

    let crop_w = x1.saturating_sub(x0).max(1).min(img_w.saturating_sub(x0).max(1));
    let crop_h = y1.saturating_sub(y0).max(1).min(img_h.saturating_sub(y0).max(1));

    image.crop_imm(x0, y0, crop_w, crop_h).resize_exact(EMBED_INPUT_SIZE, EMBED_INPUT_SIZE, FilterType::CatmullRom)
}

/// Embeds a face crop (already `112x112`, e.g. from
/// [`crop_face_for_embedding`]) via SFace: raw `[0,255]` RGB pixels, NCHW,
/// no rescale/normalize — SFace's ONNX export (`MODELS.md` §3) takes raw
/// pixel values directly (confirmed by its lack of any input-side
/// normalization node in the graph — a legacy MXNet conversion, not a
/// modern HF-style processor pipeline like SigLIP's).
pub fn embed_face(session: &mut Session, face_crop: &DynamicImage) -> Result<Vec<f32>> {
    let chw = image_to_chw_f32(face_crop);
    let input = Tensor::from_array((vec![1i64, 3, EMBED_INPUT_SIZE as i64, EMBED_INPUT_SIZE as i64], chw))?;

    let run_options = RunOptions::new()?.with_outputs(OutputSelector::no_default().with("fc1"));
    let outputs = session.run_with_options(inputs!["data" => input], &run_options)?;

    let (_shape, data) = outputs.get("fc1").ok_or_else(|| MlError::Ort(ort::Error::new("SFace session did not return an fc1 output")))?.try_extract_tensor::<f32>()?;
    Ok(data.to_vec())
}

/// Cosine similarity between two SFace embeddings — SFace's own
/// verification metric (`ML-SPEC.md` §2/§11: `face_review_threshold`'s
/// `0.363` default is SFace's *published* cosine-similarity verification
/// threshold, not a distance).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "cosine similarity is undefined between differently-dimensioned embeddings");
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 { 0.0 } else { dot / (mag_a * mag_b) }
}

/// Mean of one or more embeddings — a cluster's representative point for
/// matching (a real per-member comparison would be more precise but
/// scales worse; a judgment call, flagged rather than assumed correct at
/// any scale — see `crates/ml/src/backlog.rs`'s face-matching doc
/// comment). Panics on an empty slice; every caller only ever calls this
/// with an already-nonempty cluster's members.
pub fn centroid(embeddings: &[Vec<f32>]) -> Vec<f32> {
    assert!(!embeddings.is_empty(), "centroid of zero embeddings is undefined");
    let dim = embeddings[0].len();
    let mut sum = vec![0f32; dim];
    for embedding in embeddings {
        for (i, v) in embedding.iter().enumerate() {
            sum[i] += v;
        }
    }
    let n = embeddings.len() as f32;
    sum.iter().map(|v| v / n).collect()
}

/// §6/§11's three tunable thresholds, cosine-similarity-scaled (higher =
/// more similar) — `app_settings`' own stored values, not hardcoded here.
#[derive(Debug, Clone, Copy)]
pub struct FaceThresholds {
    pub cluster_threshold: f32,
    pub review_threshold: f32,
    pub auto_attribute_threshold: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FaceMatchDecision {
    /// High-confidence match against a named person — attach silently,
    /// no human review (§6).
    AutoAttribute { cluster_id: i64 },
    /// Medium-confidence match against a named person — queue for human
    /// review rather than silently attaching (§6).
    ReviewQueue { suggested_person_id: i64, similarity: f32 },
    /// No plausible match to any named person, but a good enough match to
    /// an existing *unnamed* cluster — ordinary clustering (§6).
    JoinCluster { cluster_id: i64 },
    /// No match anywhere — starts a new unnamed, provisional cluster.
    NewCluster,
}

/// Implements §6's three-tier match: compare `new_embedding` against
/// every **named** person's cluster centroid first (auto-attribute /
/// review queue / no match, in that strictness order), falling through to
/// ordinary clustering against **unnamed** clusters only when no named
/// person clears even the review floor — a low-confidence match to a
/// named person's cluster must never silently fall back to joining that
/// same cluster anonymously just because it also happens to be the
/// nearest unnamed-shaped grouping; the review gate is the only path
/// into a named identity below the auto-attribute bar.
pub fn match_face(new_embedding: &[f32], members: &[lenslocker_catalog::ClusterMember], thresholds: &FaceThresholds) -> FaceMatchDecision {
    let mut clusters: std::collections::BTreeMap<i64, (Option<i64>, Vec<Vec<f32>>)> = std::collections::BTreeMap::new();
    for member in members {
        let embedding = decode_embedding(&member.embedding);
        clusters.entry(member.cluster_id).or_insert_with(|| (member.person_id, Vec::new())).1.push(embedding);
    }
    let centroids: Vec<(i64, Option<i64>, Vec<f32>)> =
        clusters.into_iter().map(|(cluster_id, (person_id, embeddings))| (cluster_id, person_id, centroid(&embeddings))).collect();

    let best_named = centroids
        .iter()
        .filter_map(|(cluster_id, person_id, centroid)| person_id.map(|p| (*cluster_id, p, cosine_similarity(new_embedding, centroid))))
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    if let Some((cluster_id, person_id, similarity)) = best_named {
        if similarity >= thresholds.auto_attribute_threshold {
            return FaceMatchDecision::AutoAttribute { cluster_id };
        }
        if similarity >= thresholds.review_threshold {
            return FaceMatchDecision::ReviewQueue { suggested_person_id: person_id, similarity };
        }
    }

    let best_unnamed = centroids
        .iter()
        .filter(|(_, person_id, _)| person_id.is_none())
        .map(|(cluster_id, _, centroid)| (*cluster_id, cosine_similarity(new_embedding, centroid)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    match best_unnamed {
        Some((cluster_id, similarity)) if similarity >= thresholds.cluster_threshold => FaceMatchDecision::JoinCluster { cluster_id },
        _ => FaceMatchDecision::NewCluster,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> FaceThresholds {
        // Matches migration 0003's real defaults.
        FaceThresholds { cluster_threshold: 0.30, review_threshold: 0.363, auto_attribute_threshold: 0.50 }
    }

    fn unit_vector(index: usize, dim: usize) -> Vec<f32> {
        let mut v = vec![0f32; dim];
        v[index] = 1.0;
        v
    }

    fn member(face_detection_id: i64, cluster_id: i64, person_id: Option<i64>, embedding: &[f32]) -> lenslocker_catalog::ClusterMember {
        lenslocker_catalog::ClusterMember { face_detection_id, cluster_id, person_id, embedding: crate::encode_embedding(embedding) }
    }

    #[test]
    fn encode_decode_embedding_round_trips() {
        let values = vec![1.5f32, -2.25, 0.0, 3.0];
        assert_eq!(decode_embedding(&crate::encode_embedding(&values)), values);
    }

    #[test]
    fn centroid_of_a_single_embedding_is_itself() {
        let v = vec![1.0, 2.0, 3.0];
        assert_eq!(centroid(&[v.clone()]), v);
    }

    #[test]
    fn centroid_averages_multiple_embeddings() {
        let a = vec![0.0, 0.0];
        let b = vec![2.0, 4.0];
        assert_eq!(centroid(&[a, b]), vec![1.0, 2.0]);
    }

    #[test]
    fn match_face_auto_attributes_a_near_identical_match_to_a_named_person() {
        let dim = 4;
        let members = vec![member(1, 100, Some(7), &unit_vector(0, dim))];
        let decision = match_face(&unit_vector(0, dim), &members, &thresholds());
        assert_eq!(decision, FaceMatchDecision::AutoAttribute { cluster_id: 100 });
    }

    #[test]
    fn match_face_queues_a_medium_confidence_named_match_for_review() {
        // named=[1,0], probe=[cos(theta), sin(theta)] with theta chosen so
        // cosine similarity lands at ~0.42 — between the review floor
        // (0.363) and the auto-attribute bar (0.50).
        let named = vec![1.0f32, 0.0];
        let probe = vec![0.42f32, (1.0f32 - 0.42f32 * 0.42f32).sqrt()];
        let similarity = cosine_similarity(&probe, &named);
        assert!(
            similarity >= thresholds().review_threshold && similarity < thresholds().auto_attribute_threshold,
            "test fixture's own similarity ({similarity}) must land in the review band for this test to mean anything"
        );

        let members = vec![member(1, 100, Some(7), &named)];
        let decision = match_face(&probe, &members, &thresholds());
        assert_eq!(decision, FaceMatchDecision::ReviewQueue { suggested_person_id: 7, similarity });
    }

    #[test]
    fn match_face_never_falls_back_to_silently_joining_a_named_clusters_low_similarity_match() {
        // Below even the review floor against the only (named) cluster —
        // must NOT silently join it just because it's the closest thing
        // around; must fall through to NewCluster (no unnamed cluster
        // exists to join either).
        let named = vec![1.0f32, 0.0, 0.0, 0.0];
        let probe = vec![0.0f32, 1.0, 0.0, 0.0]; // orthogonal: similarity 0.0
        let members = vec![member(1, 100, Some(7), &named)];
        let decision = match_face(&probe, &members, &thresholds());
        assert_eq!(decision, FaceMatchDecision::NewCluster);
    }

    #[test]
    fn match_face_joins_a_good_enough_unnamed_cluster_when_no_named_match_exists() {
        let dim = 4;
        let members = vec![member(1, 200, None, &unit_vector(0, dim))];
        let decision = match_face(&unit_vector(0, dim), &members, &thresholds());
        assert_eq!(decision, FaceMatchDecision::JoinCluster { cluster_id: 200 });
    }

    #[test]
    fn match_face_starts_a_new_cluster_when_nothing_is_close_enough() {
        let dim = 4;
        let members = vec![member(1, 200, None, &unit_vector(0, dim))];
        let decision = match_face(&unit_vector(1, dim), &members, &thresholds());
        assert_eq!(decision, FaceMatchDecision::NewCluster);
    }

    #[test]
    fn match_face_with_no_existing_members_always_starts_a_new_cluster() {
        let decision = match_face(&unit_vector(0, 4), &[], &thresholds());
        assert_eq!(decision, FaceMatchDecision::NewCluster);
    }

    #[test]
    fn iou_of_identical_boxes_is_one() {
        let a = FaceBox { x: 10.0, y: 10.0, width: 20.0, height: 20.0 };
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_of_disjoint_boxes_is_zero() {
        let a = FaceBox { x: 0.0, y: 0.0, width: 10.0, height: 10.0 };
        let b = FaceBox { x: 100.0, y: 100.0, width: 10.0, height: 10.0 };
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn iou_of_half_overlapping_boxes_is_one_third() {
        let a = FaceBox { x: 0.0, y: 0.0, width: 10.0, height: 10.0 };
        let b = FaceBox { x: 5.0, y: 0.0, width: 10.0, height: 10.0 };
        // intersection = 5x10 = 50, union = 100+100-50 = 150, iou = 1/3
        assert!((iou(&a, &b) - (1.0 / 3.0)).abs() < 1e-6);
    }

    fn face_at(x: f32, score: f32) -> DetectedFace {
        DetectedFace { bbox: FaceBox { x, y: 0.0, width: 10.0, height: 10.0 }, score, keypoints: [(0.0, 0.0); 5] }
    }

    #[test]
    fn nms_keeps_the_higher_scoring_box_among_overlapping_candidates() {
        let faces = vec![face_at(0.0, 0.9), face_at(1.0, 0.95)];
        let kept = non_max_suppression(faces, 0.3);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].score, 0.95);
    }

    #[test]
    fn nms_keeps_both_boxes_when_they_dont_overlap_enough() {
        let faces = vec![face_at(0.0, 0.9), face_at(1000.0, 0.95)];
        let kept = non_max_suppression(faces, 0.3);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn cosine_similarity_of_identical_vectors_is_one() {
        let mut v = vec![0f32; EMBED_DIM];
        v[0] = 1.0;
        v[1] = 2.0;
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_of_orthogonal_vectors_is_zero() {
        let mut a = vec![0f32; EMBED_DIM];
        let mut b = vec![0f32; EMBED_DIM];
        a[0] = 1.0;
        b[1] = 1.0;
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn letterbox_preserves_aspect_ratio_for_a_wide_image() {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 100, image::Rgb([1, 2, 3])));
        let (letterboxed, transform) = letterbox_to_square(&image, 640);
        assert_eq!(letterboxed.dimensions(), (640, 640));
        // scale should be limited by width (400 -> 640, factor 1.6), not height
        assert!((transform.scale - 1.6).abs() < 1e-3);
    }

    /// A synthetic single-stride, single-position "network output" with a
    /// known expected decode, verifying the decode formulas independent
    /// of the real model (OpenCV's own postProcess formulas, transcribed
    /// in this module's doc comment).
    #[test]
    fn bbox_decode_formula_matches_a_hand_computed_example() {
        // stride=8, grid cell (c=2, r=3): anchor center = ((2,3)+bbox_offset)*8.
        // bbox_offset = (0,0) means the raw center sits exactly on the
        // anchor: cx = 2*8 = 16, cy = 3*8 = 24. bbox[2..4] = ln(2) means
        // w = h = exp(ln(2)) * 8 = 16.
        let c = 2f32;
        let r = 3f32;
        let stride = 8f32;
        let bbox_offset = [0.0f32, 0.0, 2f32.ln(), 2f32.ln()];

        let cx = (c + bbox_offset[0]) * stride;
        let cy = (r + bbox_offset[1]) * stride;
        let w = bbox_offset[2].exp() * stride;
        let h = bbox_offset[3].exp() * stride;

        assert!((cx - 16.0).abs() < 1e-4);
        assert!((cy - 24.0).abs() < 1e-4);
        assert!((w - 16.0).abs() < 1e-4);
        assert!((h - 16.0).abs() < 1e-4);
    }
}
