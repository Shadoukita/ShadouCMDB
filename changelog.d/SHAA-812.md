### Fixed: the usage report of a type or field names only what blocks its purge

`GET /api/v1/ci-classes/{id}/usage` marked a type's CIs, fields and relationship rules as `blocking` and
answered `inUse: true` for any type with CIs, although `DELETE` on a type only archives it and the purge
removes all of them (SHAA-812). Only subtypes and reference fields of other types stop a type's purge,
so only those counts are `blocking` now, and `inUse` is true exactly when the purge would answer
`409 IN_USE`. A field's report (`/api/v1/attribute-definitions/{id}/usage`) gains the count
`dependentFields`, the fields using it as their parent field, which is what refuses the field's purge.

Every usage report gains the field `removal`: `delete` when `DELETE` removes the record, `purge` for types
and fields, which `DELETE` only archives. `inUse` and `blocking` refer to that operation. No field was
removed or renamed; clients that treated a type's `inUse` as "has CIs" should read the
`configurationItems` count instead.

**Upgrade:** nothing to do.
