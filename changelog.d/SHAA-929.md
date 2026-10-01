### Added: Business service type, membership and owners in the database

The database side of business services (the API and screens follow in later changes):

- A built-in type **Business service**. It cannot be deleted, archived, made abstract or given a
  parent type, and no type can be created below it. You can still rename it, change its
  description, colour and icon, and add your own fields.
- A built-in relationship type **Service member** ("includes" / "is part of"). It records which CIs
  a business service includes, and a failing member affects its services in impact analysis. Its
  names and labels can be changed. It cannot be deleted, and its key, direction, impact direction
  and active flag are fixed. Any type of CI can be a member. A service may include other services:
  loops are refused, and so are chains more than 8 levels deep.
- New tables for user groups and for the technical and business owners of a service (users or
  groups). Both are in backups. Deleting a user or a group removes their ownerships.

**Upgrade:** No existing data changes. If your installation has the starter type *Service* (key
`service`, with the *Service tier* field, no parent type and no subtypes), that type becomes the
business service type: same table, same CIs, same permissions. Otherwise a new, empty type
*Business service* (key `business_service`) is created in a new area *Business services*.
Profiles that grant all types can see it straight away. Profiles with individual type grants need a
grant for it. On a new installation, installing the IT infrastructure starter template adds the
template's service fields to *Business service* and does not create a second service type.
Rolling back to 0.2.x after the upgrade is not supported: restore the backup you took before
upgrading.
