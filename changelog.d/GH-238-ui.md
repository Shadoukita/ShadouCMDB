### Changed: the identity provider form asks for the secret again when the server address changes

In **Administration › Identity providers**, editing the LDAP server URL (scheme, host or port), the
bind DN or the OpenID Connect issuer URL of a provider with a stored secret now opens the service
account password / client secret input, marks it required and says why: "The server address
changed. Enter the bind password again." Typing the stored address back keeps the stored secret.
When the API refuses a save with `secret_required` (for example because another administrator changed
the address in the meantime), the form opens that input and shows the server's message next to it
([GH#238]).

[GH#238]: https://github.com/Shadoukita/ShadouCMDB/issues/238
