# ML model & runtime acquisition

`workplan/ML-SPEC.md` Milestone ML-1 wires the `ort`/DirectML plumbing
(`crates/ml`) and the NSIS installer's resource bundling
(`src-tauri/tauri.conf.json`) against four binary files this repo does not
and will not vendor via automated fetch — zero network access, ever, is
this project's first non-negotiable constraint (`CLAUDE.md`), and it
applies to the build/dev toolchain's own discipline around provenance, not
just the shipped app's runtime behavior. Drop each file into
`src-tauri/models/` under the exact relative path below (matching
`ModelKind::relative_path()` in `crates/ml/src/lib.rs`, and each upstream
source's own export layout); no code change is needed once it's there.

## 1. ONNX Runtime, DirectML build — `onnxruntime.dll`

The DirectML execution provider needs the DirectML-flavored build of
`onnxruntime.dll`, not the plain CPU one. Official Microsoft builds:

- GitHub releases: `https://github.com/microsoft/onnxruntime/releases` —
  look for `onnxruntime-win-x64-directml-<version>.zip`, extract
  `onnxruntime.dll` from it.
- Alternatively, the `Microsoft.ML.OnnxRuntime.DirectML` NuGet package
  contains the same DLL under `runtimes/win-x64/native/`.

`workplan/ML-SPEC.md` §10 flags this DLL's real size as unconfirmed pending
this exact download — record it once known.

## 2. YuNet (face detection) — `face_detection_yunet/face_detection_yunet_2023mar.onnx`

OpenCV Zoo, MIT license (code + weights):
`https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet`
— the file is `face_detection_yunet_2023mar.onnx` in that directory; keep it
under a `face_detection_yunet/` subdirectory, matching the upstream repo's
own layout.

**Confirmed real I/O** (same cheap protobuf-only inspection as SigLIP's,
below): one input, `input` `FLOAT[1,3,640,640]` — fixed 640×640, not
dynamic. Three-scale YOLO-style detection heads out (strides 8/16/32):
`cls_{8,16,32}` (objectness-adjacent class score), `obj_{8,16,32}`, `bbox_{8,16,32}`
(4 values), `kps_{8,16,32}` (10 values — 5 facial landmark x/y pairs) — real
anchor-decoding logic to turn these into face boxes is ML-3 pipeline work,
not yet written.

## 3. SFace (face embedding) — `face_recognition_sface/face_recognition_sface_2021dec.onnx`

OpenCV Zoo, Apache-2.0 license (code + weights):
`https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface`
— the file is `face_recognition_sface_2021dec.onnx` in that directory; same
subdirectory convention as YuNet above.

**Confirmed real I/O**: the real input to supply at runtime is `data`
`FLOAT[1,3,112,112]` (a legacy MXNet→ONNX conversion quirk declares every
conv weight as a graph input too, alongside its matching initializer —
`ort`/ONNX Runtime treats an input with a matching initializer as already
satisfied, so only `data` needs a real value). Output `fc1`
`FLOAT[1,128]` — confirms §2's 128-dim embedding.

## 4. SigLIP `so400m` (tagging embeddings) — `siglip-so400m-onnx/model.onnx`

**Self-converted, not a third-party convenience export** — `workplan/ML-SPEC.md`
§2's explicit call, to keep the redistribution provenance chain directly
traceable to the rights-holder (Google), not routed through someone else's
unverified re-export.

```
pip install optimum[exporters] onnx
optimum-cli export onnx --model google/siglip-so400m-patch14-384 ./siglip-export/
```

Apache-2.0, independently confirmed via the Hugging Face API's `license`
field for this specific checkpoint (ticket 024) — do not substitute
SigLIP2 or a different checkpoint without re-confirming that field.

**Export shape — confirmed against a real export, not assumed:**
`optimum-cli` produces a single combined `model.onnx` (`architectures:
["SiglipModel"]` in the export's `config.json`) exposing both the vision
and text towers in one graph, plus a sibling `model.onnx_data` file
holding the external weights (ONNX's standard external-data convention for
models too large to fit initializers inline — this one is several GB).
Both files must sit together in `siglip-so400m-onnx/`; `model.onnx`'s
internal reference to `model.onnx_data` is a relative filename, so ONNX
Runtime resolves it automatically as long as they're co-located — no
extra wiring needed. Confirmed dimensions: 1152 (`hidden_size`/
`projection_size` in `config.json`, for both towers) — not the more common
768 some other SigLIP variants use; don't assume 768 when ML-2's tagging
pipeline code gets written against this.

**Confirmed real I/O** (inspected directly from the actual export's
`model.onnx` graph declaration — protobuf `ModelProto.graph.input`/`.output`,
not the external-data weights, so this was cheap and needed no session
load): `input_ids` `INT64[text_batch_size, sequence_length]`, `pixel_values`
`FLOAT[image_batch_size, num_channels, height, width]` in; `logits_per_image`
`FLOAT[image_batch, text_batch]`, `logits_per_text`, `text_embeds`
`FLOAT[text_batch, 1152]`, `image_embeds` `FLOAT[image_batch, 1152]` out.
No `attention_mask` input — this export expects fixed-length-padded token
sequences (SigLIP's own convention; `config.json`'s
`max_position_embeddings` is 64). `logits_per_image` is the model's own
sigmoid-loss logits (SigLIP's zero-shot recipe: `sigmoid(logits_per_image)`
per label, no separate cosine-similarity+threshold math needed) — ML-2's
scoring code should request that output directly rather than computing
similarity from the embeddings by hand.

## 5. SigLIP tokenizer files — **not yet supplied, blocks ML-2's text side**

`input_ids` above needs real tokenization; `optimum-cli`'s export directory
only contains `config.json`/`model.onnx`/`model.onnx_data` — the tokenizer
artifacts aren't part of the ONNX export and haven't been dropped in yet.
SigLIP `so400m` uses a SentencePiece-based (unigram) tokenizer, vocab size
32000 (`config.json`'s `text_config.vocab_size`) — download these files
directly from the same `google/siglip-so400m-patch14-384` Hugging Face
repo (no `optimum`/ONNX conversion involved, just the tokenizer's own
files) into `siglip-so400m-onnx/` alongside the `.onnx` files:

- `tokenizer.json` (preferred — lets `crates/ml` use the Rust `tokenizers`
  crate's `Tokenizer::from_file` directly, no separate SentencePiece FFI
  dependency)
- if `tokenizer.json` isn't present in that repo, `spiece.model` +
  `tokenizer_config.json` + `special_tokens_map.json` as a fallback set

Confirm the tokenizer's max length matches `config.json`'s
`max_position_embeddings` (64) — pad/truncate every label and search query
to exactly that length before feeding `input_ids`, matching the no-
`attention_mask` export shape above.

## Verifying the drop-in

```powershell
cargo test -p lenslocker-ml -- --ignored
```

Runs `sessions_load_and_run_a_forward_pass_for_each_model_slot`, which only
needs `onnxruntime.dll` in place (it uses hand-built placeholder graphs for
the three model "slots", decoupled from the real weight files — see
`crates/ml/src/placeholder.rs`'s module doc for why). Confirms the
`ort`/DirectML `Session` plumbing itself works independently of whether the
real YuNet/SFace/SigLIP weights have landed yet.
