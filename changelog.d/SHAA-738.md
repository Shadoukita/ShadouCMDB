### Security: the audit trail records the client address the server can vouch for

Behind a reverse proxy that appends to `X-Forwarded-For`, a client could put any address at the
start of the header, and that address was what the audit log (`ipAddress`), the sessions list and
an API token's last-used address recorded. The real client address, which the sign-in throttle
already used, was stored nowhere ([GH#282]).

`ipAddress`, `sessions.ip_address` and `api_tokens.last_used_ip` now hold the same address the
sign-in throttle uses: the TCP peer, or, when the peer is listed in `TRUSTED_PROXIES`, the client
that proxy reports (read right to left, skipping listed proxies). The first forwarded address,
which the client controls, is kept in audit rows as the new `claimedIpAddress` when it differs;
`peerIpAddress` (the TCP peer) is unchanged.

**Upgrade:** behind a reverse proxy, list it in `TRUSTED_PROXIES` (see the
[deployment guide][deployment]). Without that, new audit rows, sessions and token uses record the
proxy's address as `ipAddress`, with the forwarded client address in `claimedIpAddress`. Rows
written before the upgrade are not changed. Audit exports and SIEM rules that read `ipAddress`
keep working; rules that should see the client's own claim read `claimedIpAddress`.

[GH#282]: https://github.com/Shadoukita/ShadouCMDB/issues/282
[deployment]: docs/deployment.md#https-and-session-cookies
