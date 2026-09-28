### Security: An API token created during a password reset no longer survives the reset

Creating an API token now waits for a concurrent password change or reset of the token's owner or
of the caller. If that change ended the caller's session, the request fails with `401
UNAUTHENTICATED` and no token is created; otherwise the change revokes the new token. Before, a
token created at the same moment as the reset could stay active. No action is needed on upgrade.
([#143](https://github.com/Shadoukita/ShadouCMDB/issues/143))
