### Changed: The web UI uses the IBM Plex typefaces

The web UI now renders text in IBM Plex Sans, and identifiers such as CI idents, host names, IP
addresses, serial numbers, counts and timestamps in IBM Plex Mono, so they line up and read the same
on Windows, macOS and Linux. Both fonts are bundled with ShadouCMDB and served from the server's own
origin: no request goes to a font CDN, it works on air-gapped installations, and the Content Security
Policy is unchanged. Where a font cannot be loaded, the UI falls back to the operating system font.
IBM Plex is licensed under the SIL Open Font License 1.1; the licence texts are served at
`/assets/IBMPlexSans-LICENSE.txt` and `/assets/IBMPlexMono-LICENSE.txt` and listed in
`THIRD_PARTY_NOTICES`.
