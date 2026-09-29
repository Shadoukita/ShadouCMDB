### Security: an administrator's password reset also revokes the API tokens the user created for others

An administrator with `users.manage` can create a token owned by another account
(`POST /api/v1/admin/api-tokens` with `userId`). Resetting a compromised administrator's password
revoked their own tokens but not the ones they had created for other accounts, and those were hard
to find ([#145]). Tokens now record who created them (`createdByUserId`), and
`GET /api/v1/admin/api-tokens` takes a `createdBy` filter. An administrator's reset
(`PUT /api/v1/admin/users/{id}/password`) also revokes every working token the user created for
another owner, each with an `update` row in the audit log. A user's own password change
(`PUT /api/v1/auth/password`) revokes only their own tokens, as before.

**Upgrade:** migration 0022 adds `api_tokens.created_by_user_id` and fills it in from each token's
`create` audit row. Tokens whose `create` row was already purged by audit retention, or whose
creator was deleted, show no creator and are not revoked by the creator's reset; revoke them on
the **API tokens** page if needed. A service token created by an administrator stops working when
that administrator's password is reset: create a new one afterwards.

[#145]: https://github.com/Shadoukita/ShadouCMDB/issues/145
