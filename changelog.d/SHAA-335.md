### Fixed: the inventory sorts by attributes again (hostname, IP address, serial number, ...)

`GET /api/v1/configuration-items` takes `sort=attributes.<key>` (and `-attributes.<key>` for
descending) when the list is filtered by `classId` ([GH#112], [SHAA-335]). The attribute must be the
same one on every class in `classId` (its own or inherited, so `classId=<hardware>` sorts servers and
network devices by their shared `ip_address`). Text sorts case-insensitively, IP and CIDR by address
(`10.0.0.9` before `10.0.0.10`), numbers and dates by value, lookups by the list's value order; CIs
without a value come last. Reference attributes cannot be sorted on. A sort on an attribute without
`classId`, on an unknown attribute, or on a key that is a different attribute in the classes of
`classId` is a `400` with the field `sort` and the code `class_required`, `unknown_attribute`,
`ambiguous_attribute` or `not_sortable`.

Stored UI settings accept the same field in a list view's `defaultSort` and a saved search's `sort`.
Where the class has no such attribute, the effective settings drop the sort (the list sorts by label)
and report an `unknown_attribute` issue.

In the web UI ([SHAA-347]), **Administration › Customization › List views** offers the class's
attributes (not references) as the default sort, and a saved-search widget under **Dashboard** offers
the attributes every ticked class has. In the inventory of one class, the headers of attribute
columns sort the list; the sort is dropped when the class filter changes.

**Upgrade:** migration 0020 puts back the sorts migration 0016 had turned into label sorts. A list
view's default sort, or the sort of a saved-search widget on one class, that was on `hostname`,
`ipAddress`, `serialNumber` or `statusName` before 0016 becomes `attributes.hostname`,
`attributes.ip_address`, `attributes.serial_number` or `attributes.status` (or the suffixed key 0016
gave the attribute, e.g. `hostname_2`), as a new UI settings version by "migration 0020". A sort
changed since 0016, a saved search on several classes, and a class without the attribute keep their
current sort. Nothing to do otherwise.

[GH#112]: https://github.com/Shadoukita/ShadouCMDB/issues/112
[SHAA-335]: docs/api.md
[SHAA-347]: docs/api.md#customization-and-configuration-exportimport
