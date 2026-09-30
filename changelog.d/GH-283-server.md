### Security: slow anonymous request bodies no longer block sign-in, even behind a streaming proxy

The anonymous routes (setup, sign-in and its MFA step, OIDC sign-in, branding) took their slot in
the anonymous request pool before their body arrived. Behind a reverse proxy that streams request
bodies (Caddy's default), one client could open about 80 sign-in requests, send only the first byte
of each body, and hold the whole pool, so every other sign-in got `503 SERVER_BUSY` ([GH#283]).

These routes now take their slot only once the body is received. The body must still arrive within
`HTTP_HEADER_READ_TIMEOUT_SECS` (else `408 REQUEST_TIMEOUT`), and the body bytes received on them
share a budget of 64 KiB per `HTTP_MAX_CONCURRENT_REQUESTS` (at least 16 MiB; 32 MiB by default),
counted as they arrive, past which requests get `503 SERVER_BUSY`. A full anonymous pool still
answers `503` at once, without reading the body. No configuration, API or schema change.

Request buffering at the reverse proxy is now defence in depth rather than required for sign-in to
stay available; the [deployment guide][deployment-buffering] still recommends it, because each slow
request otherwise holds a server connection until the timeout.

[GH#283]: https://github.com/Shadoukita/ShadouCMDB/issues/283
[deployment-buffering]: docs/deployment.md#hardening-settings
