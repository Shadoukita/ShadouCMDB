### Security: concurrent wrong passwords no longer get past the sign-in lock

The sign-in throttle checked the lock, verified the password and only then counted the failure, so
requests for one username that arrived at the same time all had their password checked: about 300
parallel guesses got a `401` each and none a `429` ([GH#118]). An attempt is now reserved when it
passes the check and counts as a failure until it finishes. A username admits only as many attempts
at once as it has free failures left (one at a time once it has been locked); further attempts that
arrive meanwhile get `429 RATE_LIMITED` with `Retry-After: 1`. Attempts in progress also count towards
the server-wide budget. This applies to `POST /api/v1/auth/login`, `POST /api/v1/auth/login/mfa` and
every current-password check (`PUT /api/v1/auth/password`, the two-factor routes). See
[login backoff].

**Upgrade:** nothing to do. No schema change. A client that sends several sign-ins for the same user
in parallel (for example a monitoring probe) can now get `429` with `Retry-After: 1`; it should retry
after that delay.

[GH#118]: https://github.com/Shadoukita/ShadouCMDB/issues/118
[login backoff]: docs/api.md#authentication-and-permissions
