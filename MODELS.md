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

## 3. SFace (face embedding) — `face_recognition_sface/face_recognition_sface_2021dec.onnx`

OpenCV Zoo, Apache-2.0 license (code + weights):
`https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface`
— the file is `face_recognition_sface_2021dec.onnx` in that directory; same
subdirectory convention as YuNet above.

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
