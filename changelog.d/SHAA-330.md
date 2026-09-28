### Security: the audit log no longer shows CI values of classes the reader may not view

`GET /api/v1/audit-log` returned the full item in `oldValue`/`newValue` for every CI and relationship
entry, so a user with **View audit log** (`audit.view`) whose profile limits the classes they may view
could read the attribute values of CIs they get 403 on ([GH#121]). Entries about a CI of a class the
reader may not view, and relationship entries with such an endpoint, now keep their metadata (who,
when, what, which entity) but return `oldValue` and `newValue` as null with the new field
`redacted: true`. In the entries they may see, a reference attribute into a hidden CI keeps only its id,
as on the item endpoints. Administrators and profiles with View on all classes see everything as before.

**Upgrade:** nothing to do. The response gains the `redacted` field; no schema change.

[GH#121]: https://github.com/Shadoukita/ShadouCMDB/issues/121
