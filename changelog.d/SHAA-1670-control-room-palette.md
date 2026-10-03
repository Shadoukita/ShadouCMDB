### Changed: New colour palette for the web UI, in light and dark

The web UI has a new colour palette, the first step of its redesign. The light theme uses cool grey surfaces with a teal primary colour. The dark theme uses a deep blue-grey canvas with a cyan primary colour and dark button labels. The navigation sidebar stays dark in both themes, and its default highlight colour is now cyan. Panel and dialog titles are slightly smaller, and corners are tighter. Every text colour, control border and focus ring in both themes meets WCAG 2.1 AA (4.5:1 for text, 3:1 for control borders and focus rings), and a unit test enforces it.

Brand colours set in Customization › Branding still take precedence and are still contrast-checked. Only installations without a custom primary or accent colour see the new defaults. Theme selection is unchanged: each user's own choice, otherwise the administrator's default, which can follow the operating system's light or dark setting.
