### Security: Disabling an identity provider also stops sign-ins already in progress

An OIDC or directory (LDAP) sign-in that checked its identity provider just before an
administrator disabled it could still open a session once the disable had committed ([GH#250]):
the check happened before the code exchange with the provider or the directory bind, and the
disable found no session to end yet. That session stayed valid, which defeated disabling the
provider as an emergency cut-off. `shadoucmdb identity-providers reset-undecryptable`, which
disables providers too, was affected the same way.

A session (or, with MFA set up, a second-factor step) is now created only while the account's
identity provider still exists and is enabled; the check waits for a disable in progress, and a
disable waits for a session being opened and then ends it. A refused sign-in writes a
`login.failure` audit row with `reason: "provider_disabled"`; a directory sign-in gets
`401 UNAUTHENTICATED`, an OIDC sign-in returns to the sign-in page with `ssoError=unavailable`.
An OIDC sign-in refused because the account itself changed during the sign-in ([GH#209],
`reason: "account_changed"`) now also returns to the sign-in page, with
`ssoError=account_disabled`, instead of showing an error response.

In addition, a session of an account whose identity provider is disabled is no longer accepted
on any request, whether or not it was ended when the provider was disabled.

**Upgrade:** nothing to do; no configuration or schema change.

[GH#250]: https://github.com/Shadoukita/ShadouCMDB/issues/250
[GH#209]: https://github.com/Shadoukita/ShadouCMDB/issues/209
