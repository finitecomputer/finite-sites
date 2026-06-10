# Publish Immutable Versions With A Latest Pointer

Each finalized publish creates an immutable version; the site serves the
version its active pointer names, and the pointer flip is one transactional
update. Carried over from finite-site ADR-0003.

Versions reference content-addressed blobs, so history is cheap (unchanged
files are stored once) and rollback is a future pointer update, not a
re-upload.

**Considered Options**

- Mutable overwrite: simpler, but weak rollback and audit semantics, and
  in-flight requests could see mixed versions.
- Immutable versions with latest pointer: slightly more metadata; chosen.
