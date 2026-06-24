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

# Publishing And Editor Handoff

- `fsite` is the supported agent-facing surface. Do not bypass it with raw
  nostr events, direct registry writes, DNS edits, or proxy edits.
- Use `FINITE_SITES_API=https://api.finite.chat` for production unless the
  task is explicitly local development.
- Collaborative static sites use Project Repositories:

```sh
fsite describe workflow publish-static-site --output json
fsite describe workflow project-config --output json
fsite project apply --json project.json --dry-run --output json
fsite project apply --json project.json --send-invite --output json
fsite auth git PROJECT --email editor@example.com --store --output json
git clone https://git.finite.chat/PROJECT.git
```

- Commit deploy bytes and push the configured Deploy Branch. Finite Sites
  does not run builds.
- There is no direct bundle upload command. For static sites, commit the
  selected `finite.toml` output path, then push.
- Do not reconstruct source from rendered HTML. Use the Project Repository.
- A generated `/llms.txt` is platform guidance only. If a project publishes
  its own `/llms.txt`, preserve it and treat it as the project's authority.
- Never commit, print, or upload `.finite/`, `.env*`, private keys, dependency
  directories, or build caches.

# GitHub Release Shape

The public repository is expected to publish `fsite` binaries from tags named
`v*`. Keep README install commands and generated `/llms.txt` instructions in
sync with `.github/workflows/release.yml`.
