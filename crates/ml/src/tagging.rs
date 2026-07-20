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

/// SigLIP's own sigmoid-loss scale/bias — the model's two learned scalar
/// parameters, **read directly from the real export's initializers**
/// (`onnx::Exp_7146` = this raw value, exponentiated at use per SigLIP's
/// own convention of storing `log(scale)`; `onnx::Add_7147` =
/// [`LOGIT_BIAS`] directly) via a throwaway protobuf inspector, not
/// guessed or taken from a paper. `logits_per_image`/`logits_per_text`
/// aren't graph outputs reachable from stored embeddings — they're
/// computed *inside* the graph from both towers at once — so replicating
/// the formula here is what makes §4's "adding a custom label is a cheap
/// backfill... not a re-embed" possible: score a new label against every
/// already-stored `image_embeds` row without re-running the vision tower.
pub const LOGIT_SCALE_RAW: f32 = 4.7214665;
pub const LOGIT_BIAS: f32 = -16.546421;

/// Zero-shot label-match probability for one image/text embedding pair —
/// `sigmoid((image_embeds · text_embeds) * exp(LOGIT_SCALE_RAW) + LOGIT_BIAS)`,
/// SigLIP's own scoring formula (§2/§4). Uses the raw dot product, not a
/// separately L2-normalized one: `crates/ml/tests/siglip_scoring_formula.rs`
/// confirmed against the real model (to ~1e-6) that `embed_image`/
/// `text::embed_text`'s outputs are already unit-normalized by the graph
/// itself, so re-normalizing here would be redundant work, not extra
/// correctness.
pub fn zero_shot_probability(image_embeds: &[f32], text_embeds: &[f32]) -> f32 {
    debug_assert_eq!(image_embeds.len(), EMBEDDING_DIM);
    debug_assert_eq!(text_embeds.len(), EMBEDDING_DIM);
    let dot: f32 = image_embeds.iter().zip(text_embeds).map(|(a, b)| a * b).sum();
    let logit = dot * LOGIT_SCALE_RAW.exp() + LOGIT_BIAS;
    1.0 / (1.0 + (-logit).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_shot_probability_of_identical_unit_vectors_is_near_one() {
        // A perfect match (image_embeds == text_embeds, both unit-length)
        // is the largest possible dot product (1.0) — sanity-checks the
        // formula's shape independent of the real model, before the
        // ONNX-backed test (siglip_scoring_formula.rs) verifies the exact
        // constants against real output.
        let mut v = vec![0f32; EMBEDDING_DIM];
        v[0] = 1.0;
        let probability = zero_shot_probability(&v, &v);
        assert!(probability > 0.999, "expected a near-1.0 probability for a perfect match, got {probability}");
    }

    #[test]
    fn zero_shot_probability_of_orthogonal_vectors_is_low() {
        let mut a = vec![0f32; EMBEDDING_DIM];
        let mut b = vec![0f32; EMBEDDING_DIM];
        a[0] = 1.0;
        b[1] = 1.0;
        let probability = zero_shot_probability(&a, &b);
        assert!(probability < 0.5, "expected a low probability for orthogonal embeddings, got {probability}");
    }
}

/// Text-side encoding: tokenizes a label or search query to the
/// fixed-length `input_ids` sequence the text tower needs, then runs it
/// through the same session `embed_image` uses (a different output
/// selection, `text_embeds` instead of `image_embeds` — see that
/// function's doc comment for why a placeholder `pixel_values` is still
/// required, by the same unconditional-graph-dependency reasoning).
pub mod text {
    use std::path::Path;

    use ort::inputs;
    use ort::session::{OutputSelector, RunOptions, Session};
    use ort::value::Tensor;
    use tokenizers::Tokenizer;

    use super::{IMAGE_SIZE, TEXT_MAX_LEN};
    use crate::{MlError, Result};

    /// `siglip-so400m-onnx/tokenizer.json`'s `added_tokens[0]`: `<pad>`,
    /// id `0` — confirmed by reading the real bundled file (`MODELS.md` §5),
    /// not the SentencePiece-library-wide default (which is usually `<unk>`
    /// at id 0; this export's isn't).
    const PAD_TOKEN_ID: i64 = 0;

    pub struct TextEncoder {
        tokenizer: Tokenizer,
    }

    impl TextEncoder {
        /// Loads the real tokenizer from `tokenizer_path` (the sibling
        /// `tokenizer.json` next to SigLIP's `model.onnx` —
        /// `ModelKind::Siglip.relative_path()`'s parent directory).
        pub fn load(tokenizer_path: &Path) -> Result<Self> {
            let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| MlError::Tokenizer(e.to_string()))?;
            Ok(Self { tokenizer })
        }

        /// Tokenizes `text`, applying the tokenizer's own post-processor
        /// (appends `</s>`, per the bundled `tokenizer.json`'s
        /// `TemplateProcessing` config — `add_special_tokens: true`), then
        /// pads with [`PAD_TOKEN_ID`] or truncates to exactly
        /// [`TEXT_MAX_LEN`], matching the export's fixed-length,
        /// no-`attention_mask` input shape.
        pub fn tokenize(&self, text: &str) -> Result<Vec<i64>> {
            let encoding = self.tokenizer.encode(text, true).map_err(|e| MlError::Tokenizer(e.to_string()))?;
            let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
            ids.truncate(TEXT_MAX_LEN);
            ids.resize(TEXT_MAX_LEN, PAD_TOKEN_ID);
            Ok(ids)
        }
    }

    /// Runs the text tower for one already-tokenized sequence:
    /// `text_embeds` (`EMBEDDING_DIM`-long) out, via [`OutputSelector`]
    /// restricted to just that output — a placeholder all-zero
    /// `pixel_values` satisfies the vision-tower half of the graph the
    /// same way [`super::embed_image`]'s placeholder `input_ids` does.
    pub fn embed_text(session: &mut Session, input_ids: &[i64]) -> Result<Vec<f32>> {
        debug_assert_eq!(input_ids.len(), TEXT_MAX_LEN);
        let input_ids_tensor = Tensor::from_array((vec![1i64, TEXT_MAX_LEN as i64], input_ids.to_vec()))?;
        let placeholder_pixels = Tensor::from_array((
            vec![1i64, 3, IMAGE_SIZE as i64, IMAGE_SIZE as i64],
            vec![0f32; 3 * IMAGE_SIZE as usize * IMAGE_SIZE as usize]
        ))?;

        let run_options = RunOptions::new()?.with_outputs(OutputSelector::no_default().with("text_embeds"));
        let outputs = session.run_with_options(
            inputs!["input_ids" => input_ids_tensor, "pixel_values" => placeholder_pixels],
            &run_options
        )?;

        let (_shape, data) = outputs
            .get("text_embeds")
            .ok_or_else(|| MlError::Ort(ort::Error::new("SigLIP session did not return a text_embeds output")))?
            .try_extract_tensor::<f32>()?;

        Ok(data.to_vec())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tokenizer_path() -> std::path::PathBuf {
            crate::models_dir().join("siglip-so400m-onnx").join("tokenizer.json")
        }

        /// Doesn't need the ONNX Runtime dylib at all — pure tokenization,
        /// so unlike this module's ONNX-backed tests, this one isn't
        /// `#[ignore]`d; it just skips gracefully if the tokenizer file
        /// isn't present in this environment.
        #[test]
        fn tokenize_pads_a_short_label_to_the_fixed_length() {
            let path = tokenizer_path();
            if !path.is_file() {
                eprintln!("skipping: no tokenizer.json at {}", path.display());
                return;
            }
            let encoder = TextEncoder::load(&path).unwrap();
            let ids = encoder.tokenize("beach").unwrap();
            assert_eq!(ids.len(), TEXT_MAX_LEN);
            assert!(ids.contains(&1), "expected the </s> token (id 1) somewhere in a short, unpadded-length sequence");
            assert_eq!(*ids.last().unwrap(), PAD_TOKEN_ID, "expected trailing padding for a short label");
        }
    }
}
