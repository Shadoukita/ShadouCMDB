### Changed: The start-up error screen in the new look

When the server does not answer as the application loads, the browser now shows the sign-in card
instead of the bare application name over an inline alert. The heading names the problem (for example
*API unreachable* or *The database is not migrated yet*), the server's message follows in an error
box, and *Retry* spans the card. The foot says who can fix it: a hint for a stopped service or reverse
proxy, for a database the server cannot use, or a general one, and the request id in a monospaced
font when the server sent one. The texts are available in English and German.

No URL, permission or API behaviour changes.
