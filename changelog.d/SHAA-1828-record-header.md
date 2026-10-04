### Changed: CI record header

The top of a CI's page is reorganised so the record's identity and its key facts can be read at a glance.

- **Title row:** the class icon in its colour before the name. A name that reads as a hostname (`fra1-esx-01`, `db01.example.com`) is set in the monospace data font. Under the name, one line gives the state (a status dot and "Active", or the Inactive, Deactivates or Deleted badge), the ident, the class (a link to the inventory filtered by it), the criticality and when the CI was last updated.
- **Actions:** Impact analysis, Relationship map and History open those views. **Delete** has moved into the **More actions** (`⋯`) menu, so the most destructive action is no longer the most prominent button. It asks for confirmation as before and lists the relationships that will break.
- **Fact chips:** one `attribute: value` chip per lookup attribute that has a value (status, environment, owner, location and so on), taken from the class's attribute definitions. Each chip opens the inventory filtered by that class and value.
- **Stat tiles:** the number of relationships (outgoing and incoming), the business services the CI is part of, and its changes in the last 30 days with who made the last one. A tile is left out when you may not see its data (business services, or the audit log without the `audit.view` permission). It is never shown as 0 instead.
