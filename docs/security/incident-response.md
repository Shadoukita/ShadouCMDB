# Incident response plan: our own pipeline, accounts and keys

What to do when the systems that build and ship ShadouCMDB are compromised: the GitHub
repository, GitHub Actions, the container registry, a maintainer's account or machine, or a
signing key. Such a compromise can put malicious code on every customer's network, so it is a
**severe incident** under the CRA: run the [CRA reporting runbook](cra-incident-reporting.md)
(Track B, 24 h early warning) in parallel with this plan.

Vulnerabilities reported in the product itself are handled under [SECURITY.md](../../SECURITY.md).

## What we protect

| Asset | Where | Why it matters |
| --- | --- | --- |
| Source code and history | GitHub `Shadoukita/ShadouCMDB` | Everything is built from it |
| CI/CD | GitHub Actions workflows and runners | Builds and publishes releases |
| Release artefacts | GitHub Releases (archives, `SHA256SUMS`) | What customers download |
| Container images | `ghcr.io/shadoukita/shadoucmdb` | What customers pull |
| Signing identity | cosign keyless via GitHub OIDC (planned); any long-lived key if introduced | Proves a release is ours |
| Maintainer accounts | GitHub, GHCR, email, Paperclip | Can change all of the above |
| Tokens | GitHub PATs, `GITHUB_TOKEN`, registry tokens, AI agent credentials | Same |

## Triggers

Start this plan on any of these:

- a release, tag or image we did not publish, or a checksum or signature that doesn't match;
- a commit, workflow change, branch-protection change or new collaborator nobody recognises;
- a GitHub security-log entry, secret-scanning alert or token-leak notification;
- a maintainer's device lost, stolen or infected, or their account phished;
- a compromised third-party GitHub Action or dependency used by our pipeline (e.g. a tag
  moved to malicious code);
- a customer reporting that a download behaves unexpectedly.

## Roles

The **incident lead** is the security owner (Shadoukita (product owner)), or the deputy
(`TODO(owner): deputy`). They coordinate, decide, and keep the timeline. They may delegate
technical work, but not the decision to revoke, unpublish or notify.

## Phase 1: contain (first hour)

Do these fast, even before the extent is known. Every action goes into the incident timeline with
a UTC timestamp.

1. **Freeze releases.** Disable the Release workflow (*Actions → Release → Disable workflow*) and
   stop publishing anything.
2. **Cut off the attacker's access:**
   - revoke every GitHub personal access token, SSH key and OAuth/GitHub App authorization of the
     affected account(s); reset their passwords and MFA; sign out all sessions;
   - remove any collaborator, deploy key, webhook or GitHub App nobody can account for;
   - rotate repository and environment secrets used by workflows (registry tokens, anything in
     *Settings → Secrets*);
   - revoke credentials issued to AI agents working on the repository, and pause those agents
     until the scope is known;
   - for a stolen device: revoke its keys and sessions, and remotely wipe it if possible.
3. **Protect `main`:** re-apply branch protection and required reviews if they were changed; block
   force pushes.
4. **Preserve evidence** before cleaning up: export the organisation/repository audit log and
   security log, the list of workflow runs with their logs, the release and package list with
   timestamps and digests, and `git reflog`/refs from a trusted clone. Store them outside GitHub.

## Phase 2: assess (first day)

Answer, with evidence:

- **Which systems and accounts** were accessed, and since when? Take the earliest suspicious event
  as the start, not the discovery time.
- **Was anything published?** Compare every release asset and image published in the window with
  a rebuild from the tagged source and with `SHA256SUMS`. Builds are not yet bit-for-bit
  reproducible, so a differing rebuild is a lead to investigate (diff the binaries' symbols and
  embedded files), not proof of tampering. For images, compare digests. Once SLSA provenance and cosign signatures
  exist, verify them against the expected workflow identity.
- **Was the source altered?** Review every commit, tag move and workflow change in the window;
  compare `main` with a trusted clone made before the window.
- **Were secrets exposed?** Anything the compromised account or workflow could read counts as
  exposed.

## Phase 3: eradicate and recover

1. **Remove malicious artefacts.** Delete tampered releases and image tags (keep copies as
   evidence). Don't reuse a version number: publish the clean build as a new patch version so no
   customer can confuse the two.
2. **Revert malicious changes** through reviewed PRs, and check that the fix itself was not written
   with a compromised account.
3. **Rotate signing material.** With keyless cosign signing, revoke trust in signatures from the
   compromised window and publish the list of affected digests; with any long-lived key: revoke
   it, generate a new one on a clean machine, publish the new public key through a second channel
   (repository, website, customer notice), and state from which date the old key is no longer
   valid.
4. **Rebuild from a known-good state:** tag a clean commit, re-enable the workflow and release from
   it, with pinned actions. Verify the new artefacts before announcing them.
5. **Tell customers** (see below) what to check and what to install.

## Customer notification

Customers must be told **within 72 h at the latest**, and immediately if they may have installed
something malicious. Include:

- which artefacts and versions are affected, with their SHA-256 / image digests;
- how to check whether they installed one (`sha256sum`, `docker image inspect --format '{{.RepoDigests}}'`);
- what the malicious artefact could do and what to do if they ran it: isolate the host, rotate
  the database password and the credentials of every ShadouCMDB user, review the audit log,
  restore the database from a backup older than the installation if data may have been altered;
- the clean replacement version and how to verify it;
- the advisory ID and where to ask.

Channels: GitHub security advisory, release notes, and
`TODO(owner): customer notification channel`.

## Phase 4: learn

Within two weeks: a written post-incident review (timeline, root cause, what worked, what didn't),
follow-up issues with owners and dates, updates to this plan, the [SDL](sdl.md) and the
[risk assessment](risk-assessment.md). Submit the CRA final report on time.

## Prevention (what makes this plan work)

| Control | Status |
| --- | --- |
| MFA on every maintainer, GitHub and registry account | owner action (SHAA-77) |
| Branch protection on `main`: PR, review, status checks, no force-push | owner action (SHAA-77) |
| Secret scanning and push protection | owner action (SHAA-77); gitleaks in CI planned |
| Workflows with `contents: read` default permissions | in place |
| Third-party actions pinned by commit SHA; Dependabot for actions | planned (workstream 1) |
| Release only from a tag on `main`, by the workflow | in place |
| `SHA256SUMS`; cosign keyless signatures; SLSA provenance; SBOM | checksums in place, rest planned (workstream 1) |
| No long-lived signing keys (keyless OIDC) | planned |
| AI agents without release, admin or production access | policy ([SDL](sdl.md#3-code-review-policy)) |
| Evidence-grade audit logs exported regularly | `TODO(owner)` |
| Yearly exercise of this plan | `TODO(owner)` |
