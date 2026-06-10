# Sites Are Private By Default With Google-Doc-Style Sharing

A new site is `private`. Sharing is a per-site visibility setting plus an
email ACL: `private` (nobody), `shared` (listed emails via magic link), or
`public`. Making a site public requires an explicit `confirm_public` flag,
which the agent may only set after warning the human.

This inverts the finite-site prototype's public-only v1 — safer for
training rooms, and it makes the sharing model (the thing users actually
asked for) a first-class primitive instead of a proxy bolt-on.

**Considered Options**

- Public-only v1 with a warning (the prototype): simplest, but the
  dangerous default, and private sharing was the most-requested behavior.
- Private by default with explicit public confirmation: chosen.
- Per-path ACLs: Google-Docs sharing is per-document; per-site matches
  that mental model and keeps the gate one check per request.
