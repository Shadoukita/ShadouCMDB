### Added: dependent lookup lists (cascading dropdowns)

A lookup list can now depend on another list, e.g. "Model" on "Manufacturer": each model names the
manufacturer it belongs to, and a model field names its manufacturer field. The API then refuses a
model that does not belong to the CI's manufacturer. Retiring a manufacturer retires its models
(refused while CIs still use one of them); deleting it is refused while models belong to it. See
[docs/data-model.md](docs/data-model.md#dependent-lookup-lists).

Migration `0015_lookup_parent_lists` adds nullable columns (`lookup_lists.parent_list_id`,
`lookup_list_values.parent_value_id`, `ci_attribute_definitions.parent_attribute_id`) and their
integrity triggers; existing lists, values, fields and CI values are unchanged. `migrate` applies it
as usual; `verify` now runs 23 checks.

API: the audit log can now be filtered by `entityType=lookup_lists` and `lookup_list_values`. New nullable fields `parentListId`, `parentValueId`, `parentAttributeId` on lookup lists,
values and attribute definitions (responses and create/update bodies), list filters
`parentListId` and `parentValueId`, and new `409 IN_USE` cases. Configuration files are now written
as `formatVersion: 3` (with `parent` / `parentAttribute` keys); versions 1 and 2 are still imported
and leave existing parent links as they are. **Action** only for scripts that parse exported files
and check `formatVersion === 2`: accept 3.

Web UI: lookup lists moved from *Administration › Lookups › Lists* to the new *Administration › Data
model › Dropdowns* (old bookmarks redirect). There a list gets its parent list and each value its parent
value, values can be filtered by parent value, and the attribute dialog picks a lookup field's parent
field. On the CI form a child dropdown stays disabled until its parent field is set, offers only that
parent's values, and is cleared when the parent changes to one that does not offer it.
