# Content-Addressed Blobs With Global Dedup

Blobs are stored once per sha256, shared across all sites and versions.
A publish session reports which manifest hashes the server lacks, and the
client uploads only those. Uploads are verified byte-for-byte against the
hash they claim before the blob row is recorded.

v1 stores blobs on the local filesystem behind a four-operation interface
(`put`/`has`/`get`/path); a Garage/S3 implementation replaces that crate
for production without touching the engine.

Known tradeoff: the missing-blob list reveals whether a given hash exists
anywhere on the platform (here.now and Workers static assets accept the
same leak). Logged in the technical debt ledger; per-owner dedup scoping is
the fallback if it ever matters.

**Considered Options**

- Per-site blob namespaces: no cross-tenant hash oracle, but no dedup of
  framework assets shared by every generated site.
- Global content-addressed store: maximal dedup, simplest serving path;
  chosen with the leak documented.
