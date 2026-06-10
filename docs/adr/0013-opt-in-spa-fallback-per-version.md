# Opt-In SPA Fallback, Recorded Per Version

A publish may be marked as a single-page app (`fsite publish NAME PATH
--spa`). On an SPA version, request paths that match no manifest file
serve `/index.html` with status 200, so history-API client-side routers
survive deep links and refreshes. SPA manifests must contain
`/index.html`; non-SPA versions keep exact-match semantics with the
site's `404.html`.

The flag lives on the version, not the site: routing mode is a property
of the published artifact, and republishing a non-SPA build must restore
404 semantics with the same pointer flip that activates it.

**Considered Options**

- Always falling back to `/index.html`: breaks correct 404s for the
  majority of sites (plain multi-page output) and silently swallows
  broken links.
- The `404.html` convention (GitHub-Pages style): works with zero code
  but serves app routes with status 404; kept as a compatible behavior,
  not the answer.
- Site-level setting mutated like visibility: survives republishes that
  are no longer SPAs, leaving stale routing semantics.
- Per-version opt-in flag at publish time: chosen.
