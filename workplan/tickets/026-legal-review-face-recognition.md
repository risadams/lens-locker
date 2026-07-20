---
id: 026
title: "Legal/ethics review: face recognition on personal photos + model-card disclaimers"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
blocked-by: []
---

## Question

Two distinct concerns, both flagged by [ticket 016's
research](016-research-offline-autotagging.md) but not resolved there:

1. **Model-card "deployed use" disclaimers.** OpenAI CLIP's and the LAION
   checkpoints' model cards state "any deployed use case... is currently out
   of scope" and mark surveillance/facial-recognition use as "always
   out-of-scope" — this is ethics-disclaimer language, not a legal
   restriction inside the MIT/Apache-2.0 grant itself, but LensLocker would
   be exactly the "deployed use" the disclaimer describes for the *tagging*
   model. Does this change anything given the personal/never-distributed
   scope constraint, or does it dissolve the same way the GPL-3.0 and HEVC
   questions did on the MVP map (tickets 020/021/022)?
2. **Face recognition on photos of real people, for personal use only.**
   Distinct from software licensing: this is about processing biometric-like
   data (face embeddings) of people who may not be the owner, on the owner's
   own device, never transmitted or shared. Sanity-check whether this raises
   anything worth documenting (e.g. a note that face data never leaves the
   device, clusters are locally stored only) even though there's no
   distribution/vendor role to trigger the kind of regulatory exposure the
   HEVC question turned on.

Expected shape of the resolution, pending the actual grilling session:
likely dissolves (same pattern as 020/021/022) with the reasoning documented,
but run the session rather than assuming.

## Resolution

Resolved 2026-07-20 via a grilling session. Both concerns dissolve, but each
through a **different mechanism** than the MVP map's GPL-3.0/HEVC
dissolutions (020/021/022) — worth being precise about which, since not every
"personal, never-distributed" dissolution argument transfers automatically.

**1. Model-card "deployed use" disclaimer — dissolves, but not via the
distribution-trigger logic.** GPL-3.0/HEVC hinge on a *distribution* trigger
that never fires for LensLocker. This disclaimer's trigger is *deployment*
("any deployed use case... is out of scope"), and a private LensLocker
instance tagging the owner's own library genuinely is a deployment — so
"never distributed" does not make this vanish the same way. It dissolves
instead for two independent reasons:
- It is **not a license term**. MIT/Apache-2.0 carry no use restriction; the
  disclaimer is non-binding ethics guidance in the model card, not a legal
  obligation — no restriction exists to inherit regardless of what LensLocker
  does with the model.
- LensLocker's actual use is **inside the model's intended purpose, not the
  targeted abuse case**. The disclaimer specifically calls out *surveillance*
  and *third-party facial recognition*; CLIP/SigLIP here does zero-shot
  tagging/categorization of the owner's own photos, and the actual
  face-recognition feature uses a wholly separate, dedicated model (YuNet +
  SFace, per [ticket 023](023-research-face-model-licensing.md)) — no
  CLIP/SigLIP-family model is ever used for facial recognition in this app.

**General principle, worth carrying forward explicitly**: the
personal/non-commercial/never-distributed posture cleanly dissolves
**distribution-triggered** obligations (GPL-3.0, HEVC patent exposure) but
does **not** automatically dissolve **field-of-use-triggered** restrictions.
Case in point: InsightFace's model license is not merely "non-commercial" (a
bar LensLocker clears) — its actual text restricts pretrained models to
"non-commercial **research** purposes only." LensLocker's use is
non-commercial *production* personal use, not research, so it fails that
restriction's field-of-use condition regardless of distribution status. This
doesn't reopen [ticket 023](023-research-face-model-licensing.md) (InsightFace
was already correctly closed as a dead end on the restriction's own terms,
not on a dissolution argument) — it's a guardrail for future tickets: check
*what kind* of restriction a license imposes before assuming the standing
personal-use posture dissolves it.

**2. Face recognition on non-owner people's photos — dissolves via a
personal/household-activity carve-out**, not a licensing argument. Privacy
frameworks that regulate biometric-like data generally exempt exactly this
activity: GDPR Article 2(2)(c)/Recital 18 places "processing of personal data
by a natural person in the course of a purely personal or household
activity" **entirely outside its scope**, naming photography as the paradigm
case; biometric-specific statutes (e.g. Illinois BIPA) are drafted around
commercial/business collection of biometric identifiers from others, not an
individual's private, undistributed hobby software with no commercial angle.
The activity itself — naming faces in one's own photo library, the digital
equivalent of writing names on the back of prints — falls outside what these
regimes were written to reach. Not legal advice; no primary legal source was
independently verified in this session (unlike 013/020's cited-source
research tickets) — this is a reasoned, documented judgment call, not a
citation-backed finding.

Two follow-ups to carry forward, not blockers:
- **Standing design commitment** (alongside the zero-network-access
  constraint LensLocker already carries): face embeddings, clusters, and
  person names never leave the device, never transmitted, stored locally
  only in the SQLite catalog. This is the concrete fact that makes the
  carve-out reasoning hold — name it explicitly in `ML-SPEC.md` rather than
  leaving it implicit, and revisit if any future export/interop feature
  changes it.
- **Forward-flag for face-tag export** (not resolved here): if a user later
  exports a photo carrying a named-face tag and hands it to someone
  (email, drive), that is the owner's own act of sharing their own file —
  the same shape as regular tag export already has (workplan/tickets/012's
  XMP sidecar export path) — not something LensLocker's software transmits
  automatically. Whichever ticket ends up covering face-tag export/interop
  should note this explicitly rather than assume it's obvious.

**Neither concern blocks the ML effort.** Both resolutions should be folded
into `ML-SPEC.md`'s eventual offline-enforcement/legal-posture section
(mirroring `SPEC.md` §8's role on the MVP map) when [ticket
036](036-assemble-ml-spec.md) assembles it.
