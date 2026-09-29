### Added: API tokens refused for two-factor authentication are marked in the web UI

Administration › API tokens marks a working token that is refused because its owner must use
two-factor authentication ([GH#200]) with a "Refused: owner requires MFA" badge. Its tooltip explains
that a new token must be created from a session signed in with a second factor. The **Refused for
two-factor only** filter (`refusedForMfa=true`, kept in the URL) lists just those tokens, so the
integrations to re-issue after upgrading are easy to find. When creating a token is refused with
`403 MFA_REQUIRED_FOR_TOKEN`, the dialog shows the server's explanation and links to the account page
to set up two-factor authentication.

[GH#200]: https://github.com/Shadoukita/ShadouCMDB/issues/200
