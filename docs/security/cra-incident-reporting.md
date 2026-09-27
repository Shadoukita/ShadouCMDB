# CRA incident reporting runbook

What to do, and by when, when ShadouCMDB has an **actively exploited vulnerability** or a **severe
incident**, under Article 14 of the Cyber Resilience Act (Regulation (EU) 2024/2847). These
reporting obligations apply since **11 September 2026**, also to releases published before then.

The clocks run in calendar hours, including weekends and holidays. Start this runbook as soon as
there is a credible indication; you can withdraw an early warning later, but you cannot make up a
missed deadline.

## Roles

| Role | Who | Does |
| --- | --- | --- |
| Security owner | Shadoukita (product owner) | Decides whether a report is due, submits it, owns the timeline |
| Deputy | `TODO(owner): deputy` | Same authority when the owner is not reachable within 4 hours |
| Engineering lead | the maintainer on the affected area | Analysis, fix, release |
| Communications | `TODO(owner)` | Customer notification, advisory text |

Contact sheet (keep current, outside this repository if it contains personal numbers):
`TODO(owner): phone numbers of owner and deputy, ENISA reporting platform account holder, BSI contact`.

## Where reports go

- All three reports are submitted through **ENISA's single reporting platform** (Art. 16). The
  platform forwards them to the CSIRT designated as coordinator for our main establishment,
  **Germany: the BSI** (Bundesamt für Sicherheit in der Informationstechnik), and ENISA receives
  them at the same time.
- Report in English or German.
- Register the account holder and deputy on the platform **before** they are needed:
  `TODO(owner): platform registration done (date, account holders)`.
- If the platform is unavailable, report directly to the BSI through its reporting channel and
  submit to the platform as soon as it is back. Record both.

## Step 0: is it reportable?

| Question | If yes |
| --- | --- |
| Is there reliable evidence that someone exploited a vulnerability **in ShadouCMDB** on a system without the owner's permission? (A proof of concept from a researcher is *not* exploitation.) | **Actively exploited vulnerability**: Track A |
| Did an incident affect, or could it affect, ShadouCMDB's ability to protect the confidentiality, integrity, authenticity or availability of data or functions, or has it led or could it lead to malicious code in ShadouCMDB or on users' systems? Examples: our release pipeline or signing key was compromised; a tampered release or image was published; our repository was altered by an attacker. | **Severe incident**: Track B |
| Neither | Handle under [SECURITY.md](../../SECURITY.md). Voluntary reporting to the BSI/ENISA is possible (Art. 15) but not required |

Only the product counts: an incident on a customer's installation caused by their own
configuration is not ours to report, unless it reveals a vulnerability in ShadouCMDB.

Record the **time of awareness** (UTC) in the incident issue: when the security owner or anyone
acting for us first had a credible indication. All deadlines count from it. Open a private
incident issue (GitHub security advisory draft or a confidential tracker issue); never a public
issue.

## Track A: actively exploited vulnerability

| Deadline | Report | Content |
| --- | --- | --- |
| **24 h** after awareness | **Early warning** | That there is an actively exploited vulnerability in ShadouCMDB; the affected versions if known; the EU Member States where the affected product has been made available, where known |
| **72 h** after awareness | **Vulnerability notification** | Product and versions; the nature of the exploit and of the vulnerability; corrective or mitigating measures taken and those users can take; how sensitive the information is (TLP) |
| **14 days** after a fix or mitigation is available | **Final report** | Description including severity (CVSS) and impact; information about the malicious actor if available; details of the security update or other corrective measures |

## Track B: severe incident

| Deadline | Report | Content |
| --- | --- | --- |
| **24 h** after awareness | **Early warning** | That a severe incident happened; whether it is suspected to be caused by unlawful or malicious acts; the Member States where the product has been made available, where known |
| **72 h** after awareness | **Incident notification** | Nature of the incident; initial assessment; corrective or mitigating measures taken and those users can take; how sensitive the information is |
| **1 month** after the incident notification | **Final report** | Detailed description including severity and impact; type of threat or root cause; applied and ongoing mitigation |

For both tracks: the BSI may ask for an **intermediate report** on progress at any time; answer
it within the deadline they set. Submit what you know by each deadline and say what is still
under investigation; an incomplete report on time beats a complete one late.

## Hour by hour

**0–4 h**
1. Record the time of awareness. Open the private incident issue. Page the security owner and
   deputy.
2. Decide the track (Step 0). If in doubt, treat it as reportable.
3. Contain: for a compromised pipeline or key, follow the [incident response plan](incident-response.md)
   in parallel.

**By 24 h**
4. Submit the early warning (template below). Record the submission time and reference number.
5. Decide whether users must act now (Step "Inform users"). If exploitation is ongoing, warn
   users now with the mitigation you have; don't wait for the fix.

**By 72 h**
6. Submit the notification (template below).
7. Plan the fix and its release across every supported line.

**After the fix**
8. Release the fix, publish the GitHub security advisory and CVE, inform users.
9. Final report: within 14 days of the fix being available (Track A) or one month after the
   notification (Track B).
10. Post-incident review within two weeks: root cause, what the runbook got wrong, follow-up
    issues. Update the [risk assessment](risk-assessment.md).

## Inform users (Art. 14(8))

After becoming aware of an actively exploited vulnerability or a severe incident, inform the
**affected users**, and if appropriate **all users**, about it and about the mitigation and
corrective measures they can take. Do it without undue delay; in practice together with the
72 h notification at the latest, and earlier if they can protect themselves now.

- **Channels:** a GitHub security advisory (machine-readable, with CVE and affected/fixed
  versions; add a CSAF document when available), the release notes, and
  `TODO(owner): customer notification channel (mailing list / customer contacts)`.
- **Content:** what is affected (versions, configurations), what an attacker can do, whether it
  is exploited, what to do now (upgrade to X, or mitigation Y), how to tell whether you were
  affected (log lines, audit entries), and where to ask.
- Don't publish details that help attackers before users can patch; coordinate the timing with
  the BSI if unsure. If we don't inform users in time, the BSI may do it for us.

## Vulnerabilities in third-party components (Art. 13(6))

When we find a vulnerability in a component we ship (a crate, an npm package, the container base
image, a vendored library such as Swagger UI), we:

1. report it privately to the component's maintainers through their security policy (their
   `SECURITY.md`, GitHub private reporting, or security contact), with a reproduction and our
   proposed fix if we have one;
2. record the report (date, channel, reference) in our incident issue;
3. fix or mitigate it in ShadouCMDB without waiting for them (patch, pin, replace or disable the
   affected feature), and release the fix to our users;
4. agree a disclosure date with the maintainers; if they don't respond within 14 days, escalate
   through the ecosystem's security team (RustSec, the GitHub Advisory Database, npm) and the BSI
   if appropriate;
5. if it is actively exploited through ShadouCMDB, it is also a Track A report for us.

## Templates

**Early warning**

```
Product: ShadouCMDB (manufacturer: TODO(owner): legal entity, address)
Type: [actively exploited vulnerability | severe incident]
Time of awareness (UTC): …
Affected versions: … (or "under investigation")
Member States where made available: … (or "unknown; distributed publicly via GitHub")
[Severe incident only] Suspected unlawful or malicious act: [yes | no | unknown]
Summary: one or two sentences.
Contact: security owner, email, phone.
```

**72 h notification**: the early warning, plus:

```
Nature of the vulnerability / incident: …
Nature of the exploit (Track A) / initial assessment (Track B): …
Measures taken by us: …
Measures users can take now: …
Sensitivity: TLP:[CLEAR|GREEN|AMBER|RED]
```

**Final report**: the notification, plus:

```
Severity: CVSS v4.0 vector and score; impact
Root cause / type of threat: …
Malicious actor (if known): …
Fix: fixed versions, advisory ID, CVE, release date; ongoing mitigation
Lessons learned and follow-up actions: …
```

## Records

Keep for at least ten years: the incident issue with the timeline, every submitted report with
its submission time and reference, user notifications, and the post-incident review.

## Exercise

Run a tabletop exercise with this runbook at least once a year and after changing the security
owner. Record the date and findings here.

| Date | Scenario | Findings |
| --- | --- | --- |
| `TODO(owner)` | | |
