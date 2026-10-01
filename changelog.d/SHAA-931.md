### Added: User groups API, owner clean-up and configuration export version 5

- **User groups** (`/api/v1/admin/groups`, right *Manage users*): create, rename, describe and
  delete groups, and replace their members (up to 1 000 users). Group names are unique regardless
  of case. Changes need the group's `version`; a stale one is refused with `VERSION_CONFLICT`
  instead of overwriting another administrator's change. Group changes, including who was added
  and removed, are in the audit log under the new entity type *User group*. Groups are identity
  data: they are not part of the configuration export.
- **Deleting a user or a group that owns business services** is allowed. They are removed as
  owner from every service, and each service records the change in its history, with the deleting
  administrator as actor. The delete response gives the number of affected services
  (`affectedServices`), and `GET /api/v1/admin/groups/{id}` gives it in advance
  (`ownedServiceCount`). Administrators who may not view the business service type see no number.
- **Changed:** `DELETE /api/v1/admin/users/{id}` now answers `200` with `{ "affectedServices": n }`
  instead of `204` without a body. Scripts that check for `204` must accept `200`.
- **Configuration export version 5.** The built-in business service type and the service
  membership relationship type are marked with their role (`systemRole`), and so are permission
  grants on that type (`classSystemRole`). An import matches them by role, not by key: an export
  from an installation where the type is called `service` imports into one where it is called
  `business_service` (and the other way round). The type keeps its key and area on the target, and
  the dry run lists the match as a "Matched by role" warning. An import never gives a type a role
  or removes one. Files of versions 1 to 4 still import as before.

**Upgrade:** Migration 0035 adds a version column to the (new, empty) user group table. No
existing data changes.
