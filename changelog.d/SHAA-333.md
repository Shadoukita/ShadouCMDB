### Changed: a new password revokes the user's API tokens

Setting a password, by an administrator (`PUT /api/v1/admin/users/{id}/password`) or by the user
(`PUT /api/v1/auth/password`), now also revokes every API token the user owns that still works, as it
already ended their sessions ([#124]). A token created with a stolen password no longer outlives the
reset. Each revocation is an `update` row for the token in the audit log; `revokedBy` names whoever set
the password.

**Upgrade:** scripts that use a token of an account whose password is changed need a new token
afterwards. No schema change.

[#124]: https://github.com/Shadoukita/ShadouCMDB/issues/124
