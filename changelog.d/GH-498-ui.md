### Changed: The web UI asks for your password before user-management changes after 10 minutes

When you save a change to users, permission profiles, API tokens or identity providers, or import a
configuration, more than 10 minutes after signing in, the web UI now opens **Confirm your
password** ([GH#498]). Enter your password, and a code from your authenticator app (or a recovery
code) once two-factor authentication is set up, then save your change again; for the next 10 minutes
you are not asked again. Accounts that sign in through an OIDC provider have no password here: the
dialog offers **Sign in again** and brings you back to the same page.

[GH#498]: https://github.com/Shadoukita/ShadouCMDB/issues/498
