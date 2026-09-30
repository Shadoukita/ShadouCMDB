### Added: sign-in through OIDC providers and LDAP/AD directories

Users can now sign in through OpenID Connect providers (Entra ID, Okta, Keycloak, ADFS, …) and
LDAP / Active Directory directories (LDAPS or StartTLS, certificates always verified), with the
provider's groups mapped to permission profiles. Local accounts keep working as break-glass
access. Migration `0014_enterprise_sign_in` adds the tables; `migrate` applies it as usual.
Holders of the Administrator profile set providers up under **Administration › Identity
providers** (settings, group mappings, a connection test); the sign-in page shows a "Sign in with
…" button per enabled OIDC provider.

**Action** only if you want OIDC sign-in: set `PUBLIC_URL` to the address users open the web UI
at (e.g. `https://cmdb.example.com`) and register `{PUBLIC_URL}/api/v1/auth/oidc/callback` at the
provider. Allow outbound connections from the server to the provider or the directory.
