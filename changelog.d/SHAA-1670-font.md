### Changed: The web UI uses the Inter typeface

The web UI now renders text in Inter 4.1, so tables, forms and figures look the same on Windows,
macOS and Linux. The font is bundled with ShadouCMDB and served from the server's own origin: no
request goes to a font CDN, it works on air-gapped installations, and the Content Security Policy
is unchanged. Where the font cannot be loaded, the UI falls back to the operating system font as
before. Inter is licensed under the SIL Open Font License 1.1; the licence text is served at
`/assets/Inter-LICENSE.txt`, and the new `THIRD_PARTY_NOTICES` file in the repository lists it.
