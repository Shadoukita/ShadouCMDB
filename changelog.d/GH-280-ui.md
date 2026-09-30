### Fixed: Two-factor set-up screen: sign in again when the authenticator is already set up

Under `requireMfa`, a session opened with the password alone stays limited even when the user has
already set up an authenticator in another browser ([GH#280]). The set-up screen now says so and
offers **Sign out and sign in with a code** (back to where the user was going), instead of a set-up
the API refuses with `409`. Reloading the set-up screen no longer forgets where the user was going.

[GH#280]: https://github.com/Shadoukita/ShadouCMDB/issues/280
