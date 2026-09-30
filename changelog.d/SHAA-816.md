### Fixed: two-factor and password-reset texts in user administration match what the server does

The **Require two-factor authentication** hint on the permission profile page said API tokens are not
affected. They are: the server refuses API tokens whose owner holds such a profile, unless the token
was created from a session that completed two-factor sign-in. The hint now says so.

The **Reset password** panel on a user's page said that resetting your own password signs you out.
Your current session stays signed in and only your other sessions are ended. The panel now says so,
and also states that a reset revokes the user's API tokens. No API or database change.
