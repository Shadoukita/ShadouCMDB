# Product risk assessment, v1

Cybersecurity risk assessment of ShadouCMDB under CRA Art. 13(2)–(3). It records what we protect,
from whom, the controls in place, and the risk that remains. It is updated with every feature that
changes the attack surface, after every incident, and at least yearly.

| | |
| --- | --- |
| Version | 2 (2026-09-30): corrected against the code of `0.1.0-rc.1` (SHAA-782); outbound flows added (B5, B6, T21, T22) |
| Covers | `main` as of `0.1.0-rc.1`, release line `0.1.x` |
| Author | Mamori (security engineering) |
| Approved by | Shadoukita (product owner), version 1; version 2 pending |
| Next review | on the next attack-surface change, or 2027-09 at the latest |

## 1. Product and intended use

ShadouCMDB is a self-hosted configuration management database: one server binary (REST API,
embedded web UI, CLI) or container image, run by the customer on their own infrastructure against
their own PostgreSQL server. Intended users are IT operations staff inside an organisation, signed
in with individual accounts; access is governed by permission profiles. It is intended for
internal networks behind a TLS reverse proxy. Reasonably foreseeable misuse includes exposing it
to the internet, running it without TLS, and connecting as a PostgreSQL superuser; the
[hardening guide](hardening.md) addresses these.

CRA classification: `TODO(owner): confirm classification`. Our working assumption is a
**default-class** product with digital elements (not listed in Annex III or IV), so conformity
assessment by internal control (module A). Review this if the product gains functions listed in
Annex III, for example if it becomes an identity management or privileged access management
system (the planned credential vault makes this worth checking).

## 2. Assets

| Asset | Confidentiality | Integrity | Availability | Notes |
| --- | --- | --- | --- | --- |
| CMDB content: CIs, attributes, relationships | high | high | medium | A map of the customer network: reconnaissance gold for an attacker |
| User credentials (argon2id hashes) and sessions | high | high | medium | Session tokens stored as SHA-256 |
| Audit log | medium | high | medium | Evidence; also personal data (actor, IP, user agent) |
| Permission profiles and data-model definitions | medium | high | medium | Changing them changes who sees what |
| Database connection credentials | high | high | high | The owner role's: full control, including the audit log. The API role's: all data, but no schema or audit-log changes |
| Encryption key; authenticator (TOTP) secrets; OIDC client secrets and directory bind passwords | high | high | medium | Secrets encrypted with AES-256-GCM under a key kept outside the database (migrations 0025, 0026) |
| Release artefacts and signing identity | n/a | critical | medium | Tampering reaches every customer |
| Planned: stored infrastructure credentials (vault) | critical | critical | medium | See [feature requirements](feature-requirements.md) |

## 3. Trust boundaries and data flows

```
 browser / API client ──HTTPS──▶ reverse proxy ──HTTP──▶ shadoucmdb ──TLS──▶ PostgreSQL
        (B1)                         (B2)                  │   (B3)
                                                           ├── logs ──▶ customer's log system
                                                           ├── HTTPS / LDAPS ──▶ OIDC providers, LDAP/AD directories (B5)
                                                           └── syslog udp/tcp ──▶ customer's SIEM (B6)
 GitHub (source, CI) ──release──▶ GitHub Releases / GHCR ──download──▶ customer host   (B4)
```

- **B1** untrusted users and networks → proxy: TLS, the customer's responsibility.
- **B2** proxy → server: every request is untrusted until a session is validated; forwarded
  headers are trusted only for scheme and audit IP.
- **B3** server → database: the server is the only database client. It connects as the API role,
  which cannot change the system schema or alter the audit log; `migrate` and the reset commands
  connect as the owner role, `prune-audit` as the maintenance role.
- **B4** our build pipeline → customer: supply chain.
- **B5** server → identity providers, only when an administrator configures one: OIDC discovery,
  key set and token endpoint over HTTPS; LDAP/AD over LDAPS or StartTLS. The server trusts their
  answers for identity and group membership (T20).
- **B6** server → syslog/SIEM collector, only with `AUDIT_EXPORT=udp://…` or `tcp://…`: audit rows,
  unencrypted (T22).

The server opens no other outbound connection and sends nothing to us ([telemetry](telemetry.md)).

## 4. Threats and controls

Risk = likelihood × impact on a 1–3 scale each (1–2 low, 3–4 medium, 6–9 high), after the listed
controls.

| # | Threat (STRIDE) | Controls in place | L | I | Risk | Further treatment |
| --- | --- | --- | --- | --- | --- | --- |
| T1 | Credential stuffing / brute force on sign-in (S) | argon2id, 12+ char passwords, per-user backoff per client network plus a per-user budget across networks, global failure budget with a per-network share of the slow lane, the same argon2 cost for unknown names (also when a directory answers), sign-in audit | 2 | 3 | 6 high | In place since: TOTP two-factor sign-in and `requireMfa` profiles, OIDC and LDAP/AD sign-in (the operator's choice to enable). Open: throttle state per process only; lockout from many networks (see §6) |
| T2 | Session theft or fixation (S) | random tokens hashed at rest, HttpOnly/Secure/SameSite=Lax, idle + absolute timeout, rotation on sign-in, revocation on logout/password change/deactivation | 1 | 3 | 3 medium | — |
| T3 | CSRF on state-changing requests (T) | double-submit CSRF token, SameSite=Lax, strict CORS allowlist, `*` rejected | 1 | 3 | 3 medium | — |
| T4 | Broken access control / IDOR across classes (E, I) | permission declared per route and checked server-side; list, search and graph filtered by class grants | 2 | 3 | 6 high | [Pentest v0.1.0](pentest-v0.1.0.md) done, its automated part runs on every PR; authz regression tests per route |
| T5 | SQL injection (T, I) | parameterised queries, escaped LIKE wildcards, quoted identifiers | 1 | 3 | 3 medium | Runtime DDL for type tables (SHAA-56) raises likelihood: identifier handling must stay centralised and fuzz-tested |
| T6 | Stored XSS via CI attributes or UI settings (T, E) | Vue escaping, no `v-html`, strict CSP without `unsafe-inline`, uploaded images type-checked (SVGs checked against an element/attribute allowlist) and served with a sandboxing CSP | 1 | 3 | 3 medium | ZAP baseline and API scan run in CI (`dast.yml`) |
| T7 | Information disclosure via API docs (I) | `/docs` and `/openapi.json` off by default (`API_DOCS=off`, 404); `authenticated` serves them to signed-in users only | 1 | 1 | 1 low | — |
| T8 | Resource exhaustion (D) | 1 MiB body limit (64 KiB on sign-in and setup), page size ≤ 200, 30 s statement timeout, connection pool bound, header-read and request timeouts (`HTTP_HEADER_READ_TIMEOUT_SECS`, `HTTP_REQUEST_TIMEOUT_SECS`), concurrency cap with a separate pool for anonymous routes (`HTTP_MAX_CONCURRENT_REQUESTS`) | 2 | 2 | 4 medium | No per-client request rate limit in the server: at the reverse proxy ([deployment](../deployment.md#hardening-settings)) |
| T9 | Audit log tampering by a DB-level attacker (T, R) | append-only triggers that also reject `TRUNCATE`; the API role has only `SELECT` and `INSERT` on it (owner role separate); hash chain over every row, checked by `audit-verify` (0018); export to syslog/SIEM (`AUDIT_EXPORT`) | 1 | 2 | 2 low | Residual: the owner role or a superuser can rewrite rows and recompute the chain; only a comparison with an exported copy shows it, and export is off by default |
| T10 | Database impersonation / sniffing (S, I) | `DATABASE_SSL=verify-full` by default (certificate and host name checked); `require` logs a warning for a non-loopback host | 1 | 3 | 3 medium | Residual: an operator who sets `require` or `disable` |
| T11 | Compromised dependency (T, E) | lockfiles, `--locked`/`npm ci`, small dependency set, no OpenSSL | 2 | 3 | 6 high | In place since: cargo-deny, npm audit, Dependabot, CodeQL, gitleaks in CI (`supply-chain.yml`, `codeql.yml`). They catch known advisories, not a new malicious release |
| T12 | Tampered release or image (T) | release only from tagged `main` by CI, `SHA256SUMS`, `contents: read` defaults, cosign keyless signatures on every release file and the image, SLSA provenance attestations, CycloneDX SBOM, third-party actions pinned by SHA (checked in CI) ([supply chain](../supply-chain.md)) | 1 | 3 | 3 medium | Builds not yet bit-for-bit reproducible; customers must verify signatures for them to help; [IR plan](incident-response.md) |
| T13 | Compromised maintainer or AI agent account (S, E) | PR workflow, CI gates | 2 | 3 | 6 high | MFA, branch protection, mandatory human review of security changes ([SDL](sdl.md)) |
| T14 | First-run takeover: attacker creates the first admin (E) | setup only while the user table is empty; setup needs a one-time token that only the operator can read (server log, 0600 token file) or set (`SETUP_TOKEN`), dropped once the first admin exists (GH#192); `create-admin` CLI documented | 1 | 3 | 3 medium | Residual: whoever can read the server log during first run; hardening guide first-run section. Wrong tokens are throttled per client network and account-wide like sign-in, and their WARN lines are capped at 10 per minute plus one count line (GH#230), so a flood cannot rotate the token line out of the log; a client with addresses in three networks (one IPv6 host suffices), or one that can forge `X-Forwarded-For` (no overwriting proxy, see T1), can keep web setup locked for every network (15-minute locks, renewed by one wrong token per lock; a restart clears it but 15 requests lock it again) until the first administrator exists: availability of web setup only, use `create-admin`, and keep the setup endpoint unreachable from untrusted networks until setup is done |
| T15 | Forged client IP in audit log (R) | peer IP recorded alongside forwarded IP; documented as evidence, not access control | 2 | 1 | 2 low | — |
| T16 | Sensitive data in logs (I) | no secrets or tokens logged; SQL statements only at `trace` | 1 | 2 | 2 low | — |
| T17 | Data loss / ransomware on the DB (D) | customer's PostgreSQL backups; `shadoucmdb backup`/`restore` commands; backup guidance ([hardening guide](hardening.md#backup-and-restore)) | 2 | 3 | 6 high | Depends on the operator keeping offline or immutable copies and testing restores |
| T18 | Privilege misuse by an administrator (E, R) | permission profiles, audit log of all changes, export to a system admins don't control (`AUDIT_EXPORT`, off by default) | 2 | 2 | 4 medium | Operators enable the export ([hardening guide](hardening.md#logging-and-monitoring)) |
| T19 | Malicious configuration file imported (`config.export_import`) to escalate privileges or plant markup (E, T) | needs `config.export_import`, plus `datamodel.manage` for data-model/lookup sections `customization.manage` for UI settings (GH#59) and `profiles.manage` for permission profiles (GH#80); same checks as the admin API; a profile can't grant more than the importer holds; dry run; audited; exports never contain users or passwords | 1 | 3 | 3 medium | Keep import on the admin-API code paths; requirements for CI data import in [feature requirements](feature-requirements.md#import-and-export) |
| T20 | Single-factor sign-in through an OIDC provider to a profile that requires MFA (S, E; GH#131) | per-provider MFA setting, default verify for new providers: `requireMfa` users need `amr`/`acr` proof in the signed ID token, re-checked per request, for sessions and API tokens (a token counts only if created from a session that proved a second factor, GH#200); trust mode explicit, audited (`providerMfa: trusted`) and badged; upgraded providers start in trust | 2 | 3 | 6 high while in trust, 2 low with verify | Operators review upgraded providers and switch to verify ([hardening guide](hardening.md#enterprise-sign-in)); optional end-to-end test against a real IdP |
| T21 | Server-side requests to internal hosts through identity provider settings (SSRF, I; B5) | only the Administrator profile, from a signed-in session (not an API token), adds providers or runs the connection test; OIDC over https only, redirects not followed, 10 s timeout, 1 MiB response cap; LDAP only over LDAPS/StartTLS; `OIDC_ALLOWED_HOSTS` limits OIDC hosts (unset by default) | 1 | 2 | 2 low | Residual: an administrator can make the server connect to an internal host and learn from the error whether it answers; LDAP has no host allowlist. Operators set `OIDC_ALLOWED_HOSTS` and an egress firewall ([hardening guide](hardening.md#outbound-connections)) |
| T22 | Audit rows read or dropped on the way to the SIEM (I, R; B6) | off by default; `stdout`/`file:` sinks open no connection; documentation says to use a local relay with TLS and prefer TCP; the server warns at startup for UDP; the hash chain (`chainSeq`, `rowHash`) shows gaps | 2 | 2 | 4 medium | No TLS for syslog in the server (RFC 5425): consider adding it; until then a relay |

## 5. Annex I Part I mapping

| Requirement | How it is met / gap |
| --- | --- |
| (a) no known exploitable vulnerabilities on release | CI checks, dependency audit (cargo-deny, npm audit), SAST (CodeQL), DAST (ZAP), pentest ([SDL](sdl.md#4-verification)) |
| (b) secure by default configuration, reset to original state | secure defaults (`verify-full`, `/docs` off, every outbound integration off), first-run admin behind a setup token; `shadoucmdb factory-reset` |
| (c) security updates, automatic where applicable | patch releases per [support period](support-period.md); no auto-update by design (self-hosted, offline); notification channel `TODO(owner)` |
| (d) protection from unauthorised access | authentication, RBAC, sessions (T1–T4) |
| (e) confidentiality | TLS via proxy and to the DB, OIDC and LDAP over TLS, hashes at rest; authenticator and identity provider secrets encrypted (AES-256-GCM, key outside the database); gap: audit export over syslog has no TLS (T22) |
| (f) integrity | CSRF, validation, append-only audit log with a hash chain (`audit-verify`) |
| (g) data minimisation | stores only what the CMDB needs plus audit data; `AUDIT_CAPTURE_CLIENT_IP` and `AUDIT_CAPTURE_USER_AGENT` switch off client details |
| (h) availability, resilience against DoS | limits and timeouts (T8) |
| (i) minimise impact on other devices/networks | no telemetry or update check; outbound connections only to the destinations the customer configures: PostgreSQL, OIDC providers (HTTPS), LDAP/AD directories (LDAPS/StartTLS), syslog/SIEM (UDP/TCP); each off until configured, with timeouts ([outbound connections](hardening.md#outbound-connections), T21, T22) |
| (j) limit attack surface | single binary, distroless image, no shell; `/docs` off by default |
| (k) reduce incident impact (exploitation mitigation) | memory-safe language, nonroot/no-capabilities service, three least-privilege DB roles (the server cannot change the schema or the audit log) |
| (l) security-relevant logging and monitoring, with opt-out | audit log and JSON logs; SIEM export (`AUDIT_EXPORT`); opt-out of client details (`AUDIT_CAPTURE_*`) |
| (m) secure deletion of data and settings | `shadoucmdb decommission` ([backup-and-reset.md](../backup-and-reset.md)) |

Part II (vulnerability handling): [SECURITY.md](../../SECURITY.md), [CRA runbook](cra-incident-reporting.md),
[SDL](sdl.md). A CycloneDX SBOM ships with every release ([supply chain](../supply-chain.md)).

## 6. Residual risk

With the controls in place, the highest residual risks are T1, T4, T11, T13, T17 and T20 (in
trust mode). Version 2 lowered T7, T9, T10 and T12 because their planned controls now exist, and
added T21 and T22 for the outbound connections: neither is high, since each is off until
configured and T21 needs an administrator. T4 and T11 remain high on likelihood despite the
pentest and dependency checks. T1 is accepted for `v0.1.0` on the basis that installations run
on internal networks behind a proxy, and is reduced where operators require two-factor sign-in.
T13 depends on repository settings (MFA, branch protection). T17, T21 and T22 depend on the
operator following the hardening guide.

T1 lockout (GH#187): the sign-in lock is kept per username *and* client network (IPv4 /24, IPv6
/64), so wrong passwords from one network no longer lock the account holder out from another, and
one network gets at most 4 places in the server-wide slow lane. The client network is the TCP
peer's, or behind a proxy listed in `TRUSTED_PROXIES` the client address it reports, read right to
left past the listed hops (GH#215), so a forged `X-Forwarded-For` no longer picks the network. With
the list empty behind a proxy, every client shares the proxy's network (one guesser locks a name for
all of them); with guesses from three or more real networks, an attacker who knows a username can still add up
the per-user budget across networks (15 failures) and lock that name for every network, with the
same backoff up to 15 minutes, and 16 or more networks can still fill the slow lane. IPv6 makes
distinct networks cheap (a single /48 holds 65,536 /64s), and a user on the account holder's own
network (for example behind the same office NAT) still locks them out. Mitigations for
the operator: run behind a proxy that sets `X-Forwarded-For` and list it in `TRUSTED_PROXIES`,
rate-limit `/api/v1/auth/*` per client address there, and keep a break-glass administrator whose
username is not guessable. Timing (GH#190, GH#216): a name that no local account has and that the
directory does not match costs the same argon2 verify as a wrong local password, and every refused
sign-in (401) is held to `SIGN_IN_FAILURE_FLOOR_MS` (default 1 s, plus up to 5 % jitter) after the
throttle, so the directory round trip no longer shows in the latency. Residual: a sign-in that takes
longer than the floor (a slow or unreachable directory, an overloaded server) still shows, so the
floor must stay above the directory's worst case; and the answer's status still tells a right
password from a wrong one, as it must.

T20 (GH#131) is residual by design for OIDC providers set to **Trust the provider**: ShadouCMDB
cannot tell whether such a provider used a second factor, so a password-only policy at the
provider still gives a `requireMfa` profile (up to Administrator) on one factor. The choice is
explicit, audited and shown in the UI, and every provider that existed before the upgrade starts
in it until an administrator reviews it. With **Verify**, the remaining risks are inherent to
federation: `amr` and `acr` are the provider's own assertions (a compromised or malicious provider
can claim anything, as it can for the identity itself), a session that loses its exemption is
ended rather than stepped up in place, and there is no end-to-end test against a real provider
in CI.

`TODO(owner)`: the security owner accepts or rejects this residual risk for `v0.1.0`, recorded
here with date and name.
