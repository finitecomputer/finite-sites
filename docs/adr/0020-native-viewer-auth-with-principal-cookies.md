# Native Viewer Auth With Principal Cookies

Native Finite surfaces authenticate shared site viewing with a direct local
NIP-98 request to the site's serving host, not through relays, remote signers,
or page JavaScript. The endpoint mints the same host-scoped Viewer Cookie as
magic links, but the cookie now carries a Principal id instead of an email so
External Principal and Native Principal shares use one access check.

**Consequences**

- Email magic links remain the External Principal bootstrap path.
- Native app sessions prove only that the local User Key signed a bounded
  site-host challenge; the Share table still decides access on every request.
- Agents use their own Agent Key or an explicit future Agent Delegation; the
  native app never gives the user's personal key to an agent.
