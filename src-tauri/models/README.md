# `src-tauri/models/`

Bundled ML runtime + model files (`workplan/ML-SPEC.md` §10), extracted by
the NSIS installer into the Program Files install directory alongside the
app executable — see `tauri.conf.json`'s `bundle.resources`. Not fetched at
build or run time (zero network access, ever — `CLAUDE.md`'s first
non-negotiable constraint); every file below is a manual, one-time drop-in.

See [`MODELS.md`](../../MODELS.md) at the repo root for exactly where to
get each one and how to verify it. Expected paths (matching
`lenslocker-ml`'s `ModelKind::relative_path()` and `dylib_path()` — dropping
a real export in under the exact path below needs no code change):

- `onnxruntime.dll` — DirectML build of ONNX Runtime.
- `face_detection_yunet/face_detection_yunet_2023mar.onnx` — YuNet face detection.
- `face_recognition_sface/face_recognition_sface_2021dec.onnx` — SFace face embedding.
- `siglip-so400m-onnx/model.onnx` + sibling `model.onnx_data` — self-converted
  SigLIP `so400m` (tagging embeddings); both files required together.

**`model.onnx_data` (~3.3GB) does not ship in the NSIS installer** — a real
`makensis.exe` limitation (confirmed 32-bit; it can't `File`-embed a single
file this large), not a judgment call. Everything else in this directory
still ships normally. After installing, copy `model.onnx_data` into
`<install dir>\models\siglip-so400m-onnx\` by hand — see MODELS.md §4 for
the full explanation. Tagging analysis just stays backed-off/retrying
until it's there; nothing else in the app depends on it.

`*.onnx`/`*.dll` in this directory are gitignored (repo-bloat judgment
call, flagged rather than assumed — revisit if the project later wants a
fully self-contained checkout instead).

**Whatever `onnxruntime.dll` is extracted from may also contain
`onnxruntime.lib`, `onnxruntime_providers_cuda.dll`,
`onnxruntime_providers_tensorrt.dll`, `onnxruntime_providers_shared.dll`,
and their `.lib` counterparts (~240MB together, confirmed) — fine to leave
sitting in this directory for local dev, but `tauri.conf.json`'s
`bundle.resources` deliberately lists `onnxruntime.dll` by exact name
rather than globbing the whole directory, so none of these ship in the
installer.** This app only ever registers the DirectML or CPU execution
provider (never CUDA/TensorRT), confirmed by running the real SigLIP/
SFace/YuNet sessions with only `onnxruntime.dll` present (MODELS.md §1).
If you add a genuinely new required file here, add it to
`tauri.conf.json`'s resource list explicitly too — an unlisted file simply
won't ship, silently.
