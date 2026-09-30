### Changed: A deadlock or serialisation failure returns `503 SERVER_BUSY`

A request that loses a database deadlock or serialisation race used to fail with a generic `500`.
It now returns `503` with the code `SERVER_BUSY` and `Retry-After: 1`, on every route. Clients can
retry the request unchanged.

**Upgrade:** nothing to do.
