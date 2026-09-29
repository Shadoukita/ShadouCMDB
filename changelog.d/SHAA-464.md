### Changed: accessibility checks (WCAG 2.1 AA) on the main screens

The main screens are now checked automatically for WCAG 2.1 A and AA issues on every change: sign-in, the
inventory, CI detail and edit pages, the class and attribute editor, users and permission profiles, My account and
the two-factor enrolment step. What is covered is described in [Accessibility][a11y-doc]. Two issues found are
fixed:

- Required form fields are announced as required by screen readers. Before, the red asterisk was read as part of
  the field name (for example "Usernamerequired").
- Links inside explanatory text are underlined, so they no longer rely on colour alone.

[a11y-doc]: docs/accessibility.md
