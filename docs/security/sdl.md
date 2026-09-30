# Secure development lifecycle

How ShadouCMDB is designed, written, reviewed, built and released so that it meets the CRA's
essential requirements (Annex I) and stays that way. It applies to every contributor, human or
AI agent. The day-to-day workflow is in [CONTRIBUTING.md](../../CONTRIBUTING.md); this document
adds the security rules on top of it.

Status markers: **(in place)** is enforced today; **(planned)** is committed work, tracked in
SHAA-77, that this document already requires.

## 1. Requirements and design

- **Security requirements before code.** A feature that adds an input channel, an outbound
  connection, stored secrets, a new permission, or a new kind of personal data gets written
  security requirements before implementation starts. Requirements for the features already
  planned are in [feature-requirements.md](feature-requirements.md).
- **Threat model** for those features: data flows, trust boundaries, STRIDE per boundary.
  Recorded in the feature's issue or in the [risk assessment](risk-assessment.md), which is
  updated in the same PR as the feature.
- **Secure by default.** New settings default to the safe value. An unsafe option needs an
  explicit opt-in, a startup warning where it makes sense, and a line in the
  [hardening guide](hardening.md). Anything outbound is off until configured.
- **Design rules** the code already follows and new code must keep: every route declares its
  permission and is checked server-side (deny by default); SQL is parameterised, identifiers
  quoted; no `v-html` or other raw HTML rendering; request bodies are schema-validated; nothing
  secret in logs, URLs or error messages.

## 2. Implementation

- **Languages and libraries:** Rust (backend), TypeScript/Vue (UI). Cryptography only through
  vetted crates (`argon2`, `sha2`, `rustls` with *ring*); no home-made crypto.
- **No secrets in git** (in place, by rule): `.env` is ignored, `.env.example` holds placeholders.
  Secret scanning in CI over the whole history (gitleaks, in place); GitHub push protection
  (repository setting).
- **Dependencies:** added deliberately. For a new dependency, check maintenance, licence, download
  history and whether we need it at all. Lockfiles (`Cargo.lock`, `package-lock.json`) are
  committed and builds use `--locked` / `npm ci` (in place).

## 3. Code review policy

Every change reaches `main` through a pull request. Nobody pushes to `main` directly
(branch protection: planned, repository setting).

**Every PR** needs:

- green CI (below);
- one approving review by someone other than the author.

**Security-relevant PRs** additionally need **an approving review by a human** who has
read the change and **the security owner's (or their deputy's) review**, which may be the same
person. A PR is security-relevant if it touches any of:

- authentication, sessions, cookies, passwords, tokens, MFA, permission checks or profiles;
- cryptography, random number generation, secrets or their storage;
- SQL construction, especially anything that builds identifiers or DDL;
- input parsing of uploaded or imported files, and output encoding in the UI;
- outbound connections (discovery, webhooks, anything that fetches a URL);
- security headers, CSP, CORS, CSRF;
- the audit log;
- CI/CD workflows, release and signing, Dockerfiles, `deploy/`;
- dependency additions or major-version upgrades;
- the security documents in `docs/security/` and `SECURITY.md`.

The author marks such a PR with the `security` label; a reviewer who spots a missing label
adds it. `CODEOWNERS` will require the security owner on the paths above (planned).

**AI-generated code** is held to the same bar and gets no shortcuts:

- AI agents may write code and review code, but **an AI review never counts as the required
  human review** of a security-relevant PR, and an AI agent never merges one.
- The PR description says which parts were AI-generated. The human reviewer reviews them as if a
  new contributor had written them: they check the logic, not only that tests pass.
- AI tools get no production credentials, signing keys or release permissions. Agents work on
  branches with the same CI gates as everyone else.
- Generated code that adds a dependency must name it in the PR description; the reviewer checks
  that the package exists and is the intended one (hallucinated or typosquatted package names).

**Reviewers check** at least: authorization on every new route, input validation, SQL
parameterisation, output encoding, error messages that leak nothing, a regression test for every
security fix, and that the docs match the behaviour.

## 4. Verification

Required on every PR (in place): `cargo fmt`, `cargo clippy -D warnings`, unit and PostgreSQL
integration tests, `openapi --check`, frontend typecheck and build, the smoke suite, Windows and
Docker builds.

Security checks (in place; `.github/workflows/supply-chain.yml`, `codeql.yml`, `dast.yml`):

| Check | Tool |
| --- | --- |
| Known-vulnerable or disallowed-licence dependencies | `cargo deny`, `npm audit` |
| Static analysis | CodeQL (Rust, JavaScript/TypeScript) |
| Secrets in commits | gitleaks; GitHub push protection (repository setting) |
| Dependency updates | Dependabot (cargo, npm, GitHub Actions, Docker) |
| Dynamic testing | OWASP ZAP baseline and API scan against the release binary (`.github/workflows/dast.yml`, rules in `.zap/rules.tsv`) |
| Penetration test | before each major release, results kept ([v0.1.0](pentest-v0.1.0.md)); its automated part runs on every PR (`frontend/e2e/pentest.spec.ts`) |

**Security fixes ship with a regression test** that fails on the old code and passes on the new.

A finding from any of these that is at least *high* blocks the release until it is fixed or a
documented risk acceptance by the security owner exists.

## 5. Build and release

- Releases are built only by `.github/workflows/release.yml` from a tag on `main`; nobody builds
  release binaries by hand (in place).
- `SHA256SUMS` for every artefact (in place). An SBOM (CycloneDX) per release, cosign keyless
  signatures on images and archives, and SLSA provenance (in place, [supply chain](../supply-chain.md)).
- Workflows run with `contents: read` by default and widen permissions per job (in place).
  Third-party actions pinned to a commit SHA, checked in CI (in place).
- Release notes state the end-of-support date and list the security fixes with their advisories.

## 6. Maintenance and vulnerability handling

- Reports are handled per [SECURITY.md](../../SECURITY.md); actively exploited vulnerabilities and
  severe incidents are reported per the [CRA runbook](cra-incident-reporting.md).
- Dependency advisories are triaged within 5 working days: affected or not, and if affected, fixed
  under the targets in SECURITY.md.
- Security fixes are released for every supported line ([support period](support-period.md)).
- A fixed vulnerability gets a public advisory once the fix is released.

## 7. Accounts and access

- MFA on every account with write access to the repository, GitHub organisation, GHCR and any
  signing or registry account (owner action, SHAA-77).
- Least privilege on the repository: write access only for active maintainers; admin for the
  product owner and the security owner.
- Access is reviewed every six months and removed the day someone leaves.
- A compromise is handled by the [incident response plan](incident-response.md).

## 8. Training and records

- Everyone who reviews security-relevant PRs knows the OWASP Top 10 and OWASP API Top 10 and has
  read this document.
- The records the CRA asks for are kept in the repository and on the tracker: this document, the
  risk assessment, PR reviews, CI results, advisories and incident reports. Keep them for ten
  years after the product is placed on the market, or for the support period if that is longer
  (Art. 13(13)).

Owner of this document: the security owner (Shadoukita (product owner)). Reviewed at
least yearly and after every incident.
