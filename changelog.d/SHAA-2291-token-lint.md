### Changed: Design token check for the web interface stylesheets

The web interface's stylesheets now take every colour, font size, font weight, font family, corner
radius, shadow and stacking order from the design tokens. `npm run lint -w frontend` checks this and
CI runs it on every change: it names the file and line of any literal value. A unit test also checks
that every token a stylesheet reads is defined. Both use Node.js built-ins only and add no dependency.

The last literals are now tokens: the layout editor's layers (grips, handles, readouts, the editor
bar and snap lines), the brand tile letter, the counts on the navigation badges and the dashboard's
attention card. A few small bars and swatches now share one 2 px corner radius instead of 1, 2 or 3 px.
Nothing else changes on screen.

No URL, permission or API behaviour changes.
