---
id: 026
title: "Legal/ethics review: face recognition on personal photos + model-card disclaimers"
type: workplan:grilling
status: open
assignee:
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
