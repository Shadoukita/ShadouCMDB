### Added: users change their own password under My account

**My account** has a new **Password** panel (current password, new password, repeat) for every
signed-in user, so changing your own password no longer needs an administrator with `users.manage`
([GH#128]). It calls `PUT /api/v1/auth/password`, which already existed, checks length and match in
the browser and shows the server's messages next to their fields (for example a wrong current
password). Changing the password keeps the current session and signs the user out on every other
browser and device; it also revokes the user's API tokens (see "a new password revokes the user's API
tokens"). Accounts that sign in through an OIDC provider or LDAP/AD directory see a pointer to that
provider instead of the form.

**Upgrade:** nothing to do. No API or database change. Wrong current passwords count towards the same
per-user lock as sign-in.

[GH#128]: https://github.com/Shadoukita/ShadouCMDB/issues/128
