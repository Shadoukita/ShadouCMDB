### Added: notes and built-in panels in the layout editor and the designer

The layout editor window and **Customization › Detail and form layout** place layout content blocks
([layout format]): **+ Note** adds static text (plain text or limited Markdown: bold, italic, code, links and
lists; HTML is shown as text and only http, https and mailto links are kept), and **+ Panel** places the
detail page's Relationships, History or Audit trail panel in any tab, each once per layout. The detail
page shows them where the layout puts them; a panel the layout does not place keeps its usual position.
The CI form shows notes but not panels. History and Audit trail sections are shown only to users with
`audit.view`. When the API refuses a save, its messages about a section (for example
`settings.layouts.0.tabs.1.sections.0.kind`) are listed in that section.

[layout format]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2
