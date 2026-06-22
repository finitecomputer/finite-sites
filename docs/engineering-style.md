# Engineering Style

This repo borrows the parts of Tiger Style that fit Finite Sites' risk
profile: do the production-shaped thing early, keep control flow explicit,
and make invariants executable. It is the same style contract as Finite
Chat's, retargeted at a registry + serving system.

Reference: https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md

## Local Rules

- Authoritative server state must use schema, constraints, and transactions.
  Do not add JSON blobs for state the registry must query, lock, constrain,
  or recover. JSON is allowed for wire DTOs and bounded audit metadata.
- Store APIs must not hide database or corruption errors behind `Option`.
- Use typed error enums for crate boundaries. Do not use `anyhow` in proto,
  engine, store, or blob code; callers must be able to match errors without
  parsing strings.
- Every mutation that changes registry state must have a test covering the
  positive path and at least one negative/replay path.
- Prefer explicit branch structure for validation. Avoid clever `Option` or
  iterator control flow where the code is enforcing safety properties.
- Mutations that clients may retry (project apply, git ref reconciliation,
  sharing updates) must be idempotent or reject replays deterministically,
  and have tests for both.
- Do not use recursion in protocol, storage, serving, or CLI walk code.
  Loops must be iterative and visibly bounded.
- Put explicit limits on loops, batches, payloads, fanout, and manifests.
  All limits live in `finitesites-proto/src/limits.rs` with a why. If a loop
  is intentionally unbounded because it consumes a bounded source, say why
  near the loop.
- Prefer explicitly sized domain types at boundaries. Use `u32` or `u64`
  for protocol numbers, counters, and sizes; avoid exposing `usize` outside
  local indexing.
- Declare variables at the smallest useful scope.
- State invariants positively. Prefer `if value_is_valid { ... } else { ... }`
  over negated forms for safety checks.
- Centralize control flow and state mutation. The engine decides what
  happens; the store persists, the blob store holds bytes, the HTTP layer
  translates. Helpers either validate, compute, or persist one clear change.
- Keep compiler warnings at the strictest practical setting:
  `cargo clippy --all-targets -- -D warnings`.
- Do not do irreversible work directly in reaction to external events.
  Inbound HTTP is validated, persisted, and then interpreted from the
  registry's own state. Serving reads only committed versions.
- Always explain why for surprising constraints, explicit limits, schema
  choices, and security-relevant branches.
- Pass important options explicitly at call sites instead of relying on
  library defaults.
- Distinguish the control plane from the data plane. Project apply, git
  credential minting, git ref reconciliation, sharing, and tokens are control
  plane; blob bytes and site serving are data plane.
- Treat cache invalidation as a protocol decision. Any derived cache must
  name its source of truth, invalidation trigger, and stale-read behavior.
  (Today: ETags derive from blob hashes, which cannot go stale; viewer
  cookies are deliberately NOT a cache of the share table — the table is
  re-checked per request.)
- Audit every dependency addition before adding it. Prefer the standard
  library, existing workspace dependencies, or a small Rust crate over
  shell/Python tooling. New scripts should be Rust binaries or tests unless
  a non-Rust tool is clearly the better fit.
- Document tolerated technical debt in `docs/technical-debt-ledger.md`
  before relying on it. Each item needs an observed source, risk, first
  proof, and delete condition. A shortcut without a delete condition is not
  accepted debt; it is unfinished design.

## Assert Boundary

Use handled errors for client mistakes and operating conditions:

- output name already allocated, name reserved or invalid;
- pubkey has no active publish grant;
- manifest over limits, blob hash/size mismatch;
- deploy has missing blobs;
- login token unknown, used, or expired;
- email not on the share list.

Use assertions or corruption errors for internal contradictions:

- a finalized deploy without a version id;
- version file rows that do not match the deploy file rows they were copied
  from;
- a stored blob whose bytes no longer hash to its name;
- an allocated output missing immediately after its insert;
- a login token referencing a missing site.

Assertion policy:

- Target an average of two invariant checks per nontrivial function: one
  near ingress for the assumptions being consumed, and one near egress for
  the state or value being produced.
- Pair important assertions. Check data before writing it and again after
  reading it back from storage (blob writes, key files, version creation).
- Split compound assertions so failures identify the exact broken invariant.
- Pure decode/encode helpers may rely on type exhaustiveness instead of
  mechanical assertion count, but must reject impossible external values
  explicitly.

## Test Shape

- Every registry mutation gets valid and invalid tests.
- Every idempotent mutation gets success replay and conflicting-replay
  tests (project apply replay, git ref event replay, sharing update replay).
- Every storage invariant gets a restart test.
- The full project apply→git push→share→login→view loop has an end-to-end
  HTTP test that drives a real server the way the CLI, git, and a browser
  would.
- Add fuzz/property tests before changing manifest parsing, path decoding,
  or NIP-98 verification in ways that widen accepted inputs.

## Allocation Shape

Rust will allocate; the rule is to make allocations visible and bounded.

- Blob reads allocate at most MAX_FILE_BYTES, bounded at write time.
- Manifest processing is bounded by MAX_MANIFEST_FILES everywhere.
- Allocate request scratch near the handler boundary, not deep in
  validation helpers.

## Performance Sketch

Finite Sites should be network- and disk-bound before it is CPU-bound.

Initial sniff-test target for one finitesitesd process:

- site request: one registry lookup by name, one version-file lookup, one
  blob read — no writes, no allocation proportional to site size beyond the
  blob itself;
- git deploy version creation: one transaction touching publish staging,
  version, version files, site pointer, and audit row;
- project output allocation: one transaction, decided by unique indexes, not
  check-then-insert.

If a local dev server cannot handle hundreds of small site requests per
second on a laptop, assume accidental complexity until proven otherwise.

Optimize in this order: network, disk, memory, CPU.
