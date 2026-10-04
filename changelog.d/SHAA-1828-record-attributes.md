### Changed: CI record sections and save bar

The fields of a CI's page, its edit page and the create page now share one look.

- **Sections:** only a section the class layout marks as collapsed can be opened and closed, with a chevron button on its heading. Every other section is a plain panel, so nothing looks collapsible that is not. A collapsed section opens by itself when a field in it has an error.
- **Fields:** labels are set in secondary text above the value. A value you cannot change (a read-only or managed field, a field of a class you may not edit, a deleted CI) is shown as plain text where the input would be, rather than in a grey box that looked like a disabled input. Archived fields and values not defined by the class use the same layout.
- **Save bar:** docked to the bottom of the page while you scroll. It says how many fields were changed, with **Discard** and **Save** on the right. The edit and create pages use the same bar, with **Cancel** and **Save changes** or **Create …**.
- **Edit and create pages:** the same header as the CI's page: the class icon, the title, and a line with the class, the ident, the version and the last update. On the create page the title names the chosen class, and permission messages appear in the class panel.
