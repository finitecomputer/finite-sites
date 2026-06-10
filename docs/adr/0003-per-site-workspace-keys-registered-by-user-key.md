# Keep Per-Site Workspace-Held Signing Keys, Registered By The User Key

Each site gets its own signing key, generated in the agent workspace at
`.finite/sites/NAME.env` (carried over from finite-site ADR-0002). The
user's identity key signs the claim that registers the site key; from then
on the site key signs publishes and sharing changes for that one site.

The registry stores only public keys. Revoking one site's key never
touches the user identity or other sites, and a leaked workspace key
compromises exactly one site.

**Considered Options**

- One user key signs everything: fewer files, but a single compromise
  surface and no per-site revocation.
- SaaS-held per-site keys: strong operator control, but the service could
  publish as the user — the opposite of the trust story we want.
- Workspace-held per-site keys with a user-key grant: matches the
  agent-as-editor model; chosen.
