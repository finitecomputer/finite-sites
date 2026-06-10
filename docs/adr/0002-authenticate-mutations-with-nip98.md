# Authenticate Every Mutation With NIP-98 Signed Requests

Every control-plane request carries a NIP-98 authorization: a kind-27235
nostr event signing the exact URL, method, and body hash, valid for ±60
seconds. The server verifies the schnorr signature and binds the request
to the signer's pubkey. There are no API keys, passwords, or sessions on
the control plane.

This is exactly the "sign a challenge to upload and edit" model: stateless
for the server, replay-bounded, and already standard across the nostr
ecosystem with mature implementations to cross-check against.

**Considered Options**

- NIP-98 per request: simple, stateless, spec'd; chosen.
- Blossom-style (BUD-11) scoped tokens with expirations: better for huge
  parallel uploads; can be added later for the upload path only.
- Issued API keys after a one-time signature: another credential to store
  and rotate in agent workspaces, for no gain at our scale.
