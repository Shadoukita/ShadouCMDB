### Security: a right password no longer resets the lock while a second factor is due

Turning two-factor authentication off (`DELETE /api/v1/auth/mfa/totp`) and replacing recovery codes
(`POST /api/v1/auth/mfa/recovery-codes`) check the current password and then a code. A right password
cleared the user's failure count before the code was checked, and a password-only route in between
(`PUT /api/v1/auth/password`, `POST /api/v1/auth/mfa/totp`) did the same. Someone holding a user's
session and password could therefore guess the second factor without ever being locked out
([GH#141]). Now, as at sign-in, a right password alone leaves the count alone once MFA is set up; only
a right password together with a right code clears it, and a wrong code counts as a failure. See
[two-factor authentication].

**Upgrade:** nothing to do. No API or schema change.

[GH#141]: https://github.com/Shadoukita/ShadouCMDB/issues/141
[two-factor authentication]: docs/api.md#two-factor-authentication
