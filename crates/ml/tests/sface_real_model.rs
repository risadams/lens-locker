//! Real proof that SFace loads and runs against the actual bundled file.
//! `#[ignore]`d like this crate's other real-model tests. Kept in its own
//! file/process — see `yunet_real_model.rs`'s doc comment for why.

use image::{DynamicImage, RgbImage};
use lenslocker_ml::faces;

#[test]
#[ignore = "needs the bundled ONNX Runtime dylib + the real SFace export — see MODELS.md"]
fn sface_embeds_a_real_crop_into_128_dims() {
    lenslocker_ml::init(&lenslocker_ml::dylib_path()).expect("init the bundled onnxruntime dylib");

    let models_dir = lenslocker_ml::models_dir();
    let model_path = models_dir.join(lenslocker_ml::ModelKind::Sface.relative_path());
    let mut session = lenslocker_ml::load_session(&model_path).expect("load the real SFace session");

    let crop = DynamicImage::ImageRgb8(RgbImage::from_pixel(112, 112, image::Rgb([180, 140, 120])));
    let embedding = faces::embed_face(&mut session, &crop).expect("embed_face should succeed on a real 112x112 crop");

    assert_eq!(embedding.len(), faces::EMBED_DIM);
    assert!(embedding.iter().any(|&v| v != 0.0), "expected a real (non-all-zero) embedding");
}
