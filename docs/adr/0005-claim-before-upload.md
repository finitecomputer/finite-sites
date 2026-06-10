# Claim Before Uploading Content

A site name must be claimed (and the site key registered) before any
publish session can start. Carried over from finite-site ADR-0006.

Claiming first makes name conflicts cheap — they happen before bytes move —
and gives unpublished names a branded placeholder page immediately, which
is the behavior users see during training.

**Considered Options**

- Claim-on-first-publish: one fewer step, but conflicts surface after the
  upload work and the name's pre-publish state is undefined.
- Claim before upload: chosen.
