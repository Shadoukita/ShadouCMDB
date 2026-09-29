### Security: starting an OIDC sign-in no longer stores anything (GH#122)

Anyone could open `GET /api/v1/auth/oidc/{id}/start` without signing in, and each request stored a
pending sign-in under a limit shared by all users; about 17 requests a second from one client were
enough to make OIDC sign-in answer "unavailable" for everybody ([#122]). The pending sign-in (provider,
`state`, `nonce`, PKCE verifier, return path, expiry) now travels in the `shadoucmdb_oidc` cookie
itself, encrypted and authenticated with AES-256-GCM under a key the server generates and keeps in the
new table `cmdb.server_keys`. There is nothing left to fill up. The callback refuses a cookie that was
changed, cut, sealed with another key or is older than 10 minutes (`ssoError=expired`) before it
contacts the provider. Migration 0021 creates `server_keys` and drops `oidc_login_states`.

**Upgrade:**

- Run `shadoucmdb migrate` as usual. A sign-in in progress while you upgrade ends once with
  "expired"; the user clicks the provider button again.
- The API role may only read and add rows in `server_keys`. Installs split with
  `sql/bootstrap/10_split_roles.sql` get this from migration 0021; if you re-run that script, use the
  one from this release.
- Several API processes behind a load balancer share the key through the database; no configuration.
- `server_keys` is not backed up: a restored server generates a new key. To rotate the key, see
  [data model › Soft-delete decisions](docs/data-model.md#soft-delete-decisions) (`DELETE FROM cmdb.server_keys WHERE purpose =
  'oidc_state'` as the owner role, then restart the API).
- Limit anonymous requests per client address at your reverse proxy
  ([deployment › Hardening settings](docs/deployment.md#hardening-settings)).

[#122]: https://github.com/Shadoukita/ShadouCMDB/issues/122
