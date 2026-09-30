### Fixed: Two-factor set-up screen: sign in again when the authenticator is already set up

Under `requireMfa`, a session that did not prove a second factor stays limited even when the user
already has an authenticator ([GH#280]). Setting one up in another browser now ends such sessions
([GH#292]), so this applies only to a session left open by a set-up made before the upgrade. The set-up screen now says so and offers **Sign out and sign in with
a code** (back to where the user was going), instead of a set-up the API refuses with `409`.
Reloading the set-up screen no longer forgets where the user was going.

[GH#280]: https://github.com/Shadoukita/ShadouCMDB/issues/280
[GH#292]: https://github.com/Shadoukita/ShadouCMDB/issues/292
