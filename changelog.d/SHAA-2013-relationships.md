### Changed: CI record relationships panel and CI picker

The **Relationships** panel of a CI has been tidied up, and the CI picker now looks the same wherever it is used.

- **Direction:** the "This CI…" column starts with an arrow icon: outgoing, incoming, or both ways for a relationship type without a direction. Screen readers announce the direction by name. The separate Direction column is gone.
- **Related CI:** each related CI shows its class icon, and hostnames appear in the data font. A deleted CI has a **Deleted** badge.
- **Remove** is now a small button at the end of the row instead of a red link.
- **Add form:** the CI and relationship fields have the same width, and the notes field takes up the rest of the row.
- **CI picker** (Add relationship, reference fields, workflow conditions): a chosen CI now appears as a filled field with a clear button (×) instead of bold text with a **Change** button. Clearing it puts the cursor back in the search box.
- The empty option of a dropdown, enum or yes/no field now reads **Not set** instead of "— not set —".
