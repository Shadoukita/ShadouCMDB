### Security: `requireMfa` now applies to LDAP / Active Directory accounts

A permission profile with `requireMfa` used to be enforced for local accounts only, so a directory
account holding such a profile (for example the built-in Administrator) signed in with its directory
password alone ([GH#120]). Now:

- **Directory (LDAP/AD) accounts** holding a `requireMfa` profile must set up an authenticator app. At
  their next sign-in (and on the next request of a session that is already open) they get the same
  limited session as local users and set it up under **Account › Two-factor authentication**,
  confirming with their **directory password**. From then on a directory sign-in asks for the code
  after the password, like a local one.
- The MFA set-up, turn-off and recovery-code routes confirm a directory account's password against the
  account's own directory entry, under the same per-user lock as local passwords. They answer `409`
  while that directory is disabled and `503 IDENTITY_PROVIDER_UNAVAILABLE` when it cannot be reached.
- **OIDC accounts** are unchanged: the provider runs the second factor. `/auth/me` now reports
  `mfa.required: false` for them, matching what the server enforces.

**Upgrade:** before upgrading, tell the directory users who hold a `requireMfa` profile that they will
be asked to set up an authenticator app at their next sign-in, and that the directory must be
reachable for that. The local break-glass administrator is unaffected. No schema change.

[GH#120]: https://github.com/Shadoukita/ShadouCMDB/issues/120
