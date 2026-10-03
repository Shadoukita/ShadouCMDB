### Changed: The web UI draws its icons from one icon set

Buttons, menus, sort headers, tree toggles, breadcrumbs and import statuses now show line icons
instead of text characters such as `☰ ▾ ▲ ⋯ × ↑ ✓`, so they render the same on every operating
system and font. The icons are a subset of Lucide 1.51.0, bundled with ShadouCMDB: no request goes
to a CDN, they work on air-gapped installations, and the Content Security Policy is unchanged.
The CI class icons are redrawn in the same style. Their stored keys are unchanged, so existing
classes and areas keep their icon. In the sidebar they now take the sidebar's text colour instead
of sitting on a light chip. Lucide is licensed under the ISC licence; the licence text is served at
`/assets/Lucide-LICENSE.txt`, and `THIRD_PARTY_NOTICES` lists it.
