### Security: records in classes you cannot view answer 404, and `/usage` needs `datamodel.manage`

A user without `view` on a CI's class could tell that CI exists, and count a class's CIs, without
reading any of its values ([#123], [SHAA-332]). Now:

- `GET/PATCH/DELETE /api/v1/configuration-items/{id}` and `…/{id}/graph` answer `404 NOT_FOUND` for a CI
  in a class the caller may not view, the same response as for an id that does not exist. The same goes
  for `/api/v1/relationships/{id}` when either endpoint is hidden, and `POST /api/v1/relationships`
  reports such a CI with the `not_found` field error of a missing one. `403` remains for CIs the caller
  can see but lacks the edit or delete right on.
- `GET …/{id}/usage` on CI classes, fields, lookups, relationship types and rules needs
  `datamodel.manage`. The data model screens only call it before a delete, which needs the same
  permission.

**Upgrade:** nothing to do. API clients that treated `403` on a CI or relationship id as "exists but
hidden" now get `404`.

[#123]: https://github.com/Shadoukita/ShadouCMDB/issues/123
[SHAA-332]: docs/api.md#authentication-and-permissions
