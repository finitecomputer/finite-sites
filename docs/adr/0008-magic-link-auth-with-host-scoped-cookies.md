# Magic-Link Auth With Host-Scoped HMAC Cookies

Viewing a shared site works like a Google Doc: enter your email on the
site's own login page, click the mailed link, get a cookie. Tokens are
single-use and expire in 15 minutes; cookies are HMAC-signed
`(site, email, expiry)` triples set on the site's host only, and the share
table is re-checked on every request so revocation is immediate. The
login endpoint answers identically whether or not the email has access.

We build this (~300 lines) instead of deploying an identity provider:
Authelia/Authentik/Pomerium/oauth2-proxy all model operator-configured
policies, not user-self-service per-resource email ACLs, and none of them
speak "the agent shares site X with three emails" as an API call.

**Considered Options**

- oauth2-proxy + Google (status quo on finitecomputer boxes): Google-tied,
  per-route config churn, no per-site self-service ACL.
- Self-hosted IdP (Authelia/Authentik/Zitadel): heavy, wrong ACL shape.
- Tiny in-process magic-link service: exact fit, fewest moving parts;
  chosen. The mailer is a trait; only the dev mailer exists so far.
