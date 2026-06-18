# Feature Request: Email-Keyed Publishing And Multi-Editor Sites

## Problem

Finite Sites currently lets an allowed publisher claim and publish a site, then
share viewing access with email addresses. That is close to the Google Doc
mental model for readers, but it does not yet let a human identify a site by an
email owner or add additional email-keyed editors who can update the same site.

For the FiniteChat native mockup, the desired operating model is:

- `paul@finite.vip` owns the published site.
- `skyler_bot@finite.vip` can be added as an additional editor.
- `paul@finite.vip` remains the owner and can keep publishing, manage editors,
  and remove editor access.

## Requested Behavior

Add email-keyed publishing and multi-editor support on top of the existing
private-by-default sharing model.

Viewer sharing should keep using the existing email ACL and magic-link access.
Editor access is a separate authority: an editor can publish a new version of a
site but does not automatically become the site owner.

## Product Requirements

- A site can have an owner email, beginning with `paul@finite.vip` for the
  FiniteChat mockup.
- A site owner can add and remove editor emails, beginning with
  `skyler_bot@finite.vip`.
- Publishing can be authorized by a verified owner or editor email identity,
  without exposing nostr keys, npubs, manifests, blobs, or signing details in
  the user-facing workflow.
- Adding an editor must not remove or downgrade the existing owner.
- Viewer sharing remains independent from editor publishing access.
- The CLI should expose this as a simple document-like workflow, for example:

```sh
fsite publish finitechat-native-mockup dist --owner-email paul@finite.vip
fsite editors finitechat-native-mockup --add-email skyler_bot@finite.vip
fsite editors finitechat-native-mockup
```

The exact command names are implementation-owned, but the user-facing model
should be email keyed.

## Acceptance Criteria

- `paul@finite.vip` can publish and republish `finitechat-native-mockup`.
- `skyler_bot@finite.vip` can publish a new version after being added as an
  editor.
- Removing `skyler_bot@finite.vip` prevents future publishes from that email.
- `paul@finite.vip` remains owner after editor additions, removals, and
  republish operations.
- Viewer shares created with `fsite share --add-email` do not grant publish
  rights.
- Editor publish attempts are audited with site name, editor email, version,
  and timestamp.
- Replay of an old editor publish authorization cannot create a new version
  after editor access is revoked.

## Evaluation Design

- Positive test: owner publishes, adds editor, editor republishes, owner still
  appears as owner.
- Negative test: a viewer-only shared email cannot publish.
- Negative test: a removed editor cannot publish.
- Replay test: a captured editor authorization from before revocation fails
  after revocation.
- Regression test: existing private/shared/public viewer access and magic-link
  login continue to work unchanged.
