### Added: Multi-factor authentication setting for OpenID Connect providers in the web UI

Administration › Identity providers shows the OIDC MFA setting from [GH#131] on each OpenID Connect
provider: **Verify from the sign-in token (recommended)**, with an optional list of required ACR
values, or **Trust the provider without checking**, which shows a warning that permission profiles
requiring MFA then depend entirely on the identity provider's policy. The provider list and the
provider page mark trusted providers with a "MFA not verified" badge, so the providers still to
review after upgrading are easy to find. The sign-in page explains a refused sign-in
(`ssoError=mfa_not_enforced`): the identity provider did not confirm a second factor.

[GH#131]: https://github.com/Shadoukita/ShadouCMDB/issues/131
