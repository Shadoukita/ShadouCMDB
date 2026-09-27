# Security documentation

| Document | For | What |
| --- | --- | --- |
| [SECURITY.md](../../SECURITY.md) | reporters, customers | How to report a vulnerability, our response targets, supported versions |
| [Hardening guide](hardening.md) | operators | TLS reverse proxy, network segmentation, database TLS and roles, backup |
| [Support period](support-period.md) | customers | How long a release gets security fixes, and how they are delivered |
| [Telemetry and offline operation](telemetry.md) | customers, data protection officers | What the product sends where (nothing to us) |
| [Secure development lifecycle](sdl.md) | contributors | How we build, review and release, including the code-review policy |
| [CRA incident reporting runbook](cra-incident-reporting.md) | security owner | The 24 h / 72 h / 14 day / 1 month reporting steps to ENISA and the BSI |
| [Incident response plan](incident-response.md) | security owner, maintainers | What to do when our pipeline, accounts or signing keys are compromised |
| [Product risk assessment](risk-assessment.md) | maintainers, auditors | Assets, threats, controls and residual risk (CRA Art. 13(2)) |
| [Security requirements for planned features](feature-requirements.md) | maintainers | Requirements for discovery, the credential vault, import/export and webhooks, set before they are built |

## Open decisions

Values marked `TODO(owner)` in these documents are waiting on a decision by the product owner
(tracked in SHAA-77):

- a deputy for the security owner;
- the manufacturer's legal entity and address, and the ENISA platform registration;
- a customer notification channel (e.g. a security-announce mailing list);
- the CRA product classification, and acceptance of the residual risk.

Decided in SHAA-77: reports go through GitHub private vulnerability reporting only; the security
owner is Shadoukita (product owner); the support period is 5 years per major version.

Search for `TODO(owner)` to find every place that still needs an answer.
