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

`*.onnx`/`*.dll` in this directory are gitignored (repo-bloat judgment
call, flagged rather than assumed — revisit if the project later wants a
fully self-contained checkout instead).
