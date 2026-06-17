# Operator Allowlist Instead Of Billing (v1)

Superseded for ongoing product design by ADR-0016. The v1 operator command
surface remains, but it now writes `operator` rows into the publish grant cache
instead of being the only authorization model.

Publishing requires the owner's pubkey to be on an operator-curated
allowlist (`finitesitesd allow <npub>`). Allowlisted users get effectively
unlimited hosting within the per-owner and per-site limits in
`finitesites-proto/src/limits.rs`. Payments are explicitly out of scope.

De-allowlisting an owner stops new publishes for all their sites on the
next request, while already-published sites keep serving — the operator
lever is "stop the bleeding", not "take sites down" (disabling a site is a
separate status).

**Considered Options**

- Bitcoin/Lightning billing (BTCPay): the eventual model for non-VIP
  users, but a whole subsystem that v1 does not need to prove anything.
- Open registration with quotas: invites abuse before there is any abuse
  tooling.
- Operator allowlist keyed on npub: matches "VIPs publish out of the box";
  chosen.
