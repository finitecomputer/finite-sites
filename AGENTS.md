# Prompting Contract

When a prompt is not a simple question or very small ask, guide the user
toward:

1. Self-contained problem statement
2. Acceptance criteria
3. Constraints: musts, must-nots, preferences, and escalation points
4. Decomposition into clean phases
5. Evaluation design for the tests or checks that prove success

# Working In This Repo

- Read `CONTEXT.md` first and use its vocabulary.
- Follow `docs/engineering-style.md`; it is enforced, not aspirational.
- Decisions live in `docs/adr/`; add an ADR when you change one.
- Shortcuts require an entry in `docs/technical-debt-ledger.md` with a
  delete condition, before you rely on them.

## Commands

```sh
just test          # cargo test --workspace
just lint          # cargo fmt --check + clippy --all-targets -D warnings
just dev           # run finitesitesd against .dev-data
just fmt           # rustfmt
```

Every mutation needs a positive test and at least one negative/replay test.
`cargo clippy --all-targets -- -D warnings` must pass before any handoff.
