### Changed: API docs off by default; HTTP timeouts, audit hash chain and SIEM export

Backend hardening ([SHAA-80]):

- **`/openapi.json` and `/docs` are off by default** (`API_DOCS=off`). Set `API_DOCS=authenticated`
  (any signed-in user) or `public` if tools or people read the contract from the server. The contract
  is also in the repository as `backend/openapi.json`.
- Request headers must arrive within `HTTP_HEADER_READ_TIMEOUT_SECS` (default 10), and a whole
  request must be answered within `HTTP_REQUEST_TIMEOUT_SECS` (default 120); a slower one gets
  `408 REQUEST_TIMEOUT` and its transaction is rolled back. Raise the second for very large imports.
- `GET /api/v1/version` reports the build and the number of migrations it expects, without a database.
- Migration `0018_audit_hash_chain` hash-chains every `audit_log` row, existing rows included (in `id`
  order; on a large audit log, allow for it in the maintenance window). `shadoucmdb audit-verify`
  checks the chain and prints its head; after retention runs, use `audit-verify --allow-gaps`.
- `AUDIT_EXPORT` copies every new audit row to stdout, a file or syslog over UDP/TCP for a SIEM
  (default off). `AUDIT_CAPTURE_CLIENT_IP` and `AUDIT_CAPTURE_USER_AGENT` (default true) turn off
  recording the client's IP address or User-Agent where policy rules it out.
- A three-role install that runs `sql/bootstrap/10_split_roles.sql` after upgrading needs this
  release's version of the script (it also locks the API role out of the chain head).

[SHAA-80]: docs/deployment.md#hardening-settings
