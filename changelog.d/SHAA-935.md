### Added: Groups in Administration

- **Administration › Access › Groups** (`/admin/groups`, for users with *Manage users*): a paged
  list with search and sort by name or member count, and a page per group to rename it, describe
  it, add members through a user search and remove them. Search, sort and page are in the address,
  so a view survives a reload and can be bookmarked. When another administrator saved the group in
  the meantime, the save is refused with a notice and the current version can be loaded, instead
  of overwriting their change.
- **Deleting a group** asks first and says how many business services lose the group as owner.
  When your permissions do not let you see business services, it says that the group may own
  services you cannot view.
- **Deleting a user** says in the confirmation that they are removed as owner from their business
  services, and the user list then shows how many services were affected.
