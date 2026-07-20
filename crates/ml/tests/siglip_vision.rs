//! Real end-to-end proof of `tagging::embed_image` against the actual
//! bundled SigLIP export — no tokenizer needed (see `tagging`'s module
//! doc). `#[ignore]`d like the ML-1 smoke test: needs a real
//! `onnxruntime.dll` and the real SigLIP `model.onnx`/`model.onnx_data`,
//! neither guaranteed present in every environment this runs in.

use image::{DynamicImage, RgbImage};
use lenslocker_ml::tagging;

fn models_root() -> std::path::PathBuf {
    lenslocker_ml::models_dir()
}

#[test]
#[ignore = "needs the bundled ONNX Runtime dylib + the real SigLIP export — see MODELS.md"]
fn embed_image_returns_a_1152_dim_vector_for_a_real_image() {
    lenslocker_ml::init(&lenslocker_ml::dylib_path()).expect("init the bundled onnxruntime dylib");

    let model_path = models_root().join(lenslocker_ml::ModelKind::Siglip.relative_path());
    let mut session = lenslocker_ml::load_session(&model_path).expect("load the real SigLIP session");

    // A trivial synthetic image is enough to prove the plumbing (shape,
    // dtype, output wiring) — semantic correctness of the embedding isn't
    // this test's job.
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(800, 600, image::Rgb([128, 64, 200])));
    let pixel_values = tagging::preprocess_image(&image);
    assert_eq!(pixel_values.len(), 3 * tagging::IMAGE_SIZE as usize * tagging::IMAGE_SIZE as usize);

    let embedding = tagging::embed_image(&mut session, &pixel_values).expect("embed_image should succeed without input_ids");

    assert_eq!(embedding.len(), tagging::EMBEDDING_DIM);
    assert!(embedding.iter().any(|&v| v != 0.0), "expected a real (non-all-zero) embedding");
}
