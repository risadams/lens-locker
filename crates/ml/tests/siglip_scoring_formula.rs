//! Throwaway-in-spirit but committed verification: derives the exact
//! zero-shot scoring formula by comparing a hand-computed score against
//! the real model's own `logits_per_image` output for the same real
//! image+label pair, run in one session call with both real inputs (no
//! placeholders needed here — this is the one place both towers' real
//! values are available simultaneously). `#[ignore]`d like this crate's
//! other real-model tests.

use image::{DynamicImage, RgbImage};
use lenslocker_ml::tagging::{self, text::TextEncoder};
use ort::inputs;
use ort::session::{OutputSelector, RunOptions};
use ort::value::Tensor;

#[test]
#[ignore = "needs the bundled ONNX Runtime dylib + the real SigLIP export + tokenizer.json"]
fn hand_computed_sigmoid_score_matches_the_models_own_logits() {
    lenslocker_ml::init(&lenslocker_ml::dylib_path()).expect("init the bundled onnxruntime dylib");

    let model_path = lenslocker_ml::models_dir().join(lenslocker_ml::ModelKind::Siglip.relative_path());
    let mut session = lenslocker_ml::load_session(&model_path).expect("load the real SigLIP session");

    let tokenizer_path = lenslocker_ml::models_dir().join("siglip-so400m-onnx").join("tokenizer.json");
    let encoder = TextEncoder::load(&tokenizer_path).expect("load the real tokenizer");
    let input_ids = encoder.tokenize("beach").unwrap();

    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(800, 600, image::Rgb([200, 180, 120])));
    let pixel_values = tagging::preprocess_image(&image);

    let pixel_tensor = Tensor::from_array((
        vec![1i64, 3, tagging::IMAGE_SIZE as i64, tagging::IMAGE_SIZE as i64],
        pixel_values.clone()
    ))
    .unwrap();
    let ids_tensor = Tensor::from_array((vec![1i64, tagging::TEXT_MAX_LEN as i64], input_ids.clone())).unwrap();

    // Real values for both towers at once — request every output so we
    // can compare our own formula's result against the model's.
    let run_options = RunOptions::new()
        .unwrap()
        .with_outputs(OutputSelector::no_default().with("image_embeds").with("text_embeds").with("logits_per_image"));
    let outputs = session
        .run_with_options(inputs!["pixel_values" => pixel_tensor, "input_ids" => ids_tensor], &run_options)
        .unwrap();

    let (_s, image_embeds) = outputs.get("image_embeds").unwrap().try_extract_tensor::<f32>().unwrap();
    let (_s, text_embeds) = outputs.get("text_embeds").unwrap().try_extract_tensor::<f32>().unwrap();
    let (_s, logits_per_image) = outputs.get("logits_per_image").unwrap().try_extract_tensor::<f32>().unwrap();
    let model_logit = logits_per_image[0];
    let model_probability = 1.0 / (1.0 + (-model_logit).exp());

    let our_probability = tagging::zero_shot_probability(image_embeds, text_embeds);

    eprintln!("model's own sigmoid(logits_per_image) = {model_probability}");
    eprintln!("tagging::zero_shot_probability         = {our_probability}");

    assert!(
        (our_probability - model_probability).abs() < 1e-4,
        "zero_shot_probability didn't match the model's own logits_per_image-derived probability: ours={our_probability}, model's={model_probability}"
    );
}
