# ADR-0018: Generated llms.txt For Agent Editor Handoff

## Status

Accepted.

## Context

Email-keyed editors and Source Snapshots make multi-editor publishing possible:
an editor can verify an email, pull the source snapshot, make a change, and
publish a new Version with updated source. The remaining handoff problem is
discovery. If an owner sends another human a site link, that human's agent
needs to learn the correct edit flow without scraping rendered HTML and
without being taught Finite Sites internals out of band.

The web convention for agent-readable site instructions is `/llms.txt`. Finite
Sites can provide that file for collaborative sites, but it must not overwrite
or shadow a file the user intentionally published at the same path.

## Decision

For static Finite Sites, the serving plane may synthesize `/llms.txt` when all
of these are true:

- the site is published;
- the active Version has no exact `/llms.txt` manifest entry;
- the site has at least one active Editor;
- the active Version has a Source Snapshot.

The generated file is platform guidance, not user content. It includes the
site name, canonical site URL, API URL, GitHub release/source install options
for `fsite`, and the email-keyed source-pull/publish commands. It does not
include owner emails, editor emails, source hashes, tokens, keys, or private
artifact bytes.

A user-authored `/llms.txt` wins. Once a Version contains an exact
`/llms.txt` manifest entry, requests for that path follow normal site serving
and visibility rules.

App sites do not get a generated file in v1 because an app may handle
`/llms.txt` dynamically and the platform cannot prove the user did not write
that path.

## Consequences

An owner can send a site URL to another person and tell them to have their
agent inspect `/llms.txt`. If the editor has been granted access, the agent
can verify its email key, pull source, edit, test, and republish without
manual registry details.

The generated file is served with `Cache-Control: no-store` because editor
grants and Source Snapshot availability are registry state, not immutable
content.

Private site content remains private: a generated file can appear before
viewer login, but a user-authored file is gated like any other published file.
This intentionally treats the generated file as public platform documentation
for an editable site, not as part of the site artifact.
