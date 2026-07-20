# Research: redistributable face detection + embedding models (InsightFace alternatives)

**Ticket:** [023 — Survey redistributable face detection/embedding model alternatives to InsightFace](../tickets/023-research-face-model-licensing.md)
**Status:** research only — surveys the redistributable model landscape so the face-recognition milestone can pick a detection + embedding pair with a clean, installer-safe license. No product decision taken here.

## Summary — there is a clean pair, and a real desktop precedent already ships it

**A fully redistributable, ONNX, offline-inference face-recognition stack exists today, and it is not InsightFace:**

- **Detection: OpenCV Zoo YuNet** — MIT license on *both* code and weights, 233 KB ONNX, 5 landmarks, WIDER-Face AP 0.884 / 0.866 / 0.750 (easy/medium/hard). ([opencv_zoo YuNet README + LICENSE](https://github.com/opencv/opencv_zoo/blob/main/models/face_detection_yunet/README.md))
- **Embedding: OpenCV Zoo SFace** — Apache-2.0 on *both* code and weights, MobileFaceNet architecture, 112×112 input, **128-dim** embedding, ~36.9 MB fp32 ONNX (int8-quantized variant also published), LFW 99.60%. ([opencv_zoo SFace README + LICENSE](https://github.com/opencv/opencv_zoo/blob/main/models/face_recognition_sface/README.md))

Both are MIT/Apache-2.0 — the *same redistribution bar ticket 016 applied to SigLIP/CLIP* — so they clear LensLocker's "ship the weights inside the NSIS installer" requirement without the InsightFace restriction. Crucially, **[digiKam](https://www.digikam.org)** — a mature, GPL, fully-offline *desktop* photo manager (much closer to LensLocker's architecture than Immich's microservice or PhotoPrism's server) — switched to exactly this pair (YuNet detection + SFace embedding, both ONNX) as its default face pipeline in the 8.5 / 8.6 releases (2024–2025). That is a direct existence proof that this pair is production-viable for the LensLocker use case. ([digiKam 8.5 release notes](https://www.digikam.org/news/2024-11-16-8.5.0_release_announcement/), [digiKam 8.6 release notes](https://www.digikam.org/news/2025-03-15-8.6.0_release_announcement/))

**The InsightFace path is a dead end for redistribution and does not need re-investigating** — see §1. InsightFace's *code* is MIT (commercial use fine), but *every* pretrained model it publishes (buffalo_l/m/s, antelopev2, its SCRFD detector weights, its ArcFace embedding weights — auto-download and manual alike) is licensed "for non-commercial research purposes only." There is no cleaner InsightFace-family checkpoint; the restriction is blanket across its model zoo. ([InsightFace python-package README, LICENSE section](https://github.com/deepinsight/insightface/tree/master/python-package))

**One nuance to hand to the legal/ethics pass ([ticket 026](../tickets/026-legal-review-face-recognition.md)), not to block on:** the clean *weights* licenses (MIT/Apache-2.0) are the grant made by the party that trained and published the weights (OpenCV). The underlying *training data* for SFace (and for the FaceNet-family alternatives) is the usual research-only face corpus (CASIA-WebFace / VGGFace2 / MS-Celeb-1M). This is the exact same shape as the LAION-training-data provenance note ticket 016 flagged for the CLIP/SigLIP weights (§2 of `offline-autotagging.md`): the trained-weights license is what governs redistribution, but the training-data provenance is a chain a legal reviewer will want documented. See §5. It does not change the redistribution verdict.

---

## 1. Is there *any* path to use InsightFace itself? (Dead end — documented so the map can move on)

Ticket 016 already established that Immich's re-hosted `buffalo_l` checkpoint is restricted. This pass confirms the restriction is not specific to that one checkpoint — it is InsightFace's blanket model-zoo policy, and it applies to the architectures LensLocker would otherwise want (SCRFD for detection, ArcFace for embedding).

From the **InsightFace python-package README, LICENSE section** (primary source):
> "The code of InsightFace Python Library is released under the MIT License. There is no limitation for both academic and commercial usage."
> "The pretrained models we provided with this library are available for **non-commercial research purposes only**, including both auto-downloading models and manual-downloading models."

([InsightFace python-package](https://github.com/deepinsight/insightface/tree/master/python-package))

Three sub-questions the ticket asked, each answered:

1. **A differently-licensed InsightFace-family checkpoint?** — **No.** The non-commercial clause is written to cover *all* provided models, explicitly "including both auto-downloading models and manual-downloading models." There is no permissively-licensed buffalo/antelope/SCRFD/ArcFace checkpoint hiding in the InsightFace zoo.
2. **The SCRFD detector specifically?** — The **SCRFD code** in the InsightFace repo carries an **Apache-2.0** LICENSE file ([`detection/scrfd/LICENSE`](https://github.com/deepinsight/insightface/blob/master/detection/scrfd/LICENSE)), but the **published SCRFD *weights*** are governed by the same InsightFace non-commercial model policy above. This is the code/weights divergence ticket 016 warned about, in its sharpest form: the architecture is Apache-2.0, the checkpoints are not.
3. **Train a redistributable model from InsightFace's published *architecture*?** — **Technically yes, practically unnecessary.** SCRFD (detection) and ArcFace (embedding) are published architectures — papers plus Apache-2.0/MIT reference code. Training fresh weights on a redistributable dataset would produce weights LensLocker could license itself. But this needs a training dataset + GPU compute + evaluation, and **other people have already done exactly this**: YuNet is an independently-trained MIT detector ([libfacedetection.train](https://github.com/ShiqiYu/libfacedetection.train)), and SFace is an independently-trained Apache-2.0 MobileFaceNet embedder. So the "train from architecture" path is real but redundant — the clean alternatives already exist off the shelf. **Recommendation: treat InsightFace as closed; do not build a training pipeline.**

---

## 2. Face detection candidates (bounding boxes + landmarks)

| Model | Code license | Weights license | Accuracy signal | ONNX | Approx. size | Source |
|---|---|---|---|---|---|---|
| **OpenCV Zoo YuNet** (`face_detection_yunet_2023mar`) | **MIT** | **MIT** (weights explicitly in the same MIT-licensed directory) | WIDER-Face AP **0.884 / 0.866 / 0.750** (easy/med/hard); detects faces ~10×10 to 300×300 px; 5 landmarks | **Yes — ships as `.onnx`** (fp32 + int8) | **~233 KB** fp32 (~75k params); int8 smaller | [README+LICENSE](https://github.com/opencv/opencv_zoo/blob/main/models/face_detection_yunet/README.md), [HF `opencv/face_detection_yunet`](https://huggingface.co/opencv/face_detection_yunet) |
| MediaPipe **BlazeFace** (Google) | **Apache-2.0** | **Apache-2.0** (Google licenses code + weights Apache-2.0) | Tuned for mobile-GPU real-time; 6 landmarks; short- and full-range variants | Not officially; **community/Qualcomm ONNX exports exist** (TFLite→ONNX; e.g. Qualcomm AI Hub, `hollance/BlazeFace-PyTorch`) | ~1.1 MB TFLite | [Google MediaPipe face detector docs](https://developers.google.com/mediapipe/solutions/vision/face_detector), [qualcomm/MediaPipe-Face-Detection](https://huggingface.co/qualcomm/MediaPipe-Face-Detection) |
| InsightFace **SCRFD** | Apache-2.0 (repo `detection/scrfd/LICENSE`) | **Restricted — non-commercial research only** (InsightFace model policy) | State-of-the-art efficiency/accuracy on WIDER-Face (ICLR-2022 paper) | Exportable; some restricted checkpoints published as ONNX | family from ~3.5 MB (0.5g) up | [SCRFD paper](https://arxiv.org/abs/2105.04714), [InsightFace LICENSE](https://github.com/deepinsight/insightface/tree/master/python-package) |
| InsightFace **RetinaFace** | MIT (repo code) | **Restricted — non-commercial research only** | High-accuracy detector w/ 5 landmarks | Exportable | varies | [InsightFace LICENSE](https://github.com/deepinsight/insightface/tree/master/python-package) |
| **YOLOv8-face / YOLO-face** (Ultralytics-derived) | **AGPL-3.0** | **AGPL-3.0** (Ultralytics: weights produced by AGPL training code inherit AGPL) | Strong; many community variants | Yes | yolov8n-face ~6 MB | [Ultralytics license](https://www.ultralytics.com/license), [Ultralytics/YOLOv8 HF card (`license: agpl-3.0`)](https://huggingface.co/Ultralytics/YOLOv8) |

**Detection verdict:** **YuNet is the clean pick** — MIT on both code and weights, tiny (negligible installer budget), already ONNX, already the default detector in a shipping offline desktop photo manager (digiKam, §4). BlazeFace is a viable Apache-2.0 backup but needs a non-official ONNX conversion step and only ships a "detector + 6 coarse landmarks" — YuNet's 5 landmarks are adequate for the align-then-embed pipeline. **Avoid the Ultralytics YOLO-face family: AGPL-3.0 is a copyleft trap** — the model weights themselves inherit AGPL, which would force LensLocker's whole application source open if it were ever distributed. (For LensLocker's *never-distributed, personal-use-only* posture per ML-MAP.md, AGPL technically imposes no obligation because there is no conveyance — but selecting an AGPL model would silently re-couple the whole app to the "never distribute" constraint the way the MVP map's GPL-3.0 note did; not worth it when MIT YuNet is strictly better on size and licensing.) SCRFD/RetinaFace = the InsightFace dead end (§1).

---

## 3. Face embedding candidates (face crop → comparable vector)

| Model | Code license | Weights license | Accuracy signal | Emb. dim | ONNX | Approx. size | Source |
|---|---|---|---|---|---|---|---|
| **OpenCV Zoo SFace** (`face_recognition_sface_2021dec`) | **Apache-2.0** | **Apache-2.0** (explicit: "All files in this directory are licensed under Apache 2.0") | **LFW 99.60%**; CALFW 93.95, CPLFW 91.05, AgeDB-30 94.90, CFP-FP 94.80 | **128** | **Yes — ships as `.onnx`** (fp32 + int8bq) | **~36.9 MB** fp32; int8bq much smaller | [README+LICENSE](https://github.com/opencv/opencv_zoo/blob/main/models/face_recognition_sface/README.md), [HF `opencv/face_recognition_sface`](https://huggingface.co/opencv/face_recognition_sface) |
| **facenet-pytorch** (InceptionResnet-v1, timesler) | **MIT** (© Timothy Esler 2019) | Weights: **no explicit license tag**; lineage MIT (ported from David Sandberg's MIT FaceNet), but **trained on VGGFace2 / CASIA-WebFace (research-only datasets)** — see §5 | **LFW 0.9965** (VGGFace2) / 0.9905 (CASIA) | **512** | Not shipped; **exportable via `torch.onnx`** | fp32 InceptionResnet-v1 ~90 MB class | [facenet-pytorch repo + LICENSE.md](https://github.com/timesler/facenet-pytorch), [davidsandberg/facenet LICENSE (MIT)](https://github.com/davidsandberg/facenet) |
| **AdaFace** (CVLface / mk-minchul) | **MIT** (implementation) | Pretrained checkpoints: **inherit training-dataset license** (MS1MV2 / WebFace4M — research-oriented); verify per checkpoint | SOTA on IJB-B/C; beats ElasticFace (IR-101) | 512 | Exportable (PyTorch) | IR-50 ~170 MB / IR-101 ~250 MB class | [AdaFace repo](https://github.com/mk-minchul/AdaFace), [CVLface AdaFace IR50 MS1MV2 card](https://huggingface.co/minchul/cvlface_adaface_ir50_ms1mv2) |
| InsightFace **ArcFace** (buffalo_l/antelopev2) | MIT (code) | **Restricted — non-commercial research only** | Highest published accuracy of the family | 512 | Exportable / restricted ONNX published | buffalo_l ~330 MB pack | [InsightFace LICENSE](https://github.com/deepinsight/insightface/tree/master/python-package), and ticket 016 §4 |

**Embedding verdict:** **SFace is the clean pick** — it is the *only* surveyed embedder whose *weights* carry an explicit permissive grant (Apache-2.0) from the publisher, it is already ONNX, it is small enough to bundle (~37 MB fp32, less quantized), it posts a strong LFW 99.60%, and — like YuNet — it is the shipping default in digiKam. Its 128-dim output is smaller than the 512-dim ArcFace/FaceNet vectors, which is *good* for `sqlite-vec` storage/scan cost at 100k+ scale (see ticket 016 §3 — brute-force KNN cost scales with dimension).

**facenet-pytorch is the credible backup** if a 512-dim embedding or higher recall is wanted later: MIT code, MIT-lineage weights, well-trodden ONNX export. Its only encumbrance is the training-data provenance (§5) — the *author* attaches no restrictive weights license, but the weights were trained on research-only corpora. AdaFace is higher-accuracy still but its checkpoints more explicitly defer to their (research-only) training-dataset licenses, making it the weakest of the three on redistribution cleanliness. ArcFace-via-InsightFace remains the dead end (§1).

---

## 4. Precedent — digiKam (extends `offline-autotagging.md` §4)

Ticket 016's precedents (Immich, PhotoPrism) were both *server/microservice*-shaped and both lean on InsightFace for faces. **digiKam is the missing precedent that matches LensLocker's shape and avoids InsightFace:**

- **Architecture match:** digiKam is a single desktop application (C++/Qt), fully offline, no server, no per-user cloud model fetch — the closest architectural analogue to LensLocker among the surveyed photo managers.
- **Exact model pair:** As of digiKam 8.5 (Nov 2024) and refined in 8.6 (Mar 2025), "all processing is now handled by **YuNet for face detection and SFace for feature extraction**," both as ONNX models; YuNet replaced its older MobileNetSSD/YOLOv3 detectors "in both speed and accuracy," and SFace is "the default deep-learning model for face recognition." ([digiKam 8.5 notes](https://www.digikam.org/news/2024-11-16-8.5.0_release_announcement/), [digiKam 8.6 notes](https://www.digikam.org/news/2025-03-15-8.6.0_release_announcement/), [digiKam faces manual](https://docs.digikam.org/en/maintenance_tools/maintenance_faces.html))
- **Offline provisioning:** digiKam hosts the YuNet ONNX itself on KDE's own mirrors ([files.kde.org digikam/facesengine/yunet](https://files.kde.org/digikam/facesengine/yunet/)), i.e. it does not depend on a Hugging Face fetch at runtime — a packaging pattern LensLocker's bundle-in-installer approach can mirror (relevant to [ticket 032](../tickets/032-design-model-provisioning.md)).
- **GPU note:** digiKam runs these ONNX models on GPU via OpenCL for the pre/post-processing and inference. LensLocker's chosen runtime is `ort` + DirectML (ticket 016), a different acceleration path — the *models* transfer directly (they are plain ONNX), the *acceleration wiring* does not.
- **License note:** digiKam itself is GPL, but that is digiKam's *application* license; the YuNet (MIT) and SFace (Apache-2.0) model files it bundles are independently permissive — which is precisely why LensLocker can adopt the same two models without inheriting digiKam's GPL.

**Implication for LensLocker:** digiKam is the existence proof that a permissive, InsightFace-free, ONNX, single-binary-desktop face pipeline works in a real shipping product. It validates the *model choice and the offline-desktop shape simultaneously* — the gap ticket 016 noted (Immich/PhotoPrism validate models but not the single-binary-offline architecture) is closed by this precedent.

---

## 5. The training-data provenance caveat (parallel to ticket 016's LAION note)

The permissive *weights* licenses above are grants from the org that trained and published the weights, not statements about the training data:

- **SFace** (OpenCV Zoo, Apache-2.0 weights) is a MobileFaceNet trained with the SFace loss from Zhong & Deng, *"SFace: Sigmoid-Constrained Hypersphere Loss for Robust Face Recognition"* (TIP 2021). The paper's experiments train on **CASIA-WebFace / VGGFace2 / MS-Celeb-1M** ([arXiv 2205.12010](https://arxiv.org/abs/2205.12010)); the exact corpus behind the specific OpenCV Zoo checkpoint is not spelled out on the model card.
- **facenet-pytorch / davidsandberg-facenet** weights are trained on **VGGFace2** (LFW 0.9965) and **CASIA-WebFace** (LFW 0.9905).
- **VGGFace2** (Univ. of Oxford) and **CASIA-WebFace** (CASIA) are **released for non-commercial academic research only**, with redistribution restrictions on the *datasets themselves*.

This is structurally identical to the LAION-2B situation ticket 016 documented for CLIP/SigLIP: the *trained-weights* license is what governs redistributing the model file, and it is clean (Apache-2.0 / MIT), but the *training-data* provenance is a research-only chain a legal reviewer should have on record. For LensLocker's stated posture — **personal/internal, never distributed outside the owner** (ML-MAP.md standing constraints) — both the weights-redistribution question *and* the training-data question dissolve (same dissolution logic the map applies to GPL-3.0/HEVC). The redistribution analysis above is what matters *if that posture ever changes*, and even then the publisher's explicit permissive weights grant is the operative license. This belongs in [ticket 026](../tickets/026-legal-review-face-recognition.md), not as a blocker here.

---

## Recommended pairing and what downstream tickets can rely on

**Recommended clean pair: YuNet (detect) → align on 5 landmarks → SFace (embed, 128-dim) → cluster.** Both MIT/Apache-2.0 on code *and* weights, both native ONNX (drop-in for `ort`+DirectML), combined installer weight ~37 MB (dominated by SFace; YuNet is negligible), and both already shipping in a comparable offline desktop product.

Facts the blocked tickets need:

- **[Ticket 028 — face clustering/grouping UX]:** the embedding is **128-dim** (SFace), cosine-similarity comparable; SFace's own recommended verification cosine threshold is **0.363** (LFW-tuned) — a concrete starting point for the "when are two faces the same person" decision the clustering/merge/split UX is built on. Detection yields bounding box + 5 landmarks per face, so multi-face-per-image overlay is natural. Expect real-world clustering error (any local model) → the merge/split correction flow remains load-bearing, as the ticket already assumes.
- **[Ticket 030 — background execution model]:** both models are small and CPU-viable (digiKam runs them CPU-only acceptably, "25–50% faster" gains from its 8.6 optimizations were on CPU); GPU via DirectML is an accelerator, not a requirement — so import-time vs background-pass is a free design choice, not forced by model cost. A model-version bump (e.g. swapping SFace for a future embedder) invalidates stored 128-dim vectors → re-embed trigger needed, same pattern ticket 016 reserved for CLIP re-embeds.
- **[Ticket 032 — model provisioning / installer]:** budget **~37 MB** for the face pair (SFace fp32 ~36.9 MB + YuNet ~0.23 MB), *before* the tagging model and the ONNX Runtime DLL; int8-quantized SFace/YuNet variants exist if the budget is tight. License/attribution to ship: MIT (YuNet) + Apache-2.0 (SFace) NOTICE/attribution text in the installer/about screen. digiKam's self-hosted-ONNX pattern (KDE mirrors, no HF fetch) is the packaging precedent to copy for bundle-in-installer.
- **The InsightFace question is closed:** do not plan around any InsightFace checkpoint or an InsightFace-architecture training pipeline. The permissive off-the-shelf pair supersedes it.

---

## Open questions (not confirmed from a primary source in this pass)

- **SFace exact training corpus for the specific OpenCV Zoo checkpoint** — the SFace *paper* trains on CASIA-WebFace/VGGFace2/MS-Celeb-1M, but the model card does not pin which corpus produced `face_recognition_sface_2021dec.onnx`. Matters only for the §5 provenance chain, not the (confirmed Apache-2.0) redistribution grant.
- **SFace int8-quantized on-disk size** — the fp32 ONNX is confirmed ~36.9 MB and an `int8bq` variant is published, but its exact MB was not confirmed from a primary source; verify before locking ticket 032's size budget if quantization is used.
- **SFace / YuNet accuracy on *LensLocker's own* photo distribution** — the cited numbers are LFW / WIDER-Face benchmark accuracy, not personal-photo-library conditions (group shots, kids/aging, low light). digiKam's shipping use is reassuring but not a benchmark; a small real-library eval before locking the embedder is prudent (parallels ticket 016's "re-benchmark sqlite-vec on real hardware").
- **BlazeFace ONNX export fidelity** — Apache-2.0 and attractive if an even smaller detector is ever wanted, but there is no *official* Google ONNX export; the community/Qualcomm conversions were not validated for landmark/output parity in this pass. YuNet avoids the question entirely.
- **facenet-pytorch weights license, strictly** — the *code* is MIT and the weights carry no restrictive tag, but the author never issues an explicit weights license; the redistribution comfort rests on MIT-code lineage + the §5 training-data caveat, not an affirmative weights grant like SFace's. Prefer SFace precisely because its weights grant is explicit.
- **AdaFace / ElasticFace / higher-accuracy embedders** — surveyed only enough to place them (MIT code, dataset-encumbered weights). If SFace's accuracy proves insufficient on a real eval, a deeper pass on whether any *higher-accuracy* embedder carries an explicit permissive *weights* grant (rather than deferring to dataset terms) would be the follow-up — none was found permissively-licensed at the weights level in this pass except SFace.
