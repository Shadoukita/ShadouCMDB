# Security policy

ShadouCMDB stores a map of an organisation's IT estate, so vulnerabilities in it matter. This
file explains how to report a vulnerability, what happens after you do, and which releases get
security fixes. It is our coordinated vulnerability disclosure (CVD) policy under the EU Cyber
Resilience Act (CRA, Annex I Part II).

Operators: the [hardening guide](docs/security/hardening.md) covers secure deployment. The
other security documents are listed in [docs/security](docs/security/README.md).

## Reporting a vulnerability

**Do not open a public issue, pull request or discussion for a vulnerability.**

Report it privately through GitHub:
[report a vulnerability](https://github.com/Shadoukita/ShadouCMDB/security/advisories/new)
(GitHub private vulnerability reporting). This is currently the only reporting channel.

Every installation also serves these contacts at `/.well-known/security.txt` (RFC 9116).

Please include what you can of:

- the affected version (`shadoucmdb --version`) and platform (Linux, Windows, Docker);
- the affected component (API endpoint, UI page, CLI command, release artefact);
- steps to reproduce, or a minimal proof of concept;
- the impact as you understand it: who can do what to whom;
- whether you know of it being exploited, or being public already;
- how you want to be credited, if at all.

Write in English or German. Don't send real customer data, credentials or personal data; if a
proof of concept needs them, describe them instead.

## What happens next

| Step | Target |
| --- | --- |
| We acknowledge your report | within **3 working days** |
| We confirm or reject it, with a first severity rating (CVSS v4.0) | within **10 working days** |
| Fix released for a **critical** or **high** vulnerability | within **30 days** of confirmation |
| Fix released for a **medium** or **low** vulnerability | within **90 days**, or the next regular release |
| Public disclosure | when the fix is released, and no later than **90 days** after your report unless we agree another date with you |

We keep you informed at least every 14 days until the fix ships. If a vulnerability is being
actively exploited, we move faster than the table above, and we are legally required to notify
the authorities (ENISA and the BSI) within 24 hours; see the
[CRA reporting runbook](docs/security/cra-incident-reporting.md). That notification does not
make your report public.

When the fix is released we publish a GitHub security advisory with a description, the affected
and fixed versions, the severity, the impact and how to remediate. We request a CVE for
vulnerabilities in supported releases. We credit you in the advisory unless you ask us not to.

If the vulnerability is in a third-party component we ship (a Rust crate, an npm package, the
container base image), we report it to that component's maintainers as well and coordinate the
disclosure date with them.

## Supported versions

| Version | Security fixes |
| --- | --- |
| `0.1.x` (pre-release, `0.1.0-rc.*`) | Yes, until `0.1.0` is released. Release candidates are for evaluation, not production. |

Support period: **5 years from the release of each major version** (the CRA default). Each release will state its end-of-support date
(month and year) in its release notes, and this table will list it. See
[support period](docs/security/support-period.md) for what "supported" means.

Security fixes are free of charge. Where we can, they ship as their own patch release, separate
from new features, so you can apply them without taking on other changes.

## Scope

In scope: the `shadoucmdb` binary (API, embedded web UI, CLI), the container images at
`ghcr.io/shadoukita/shadoucmdb`, the release archives and their checksums, and the sample
deployment files in `deploy/`.

Out of scope:

- a particular installation's configuration, hosting or network (report those to its operator);
- PostgreSQL itself, the reverse proxy and the operating system, unless ShadouCMDB uses them
  insecurely;
- findings that need an already-compromised host, database or administrator account, unless
  ShadouCMDB makes the compromise worse than it has to be;
- automated scanner output without a demonstrated impact, missing headers on responses that
  don't need them, and denial of service by sheer traffic volume;
- social engineering, and physical attacks.

## Safe harbour

We will not take legal action against, or ask anyone else to act against, research that:

- is on your own installation, or one whose owner has allowed you to test it;
- avoids privacy violations, destruction of data and interruption of service;
- stops and reports as soon as you reach data that isn't yours;
- gives us reasonable time to fix before disclosure, as described above.

If you are unsure whether something is allowed, ask us first through the channels above.

## Contact and responsibility

Security owner (the person accountable for handling reports and CRA notifications):
Shadoukita (product owner, [@Shadoukita](https://github.com/Shadoukita)).
