# Cloudflare-Proxied Edge With Origin CA Certificates

`finite.chat` lives on Cloudflare. The wildcard and API records are
proxied: Cloudflare terminates public TLS with its Universal SSL wildcard
and absorbs DDoS, and the origin (Caddy on the SaaS box) presents a
15-year Cloudflare Origin CA certificate in Full (strict) mode. Outbound
magic-link mail goes through Resend/Postmark (Cloudflare Email Routing is
inbound-only); their DKIM records live in the same zone.

This keeps Cloudflare strictly at the dumb-pipe layer: DNS, TLS
termination, and flood absorption. No Workers, no KV, no per-tenant
Cloudflare state — serving logic stays in finitesitesd, so leaving
Cloudflare is a nameserver change plus a cert swap (ADR-0001's
no-lock-in stance holds).

**Considered Options**

- DNS-only records + Let's Encrypt DNS-01 wildcard on the box: no
  Cloudflare in the request path, but exposes the shared SaaS box's IP to
  direct floods and needs a Caddy DNS-plugin build plus a zone-scoped API
  token living on the box.
- Proxied records + ACME on the origin anyway: workable, but the Origin CA
  cert removes ACME entirely for this zone — fewer moving parts.
- Proxied records + Origin CA cert: simplest origin, edge protection,
  trivially reversible; chosen.
