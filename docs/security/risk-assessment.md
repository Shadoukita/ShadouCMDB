# Product risk assessment, v1

Cybersecurity risk assessment of ShadouCMDB under CRA Art. 13(2)–(3). It records what we protect,
from whom, the controls in place, and the risk that remains. It is updated with every feature that
changes the attack surface, after every incident, and at least yearly.

| | |
| --- | --- |
| Version | 1 (2026-09-27) |
| Covers | `main` as of SHAA-79, release line `0.1.x` |
| Author | Mamori (security engineering) |
| Approved by | Shadoukita (product owner) |
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
| Database connection credentials | high | high | high | Full control of the data |
| Release artefacts and signing identity | n/a | critical | medium | Tampering reaches every customer |
| Planned: stored infrastructure credentials (vault) | critical | critical | medium | See [feature requirements](feature-requirements.md) |

## 3. Trust boundaries and data flows

```
 browser / API client ──HTTPS──▶ reverse proxy ──HTTP──▶ shadoucmdb ──TLS──▶ PostgreSQL
        (B1)                         (B2)                  │   (B3)
                                                           └── logs ──▶ customer's log system
 GitHub (source, CI) ──release──▶ GitHub Releases / GHCR ──download──▶ customer host   (B4)
```

- **B1** untrusted users and networks → proxy: TLS, the customer's responsibility.
- **B2** proxy → server: every request is untrusted until a session is validated; forwarded
  headers are trusted only for scheme and audit IP.
- **B3** server → database: the server is the only database client; the database trusts it fully.
- **B4** our build pipeline → customer: supply chain.

## 4. Threats and controls

Risk = likelihood × impact on a 1–3 scale each (1–2 low, 3–4 medium, 6–9 high), after the listed
controls.

| # | Threat (STRIDE) | Controls in place | L | I | Risk | Further treatment |
| --- | --- | --- | --- | --- | --- | --- |
| T1 | Credential stuffing / brute force on sign-in (S) | argon2id, 12+ char passwords, per-user backoff per client network plus a per-user budget across networks, global failure budget with a per-network share of the slow lane, the same argon2 cost for unknown names (also when a directory answers), sign-in audit | 2 | 3 | 6 high | MFA (workstream 5), OIDC/LDAP (6); throttle state per process only; lockout by forged client addresses (see §6) |
| T2 | Session theft or fixation (S) | random tokens hashed at rest, HttpOnly/Secure/SameSite=Lax, idle + absolute timeout, rotation on sign-in, revocation on logout/password change/deactivation | 1 | 3 | 3 medium | — |
| T3 | CSRF on state-changing requests (T) | double-submit CSRF token, SameSite=Lax, strict CORS allowlist, `*` rejected | 1 | 3 | 3 medium | — |
| T4 | Broken access control / IDOR across classes (E, I) | permission declared per route and checked server-side; list, search and graph filtered by class grants | 2 | 3 | 6 high | pentest pass before `v0.1.0` (workstream 8); authz regression tests per route |
| T5 | SQL injection (T, I) | parameterised queries, escaped LIKE wildcards, quoted identifiers | 1 | 3 | 3 medium | Runtime DDL for type tables (SHAA-56) raises likelihood: identifier handling must stay centralised and fuzz-tested |
| T6 | Stored XSS via CI attributes or UI settings (T, E) | Vue escaping, no `v-html`, strict CSP without `unsafe-inline`, uploaded images type-checked (SVGs checked against an element/attribute allowlist) and served with a sandboxing CSP | 1 | 3 | 3 medium | ZAP baseline (workstream 8) |
| T7 | Information disclosure via API docs (I) | none: `/docs`, `/openapi.json` public | 3 | 1 | 3 medium | Config switch, default off/authenticated (workstream 3) |
| T8 | Resource exhaustion (D) | 1 MiB body limit, page size ≤ 200, 30 s statement timeout, connection pool bound | 2 | 2 | 4 medium | HTTP read/request timeouts (workstream 3) |
| T9 | Audit log tampering by a DB-level attacker (T, R) | append-only by application design and triggers | 2 | 2 | 4 medium | Block TRUNCATE, hash chain, syslog/SIEM export (workstream 3) |
| T10 | Database impersonation / sniffing (S, I) | TLS by default (`require`), `verify-full` available | 2 | 3 | 6 high | Recommend `verify-full` (hardening guide); consider making it the default |
| T11 | Compromised dependency (T, E) | lockfiles, `--locked`/`npm ci`, small dependency set, no OpenSSL | 2 | 3 | 6 high | cargo-deny, npm audit, Dependabot, CodeQL (workstream 1) |
| T12 | Tampered release or image (T) | release only from tagged `main` by CI, `SHA256SUMS`, `contents: read` defaults | 2 | 3 | 6 high | cosign signatures, SLSA provenance, SBOM, SHA-pinned actions (workstream 1); [IR plan](incident-response.md) |
| T13 | Compromised maintainer or AI agent account (S, E) | PR workflow, CI gates | 2 | 3 | 6 high | MFA, branch protection, mandatory human review of security changes ([SDL](sdl.md)) |
| T14 | First-run takeover: attacker creates the first admin (E) | setup only while the user table is empty; setup needs a one-time token that only the operator can read (server log, 0600 token file) or set (`SETUP_TOKEN`), dropped once the first admin exists (GH#192); `create-admin` CLI documented | 1 | 3 | 3 medium | Residual: whoever can read the server log during first run; hardening guide first-run section. Wrong tokens are throttled per client network and account-wide like sign-in, and their WARN lines are capped at 10 per minute plus one count line (GH#230), so a flood cannot rotate the token line out of the log; a client with addresses in three networks (one IPv6 host suffices), or one that can forge `X-Forwarded-For` (no overwriting proxy, see T1), can keep web setup locked for every network (15-minute locks, renewed by one wrong token per lock; a restart clears it but 15 requests lock it again) until the first administrator exists: availability of web setup only, use `create-admin`, and keep the setup endpoint unreachable from untrusted networks until setup is done |
| T15 | Forged client IP in audit log (R) | peer IP recorded alongside forwarded IP; documented as evidence, not access control | 2 | 1 | 2 low | — |
| T16 | Sensitive data in logs (I) | no secrets or tokens logged; SQL statements only at `trace` | 1 | 2 | 2 low | — |
| T17 | Data loss / ransomware on the DB (D) | customer's PostgreSQL backups | 2 | 3 | 6 high | Backup guidance (hardening guide); `backup`/`restore` commands (workstream 7) |
| T18 | Privilege misuse by an administrator (E, R) | permission profiles, audit log of all changes | 2 | 2 | 4 medium | Audit export to a system admins don't control (workstream 3) |
| T19 | Malicious configuration file imported (`config.export_import`) to escalate privileges or plant markup (E, T) | needs `config.export_import`, plus `datamodel.manage` for data-model/lookup sections `customization.manage` for UI settings (GH#59) and `profiles.manage` for permission profiles (GH#80); same checks as the admin API; a profile can't grant more than the importer holds; dry run; audited; exports never contain users or passwords | 1 | 3 | 3 medium | Keep import on the admin-API code paths; requirements for CI data import in [feature requirements](feature-requirements.md#import-and-export) |
| T20 | Single-factor sign-in through an OIDC provider to a profile that requires MFA (S, E; GH#131) | per-provider MFA setting, default verify for new providers: `requireMfa` users need `amr`/`acr` proof in the signed ID token, re-checked per request, for sessions and API tokens (a token counts only if created from a session that proved a second factor, GH#200); trust mode explicit, audited (`providerMfa: trusted`) and badged; upgraded providers start in trust | 2 | 3 | 6 high while in trust, 2 low with verify | Operators review upgraded providers and switch to verify ([hardening guide](hardening.md#enterprise-sign-in)); optional end-to-end test against a real IdP |

## 5. Annex I Part I mapping

| Requirement | How it is met / gap |
| --- | --- |
| (a) no known exploitable vulnerabilities on release | CI checks; dependency audit and SAST planned before `v0.1.0` |
| (b) secure by default configuration, reset to original state | secure defaults, forced first-run admin; factory reset planned (workstream 7) |
| (c) security updates, automatic where applicable | patch releases per [support period](support-period.md); no auto-update by design (self-hosted, offline); notification channel `TODO(owner)` |
| (d) protection from unauthorised access | authentication, RBAC, sessions (T1–T4) |
| (e) confidentiality | TLS via proxy and to the DB, hashes at rest; no field-level encryption (vault will need it) |
| (f) integrity | CSRF, validation, audit log; audit hash chain planned |
| (g) data minimisation | stores only what the CMDB needs plus audit data; IP capture switch planned for works councils |
| (h) availability, resilience against DoS | limits and timeouts (T8) |
| (i) minimise impact on other devices/networks | no outbound connections today ([telemetry](telemetry.md)) |
| (j) limit attack surface | single binary, distroless image, no shell; `/docs` switch planned |
| (k) reduce incident impact (exploitation mitigation) | memory-safe language, nonroot/no-capabilities service, least-privilege DB role |
| (l) security-relevant logging and monitoring, with opt-out | audit log and JSON logs; SIEM export and opt-out configurability planned |
| (m) secure deletion of data and settings | decommission wipe planned (workstream 7) |

Part II (vulnerability handling): [SECURITY.md](../../SECURITY.md), [CRA runbook](cra-incident-reporting.md),
[SDL](sdl.md). SBOM planned (workstream 1).

## 6. Residual risk

With the controls in place, the highest residual risks are T1, T4, T10, T11, T12, T13 and T17.
T11–T13 and T4 are addressed by the work required before `v0.1.0` (workstreams 1, 3, 8). T1 is
accepted for `v0.1.0` on the basis that installations run on internal networks behind a proxy, and
reduced by MFA in a later release. T10 and T17 depend on the operator following the hardening
guide.

T1 lockout (GH#187): the sign-in lock is kept per username *and* client network (IPv4 /24, IPv6
/64), so wrong passwords from one network no longer lock the account holder out from another, and
one network gets at most 4 places in the server-wide slow lane. The client network comes from the
forwarding headers, which only a reverse proxy that overwrites them makes trustworthy. Without one,
or with guesses from three or more real networks, an attacker who knows a username can still add up
the per-user budget across networks (15 failures) and lock that name for every network, with the
same backoff up to 15 minutes, and 16 or more networks can still fill the slow lane. IPv6 makes
distinct networks cheap (a single /48 holds 65,536 /64s), and a user on the account holder's own
network (for example behind the same office NAT) still locks them out. Mitigations for
the operator: run behind a proxy that sets `X-Forwarded-For`, rate-limit `/api/v1/auth/*` per client
address there, and keep a break-glass administrator whose username is not guessable. Timing (GH#190):
a name that no local account has and that the directory does not match now costs the same argon2
verify as a wrong local password, but also a directory round trip, so a patient attacker can still
tell local accounts from other names by latency when a directory is enabled; padding every failed
sign-in to a fixed minimum would close that and is not done.

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
