### Changed: Identifiers in the web UI use JetBrains Mono

Identifiers in the web UI, such as CI idents, IP addresses, serial numbers, technical names and code,
are now set in JetBrains Mono 2.304, so they line up and read the same on every operating system.
Body text stays in the regular UI font. Like Inter, the font is bundled with ShadouCMDB and served
from the server's own origin: no font CDN, it works on air-gapped installations, and the Content
Security Policy is unchanged. Where the font cannot be loaded, the operating system's monospace
font is used as before. JetBrains Mono is licensed under the SIL Open Font License 1.1; the licence
text is served at `/assets/JetBrainsMono-LICENSE.txt` and listed in `THIRD_PARTY_NOTICES`.
