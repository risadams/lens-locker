//! SigLIP `so400m` tagging pipeline (ML-SPEC.md Milestone ML-2).
//!
//! Image-side only for now: text encoding (label/query embedding) needs a
//! tokenizer that isn't bundled yet (`MODELS.md` §5) — see this module's
//! `text` submodule stub. The image side is independently useful and
//! independently testable without a real tokenizer: an
//! [`ort::session::RunOptions::with_outputs`] selection restricted to
//! `image_embeds` means the *content* of `input_ids` never affects
//! anything this module returns — a placeholder tensor satisfies the
//! graph structurally (see [`embed_image`]'s doc comment for why one is
//! still required at all, confirmed empirically rather than assumed).
//!
//! All constants below are confirmed against the real export's
//! `config.json`/graph declaration (`MODELS.md` §4) — see `crates/ml`'s
//! throwaway inspector notes there — except [`preprocess_image`]'s exact
//! resize filter and normalization constants, which are SigLIP's
//! well-documented Hugging Face processor defaults, **not** locally
//! confirmed against a `preprocessor_config.json` (the export doesn't
//! include one). Flagged rather than silently assumed, matching this
//! repo's convention for numbers of this shape (`ML-SPEC.md` §10).

use image::DynamicImage;
use image::imageops::FilterType;
use ort::session::{OutputSelector, RunOptions, Session};
use ort::inputs;
use ort::value::Tensor;

use crate::{MlError, Result};

/// `config.json`'s `vision_config.image_size` — both dimensions, this
/// export has no aspect-preserving crop step (a direct resize, per
/// SigLIP's own default image processor).
pub const IMAGE_SIZE: u32 = 384;

/// `config.json`'s `text_config.projection_size` /
/// `vision_config.hidden_size` — both towers project to this dimension.
pub const EMBEDDING_DIM: usize = 1152;

/// `config.json`'s `text_config.max_position_embeddings`. The export has
/// no `attention_mask` input, so every tokenized sequence must be padded
/// (or truncated) to exactly this length before being sent as `input_ids`.
pub const TEXT_MAX_LEN: usize = 64;

/// Resizes/normalizes `image` into the flat NCHW `f32` buffer SigLIP's
/// `pixel_values` input expects: `[1, 3, IMAGE_SIZE, IMAGE_SIZE]`,
/// rescaled to `[0, 1]` then normalized with mean/std `0.5` per channel
/// (SigLIP's own convention — not ImageNet's stats, unlike CLIP).
///
/// **Unconfirmed against a real `preprocessor_config.json`** (not present
/// in the export — `MODELS.md` §4): the resize filter
/// ([`FilterType::CatmullRom`], `image`'s closest match to the bicubic
/// resampling Hugging Face's default `SiglipImageProcessor` uses) and the
/// 0.5/0.5 rescale+normalize constants are SigLIP's well-published
/// defaults for this model family, not read from a local artifact.
pub fn preprocess_image(image: &DynamicImage) -> Vec<f32> {
    let resized = image.resize_exact(IMAGE_SIZE, IMAGE_SIZE, FilterType::CatmullRom);
    let rgb = resized.to_rgb8();

    let mut chw = vec![0f32; 3 * IMAGE_SIZE as usize * IMAGE_SIZE as usize];
    let plane_len = (IMAGE_SIZE * IMAGE_SIZE) as usize;
    for (i, pixel) in rgb.pixels().enumerate() {
        for channel in 0..3 {
            let value = pixel.0[channel] as f32 / 255.0;
            chw[channel * plane_len + i] = (value - 0.5) / 0.5;
        }
    }
    chw
}

/// Runs the vision tower: `pixel_values` in, `image_embeds`
/// (`EMBEDDING_DIM`-long) out, via [`OutputSelector`] restricted to just
/// `image_embeds`.
///
/// **Still needs a placeholder `input_ids`, despite the output
/// selection** — tried assuming ONNX Runtime would prune the text-tower
/// subgraph away entirely and it does not: this export's trace has an
/// unconditional `/text_model/Reshape` on `input_ids` that the graph
/// hasn't separated from the vision path, so omitting it errors
/// (`Missing Input: input_ids`), confirmed empirically against the real
/// model rather than assumed. A zero-filled `[1, TEXT_MAX_LEN]` tensor is
/// supplied to satisfy that node structurally; since only `image_embeds`
/// is selected as an output, its *content* never reaches anything this
/// function returns, so it doesn't need to be a real tokenization.
pub fn embed_image(session: &mut Session, pixel_values: &[f32]) -> Result<Vec<f32>> {
    let pixel_tensor = Tensor::from_array((vec![1i64, 3, IMAGE_SIZE as i64, IMAGE_SIZE as i64], pixel_values.to_vec()))?;
    let placeholder_input_ids = Tensor::from_array((vec![1i64, TEXT_MAX_LEN as i64], vec![0i64; TEXT_MAX_LEN]))?;

    let run_options = RunOptions::new()?.with_outputs(OutputSelector::no_default().with("image_embeds"));
    let outputs = session.run_with_options(
        inputs!["pixel_values" => pixel_tensor, "input_ids" => placeholder_input_ids],
        &run_options
    )?;

    let (_shape, data) = outputs
        .get("image_embeds")
        .ok_or_else(|| MlError::Ort(ort::Error::new("SigLIP session did not return an image_embeds output")))?
        .try_extract_tensor::<f32>()?;

    Ok(data.to_vec())
}

/// Text-side encoding — blocked on a tokenizer not yet bundled
/// (`MODELS.md` §5). Intentionally unimplemented rather than guessed at:
/// zero-shot label scoring and text-to-image search (§8) both need this,
/// but a wrong tokenization silently produces wrong `input_ids`, not an
/// error — not a risk worth taking to unblock a stub.
pub mod text {
    /// Will tokenize `text` to a length-[`super::TEXT_MAX_LEN`] `input_ids`
    /// sequence once a real tokenizer is bundled. Panics unconditionally for
    /// now so a caller can't silently ship wrong tokenization.
    pub fn tokenize(_text: &str) -> Vec<i64> {
        unimplemented!("needs a real tokenizer — see MODELS.md §5")
    }
}
