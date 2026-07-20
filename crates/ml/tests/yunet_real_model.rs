//! Real proof that YuNet loads and runs against the actual bundled file —
//! no test face available in this environment (no real photo of a face,
//! no network to fetch a licensed one), so this proves the plumbing
//! (session creation, tensor shapes, decode-without-crashing), not real
//! detection accuracy. `#[ignore]`d like this crate's other real-model
//! tests. Kept in its own file/process, not alongside the SFace test —
//! per the ML-1 addendum, a second DirectML session in one process
//! crashes even after the first was dropped, and cargo runs every #[test]
//! in one file's binary in the same process.

use image::{DynamicImage, RgbImage};
use lenslocker_ml::faces;

#[test]
#[ignore = "needs the bundled ONNX Runtime dylib + the real YuNet export — see MODELS.md"]
fn yunet_runs_on_a_real_image_without_crashing() {
    lenslocker_ml::init(&lenslocker_ml::dylib_path()).expect("init the bundled onnxruntime dylib");

    let models_dir = lenslocker_ml::models_dir();
    let model_path = models_dir.join(lenslocker_ml::ModelKind::Yunet.relative_path());
    let mut session = lenslocker_ml::load_session(&model_path).expect("load the real YuNet session");

    // A non-square photo, so the letterbox path (not just a square
    // resize) actually gets exercised against the real model.
    let image = DynamicImage::ImageRgb8(RgbImage::from_fn(1200, 800, |x, y| {
        image::Rgb([((x * 3) % 256) as u8, ((y * 5) % 256) as u8, 128])
    }));

    let detections = faces::detect_faces(&mut session, &image, 0.6, 0.3).expect("detect_faces should run without error on a real image");
    // No real face in this synthetic noise pattern — the assertion is
    // "ran without crashing / errors, produced a well-formed (possibly
    // empty) result", not "found N faces."
    for face in &detections {
        assert!(face.score >= 0.0 && face.score <= 1.0, "score {} out of expected range", face.score);
        assert!(face.bbox.width > 0.0 && face.bbox.height > 0.0);
    }
    eprintln!("YuNet ran on a real image; {} raw detections above threshold 0.6", detections.len());
}
