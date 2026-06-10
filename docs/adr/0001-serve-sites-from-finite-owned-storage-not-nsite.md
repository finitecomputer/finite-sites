# Serve Sites From Finite-Owned Storage, Not Nsite

Finite Sites serves site content from its own registry and blob store
behind one wildcard domain, instead of publishing nostr nsite manifests
(kinds 15128/35128) resolved through relays and Blossom servers. This
supersedes the finite-site prototype's serving substrate while keeping its
claim, registry, immutable-version, and workspace-key designs.

Cold nsite resolution fans out to relays and Blossom servers, missing
blobs are endemic in that ecosystem, and a gateway that caches enough to
be fast is a centralized host anyway — with the operational complexity but
without the control. Owning the serving path is what makes private sites,
email sharing, instant revocation, and future stateful tiers possible.

**Considered Options**

- Nsite + blessed gateway (the prototype): censorship-resistance story,
  but slow cold paths, no private sites, and we run the hard parts anyway.
- Finite-owned storage and serving: full control over access and latency;
  an optional future nsite *export* can restore the portability story.
- here.now or similar hosted service: no nostr auth, no self-hosting, and
  a third party between users and their sites.
