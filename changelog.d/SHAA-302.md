### Added: multi-line text fields; Notes keep their line breaks

Text attributes have a new validation rule `multiline` (`validation: {"multiline": true}`) that
tells the CI form to edit the value in a multi-line text area and the detail page to show its line
breaks ([GH#109]). Administrators set or clear it when creating or editing a text attribute
(`POST`/`PATCH /api/v1/attribute-definitions`; omitted from responses when false). It is only valid
for `text` attributes (`400` otherwise). Text values were and are stored exactly as sent, line breaks
included. In the class editor the option is **Multiline** on a text attribute; the CI form saves a
multi-line value exactly as typed (indentation and trailing line breaks included), where single-line
text is still trimmed.

**Upgrade:** migration `0019_multiline_notes` sets `multiline` on the **Notes** fields that migration
`0016_core_ci_model` created from the former `notes` column (`notes`, or `notes_<n>` where the key was
taken), identified by that migration's recorded schema change rather than by name; fields created by
administrators are not touched. Each change is in the audit log (actor `migration 0019`). A fresh
install's IT infrastructure template creates **Notes** as a multi-line field. Before this release the
form edited Notes in a single-line input, so saving a CI could drop line breaks from its notes: values
saved that way are not restored.

[GH#109]: https://github.com/Shadoukita/ShadouCMDB/issues/109
