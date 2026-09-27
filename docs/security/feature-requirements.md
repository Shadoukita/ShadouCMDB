# Security requirements for planned features

Requirements for features that are planned but not built: **discovery**, the **credential
vault**, **import/export** of CI data, and **webhooks**. They are set now so the design starts
from them ([SDL](sdl.md#1-requirements-and-design)). Each feature's PR must show how it meets them
(tests where the requirement is testable) and update the [risk assessment](risk-assessment.md).

**MUST** is a release blocker for the feature; **SHOULD** needs a written reason if skipped.

## Requirements for all four

- **G1** MUST be off by default; an administrator enables it explicitly. Enabling and every
  configuration change is audited.
- **G2** MUST have its own permissions (e.g. `discovery.manage`, `vault.read`, `vault.manage`,
  `import.run`, `webhooks.manage`), none implied by existing ones except for the built-in
  Administrator profile. Routes declare them like every other route.
- **G3** MUST treat everything it receives (scan results, agent reports, files, webhook responses)
  as untrusted input: schema-validated, size-limited, and never interpreted as code, SQL, shell or
  HTML.
- **G4** MUST NOT log secrets, credentials, tokens or full payloads; log identifiers and outcomes.
- **G5** MUST be rate-limited and bounded (concurrency, time, size) so it cannot exhaust the
  server or the database.
- **G6** Outbound connections MUST be listed in [telemetry.md](telemetry.md) and the
  [hardening guide](hardening.md) when the feature ships.

## Discovery

Scanning customer networks, or agents on hosts reporting into ShadouCMDB.

**Network scanning from the server**

- **D1** MUST scan only targets in administrator-defined allowlisted ranges; the default is none.
  Reject ranges wider than a configured limit (e.g. /16) unless explicitly overridden.
- **D2** MUST support excluding ranges and hosts, and MUST never scan the server's own loopback,
  link-local (`169.254.0.0/16`, `fe80::/10`) or cloud metadata addresses unless explicitly
  allowlisted.
- **D3** MUST rate-limit probes per target and globally, so a scan cannot become a denial of service
  on the customer's network.
- **D4** Scan credentials (SSH, WinRM, SNMPv3, APIs) MUST come from the credential vault, never from
  plain configuration, and MUST be usable only by the discovery job they are assigned to.
- **D5** SSH MUST verify host keys (trust on first use at most, with alerts on change); WinRM MUST use
  HTTPS with certificate validation; SNMP MUST be v3 with authPriv (v1/v2c only by explicit opt-in).
- **D6** Scanning MUST run in a separate process or service with its own, minimal OS privileges,
  not in the web server process (raw sockets need capabilities the web server must not have).

**Agents**

- **D7** Agents MUST authenticate with mutual TLS: per-agent client certificates issued at
  enrolment, short-lived (≤ 90 days) with automatic renewal, revocable individually.
- **D8** Enrolment MUST use a single-use, expiring token; the server MUST show the agent's
  certificate fingerprint for approval, or pin it on first contact.
- **D9** An agent MUST only be able to report for its own host (bound to its certificate identity),
  never read CMDB data or report for other hosts.
- **D10** Agents MUST run with least privilege on the host, and their updates MUST be signed and
  verified before install.
- **D11** Discovered data MUST go through the same validation and audit as API writes, with the
  agent or job as the actor, and MUST NOT overwrite manually maintained fields unless the class is
  configured to allow it.

## Credential vault

Storing credentials for discovery and integrations. This moves ShadouCMDB close to privileged
access management; check the CRA classification (see [risk assessment](risk-assessment.md#1-product-and-intended-use)).

- **V1** Secrets MUST be encrypted at rest with an AEAD (AES-256-GCM or XChaCha20-Poly1305) from a
  vetted library, with a unique random nonce per encryption and the record ID and version as
  associated data (so ciphertexts cannot be swapped between records).
- **V2** Envelope encryption: a per-installation data key, wrapped by a master key that is NOT in
  the database: from a file or environment variable at minimum, and SHOULD support an external KMS
  (HashiCorp Vault, AWS/Azure/GCP KMS) or an HSM via PKCS#11. A database dump alone MUST NOT reveal
  any secret.
- **V3** MUST support master-key rotation (rewrap without downtime) and document the procedure.
- **V4** Secrets MUST be write-only through the API and UI: they can be set, replaced and deleted,
  never read back. Only server-side consumers (discovery jobs) decrypt them, in memory, for the
  duration of use.
- **V5** Every use (decryption) and every change MUST be audited with actor, credential ID and
  purpose; never the value.
- **V6** Decrypted secrets SHOULD be held in zeroising memory types and MUST NOT appear in logs,
  error messages, panics, core dumps or API responses.
- **V7** Access MUST be scoped: a credential is bound to the discovery jobs or integrations allowed
  to use it; `vault.manage` does not imply seeing values.
- **V8** Backups: the hardening guide MUST explain that the master key is backed up separately from
  the database, and what happens if it is lost.

## Import and export

CSV (and similar) import and export of CI data. The configuration import/export that exists
today (`config.export_import`, JSON) already follows I3–I5 and I9; keep it that way.

**Import**

- **I1** MUST parse with a strict, well-tested parser; reject files over a configured size and row
  count, and rows over a column/length limit.
- **I2** If any XML-based format is accepted (XLSX, XML), external entities, DTDs and XInclude MUST
  be disabled (no XXE), and decompression MUST be bounded (zip bombs).
- **I3** MUST apply the importing user's permissions per class and per operation: an import can do
  nothing the user couldn't do through the API.
- **I4** MUST run through the same validation and business rules as the API, in a transaction, with
  a dry-run mode that reports what would change.
- **I5** Every created or changed row MUST be audited with the importing user and a common import ID.
- **I6** Parsing MUST NOT execute anything: no formulas, macros or embedded objects are evaluated.

**Export**

- **I7** MUST export only rows and attributes the user may read (same filtering as list/search).
- **I8** CSV export MUST neutralise formula injection: prefix cells that start with `=`, `+`, `-`,
  `@`, tab or carriage return with a `'` (OWASP CSV injection guidance), and always quote fields.
- **I9** MUST NOT export password hashes, session data, vault secrets or other credentials.
- **I10** Exports SHOULD be streamed with `Content-Disposition: attachment` and `no-store`, and MUST
  be audited (who exported what, when). Large exports SHOULD run as background jobs with size
  limits.

## Webhooks

The server calling URLs registered by administrators when CIs change.

- **W1** SSRF protection MUST be applied to every request, including after redirects: resolve the
  host name, and refuse loopback, private (RFC 1918, `fc00::/7`), link-local, cloud metadata
  (`169.254.169.254`, `fd00:ec2::254`), multicast and unspecified addresses unless an
  administrator allowlists the range. Connect to the resolved, checked address (no second DNS
  lookup that could rebind).
- **W2** HTTPS MUST be required by default, with certificate validation; plain HTTP only by explicit
  per-webhook opt-in. Redirects SHOULD NOT be followed; if they are, W1 applies to each hop, and
  at most 3.
- **W3** Every delivery MUST be signed: HMAC-SHA256 over timestamp and body with a per-webhook secret,
  sent in a header with the timestamp so receivers can reject replays. Secrets are generated by
  the server, shown once, stored in the credential vault (or at least encrypted as in V1) and
  rotatable with an overlap period.
- **W4** Payloads MUST contain only data the webhook's owner may read at delivery time; the
  webhook runs with a service identity whose permissions are set when it is created, and
  deliveries stop if that identity loses access.
- **W5** Timeouts (connect ≤ 5 s, total ≤ 10 s), bounded retries with backoff, a response-size limit,
  and a per-webhook and global rate limit. The response body is not stored beyond a short,
  truncated excerpt for troubleshooting.
- **W6** Deliveries MUST run in a background worker, never in the request that triggered the change,
  so a slow receiver cannot slow the API.
- **W7** Creating, changing, disabling a webhook and each failed delivery MUST be audited. The UI MUST
  NOT display the secret after creation.
