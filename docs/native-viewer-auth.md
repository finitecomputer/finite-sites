# Native Viewer Auth

Native Finite apps open shared Finite Sites without the email magic-link flow
by signing a local NIP-98 request directly to the site host:

```http
POST https://{site}.{base_domain}/_finite/auth/native-session
Authorization: Nostr <kind-27235-event>
Content-Type: application/json
```

```json
{
  "purpose": "finite_site_view_session",
  "return_to": "/path",
  "client": "finite-chat-ios",
  "nonce": "client-random"
}
```

The server verifies the exact URL, method, body hash, signature freshness,
site host, signer pubkey, return path, and nonce replay. A valid request
mints the normal host-scoped HttpOnly Viewer Cookie and redirects to
`return_to`. The page never receives the private key or signed event, and no
Nostr relay participates in the ceremony.

Access is still decided by the Share table on every served request. Email
magic links create External Principal viewer cookies; native auth creates
Native Principal viewer cookies. Revoking the Share row removes access on the
next request for both paths.

Minimum regression matrix:

- valid native session sets a cookie and loads the site;
- stale or mismatched NIP-98 signature is rejected;
- URL, method, and payload mismatches are rejected;
- unshared signer is rejected;
- replayed nonce is rejected;
- malformed external `return_to` is rejected;
- public sites continue to work without auth;
- email magic-link login remains unchanged.
