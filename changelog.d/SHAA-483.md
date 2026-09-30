### Security: framing, opener and browser-feature headers on every response; layouts are not access control

The server now sends `X-Frame-Options: DENY`, `Cross-Origin-Opener-Policy: same-origin` and a
`Permissions-Policy` that switches off camera, microphone, geolocation, payment, USB, serial and other
device features, on every response ([GH#192]). Until now clickjacking protection came only from the
CSP's `frame-ancestors`, which HTML documents carry but API responses, assets and uploaded logos do
not.

The layout editor and the API documentation now state that a layout's hidden and read-only fields
change what the web UI shows, not who can read or change the data: the API still returns hidden
attributes and accepts writes to read-only ones. Restrict data with permission profiles. No API or
database change.

**Upgrade:** if your reverse proxy adds `Permissions-Policy`, `X-Frame-Options` or
`Cross-Origin-Opener-Policy` (the hardening guide used to suggest `Permissions-Policy`), remove them so
responses do not carry two copies. ShadouCMDB can no longer be shown inside a frame on another page
(an intranet portal or dashboard); open it in its own tab or window.

[GH#192]: https://github.com/Shadoukita/ShadouCMDB/issues/192
