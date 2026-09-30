# Supply-chain security

What CI checks on every change, what every release carries, and how to verify a download or an
image before you run it.

## Checks on every pull request and on `main`

| Check | Workflow | Fails when |
| --- | --- | --- |
| **cargo-deny** ([`backend/deny.toml`](../backend/deny.toml)) | `supply-chain.yml` | a Rust crate has a RustSec advisory (vulnerable, unsound, unmaintained) or is yanked; a licence is not on the allow list; a crate comes from anywhere but crates.io |
| **npm audit** | `supply-chain.yml` | `package-lock.json` or `tools/sbom/package-lock.json` has a high or critical advisory; `npm audit signatures` finds a package whose registry signature or provenance does not verify |
| **gitleaks** | `supply-chain.yml` | a secret (key, token, password, private key) is anywhere in the git history. Findings are redacted in the log |
| **Dependency review** (PRs only) | `supply-chain.yml` | the PR adds a dependency with a known high or critical advisory. Needs the repository's Dependency graph: while that is off, the job passes with a *Dependency review skipped* warning and cargo-deny and npm audit remain the advisory gate |
| **Actions pinned** ([`check-actions-pinned.py`](../.github/scripts/check-actions-pinned.py)) | `supply-chain.yml` | any `uses:` in `.github/workflows` or `.github/actions` (any YAML form, reusable workflows included) refers to a tag, branch or short SHA instead of a 40-character commit SHA. Local `./` actions and `docker://` images pinned by `sha256` digest are allowed |
| **SBOM** | `supply-chain.yml` | the CycloneDX SBOM cannot be generated or does not validate. The SBOM of each build is kept as the `sbom` artifact for 30 days |
| **CodeQL** (Rust, JavaScript/TypeScript, GitHub Actions; `security-extended`) | `codeql.yml` | results appear under *Security → Code scanning* and on the PR. They block a merge once code scanning is a required check (below) |

`supply-chain.yml` also runs every morning and CodeQL every Monday, so an advisory published after
a merge fails the next run without a code change. Dependabot ([`.github/dependabot.yml`](../.github/dependabot.yml))
opens update PRs weekly for cargo, the main npm workspace, GitHub Actions (SHA and version
comment) and the Docker base images (pinned by digest), and monthly for the npm dependencies of
the SBOM tooling (`tools/sbom`).

Every workflow keeps `permissions: contents: read` at the top. A job that needs more asks for it
itself: code scanning upload (`security-events: write`), signing (`id-token: write`,
`attestations: write`), the image push (`packages: write`) and the GitHub Release
(`contents: write`, only on a tag). Checkouts do not keep the token in `.git/config`
(`persist-credentials: false`).

### When a check fails

- **RustSec advisory:** update the crate (`cargo update -p <crate>`). If there is no fix and the
  vulnerable code path is not reachable, add the id to `[advisories] ignore` in `deny.toml` with a
  reason and the tracking issue. Remove it when a fix ships.
- **Licence:** a new licence is only added to `deny.toml` after someone has checked it is
  compatible. Copyleft (GPL, AGPL, SSPL) does not go in.
- **npm audit:** `npm audit fix`, or bump the parent package. There is no ignore list; if an
  advisory cannot be fixed and does not apply, raise it in the PR.
- **gitleaks:** treat a real secret as leaked even after the commit is gone. Rotate it first,
  then remove it from the history. A false positive goes in `.gitleaksignore` (the fingerprint
  gitleaks prints) with a comment.
- **Unpinned action:** look up the commit of the release tag (`git ls-remote --tags
  https://github.com/<owner>/<repo> <tag>`, use the `^{}` line for annotated tags) and write
  `uses: owner/repo@<sha> # <tag>`.

Run the same checks locally:

```sh
cd backend && cargo deny --locked check        # cargo install --locked cargo-deny
npm audit --audit-level=high
gitleaks git --redact .
.github/scripts/check-actions-pinned.py        # needs PyYAML (python3-yaml)
```

## What a release carries

A `v<version>` tag runs [`release.yml`](../.github/workflows/release.yml). On top of the archives
and `SHA256SUMS` ([deployment.md](deployment.md#release-downloads)), the GitHub Release has:

| Asset | What it is |
| --- | --- |
| `shadoucmdb-<version>.cdx.json` | CycloneDX SBOM of everything in the binary: every Rust crate for all shipped targets and the web UI's runtime npm packages. [`.github/scripts/sbom.sh`](../.github/scripts/sbom.sh) makes it |
| `<file>.sigstore.json` | A cosign keyless signature for each archive, the SBOM and `SHA256SUMS`: signature, short-lived Fulcio certificate naming the release workflow and tag, and the Rekor transparency-log entry |
| `shadoucmdb-<version>.provenance.jsonl` | SLSA v1 build provenance for the archives and the SBOM (GitHub artifact attestation, also listed under the repository's *Attestations*) |

The image `ghcr.io/shadoukita/shadoucmdb:<version>` is signed with cosign by digest. It carries
the same SBOM as a signed CycloneDX attestation, a GitHub SLSA provenance attestation, and
BuildKit's own provenance (`mode=max`) and SPDX SBOM.

No key is stored anywhere. Signing uses the GitHub Actions OIDC token of the release run, so a
signature proves the file came from `release.yml` in this repository at that tag. The release
workflow checks every signature and attestation with the commands below before it publishes, and
it builds without caches so a poisoned cache cannot end up in a signed binary.

Same-repository pull requests that touch the pipeline also sign and attest their `-dev` archives,
which tests the signing path. Those certificates name `refs/pull/<n>/merge`, not a `v*` tag, so they
fail the checks below.

## Verifying a download

You need [cosign](https://docs.sigstore.dev/cosign/system_config/installation/) 2.4 or newer and,
for provenance, the [GitHub CLI](https://cli.github.com/). Download the archive, its
`.sigstore.json` and `SHA256SUMS` from the release. Then:

```sh
VERSION=1.2.0
FILE=shadoucmdb-$VERSION-linux-x64.tar.gz

# 1. Signature: made by the release workflow for exactly this tag.
cosign verify-blob "$FILE" --bundle "$FILE.sigstore.json" \
  --certificate-identity "https://github.com/Shadoukita/ShadouCMDB/.github/workflows/release.yml@refs/tags/v$VERSION" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com

# 2. Provenance: built by that workflow from that tag.
gh attestation verify "$FILE" --repo Shadoukita/ShadouCMDB \
  --signer-workflow Shadoukita/ShadouCMDB/.github/workflows/release.yml --source-ref "refs/tags/v$VERSION"
#    Offline, with the file from the release: add --bundle shadoucmdb-$VERSION.provenance.jsonl

# 3. Checksum (also signed: verify SHA256SUMS.sigstore.json the same way as in step 1).
sha256sum --ignore-missing -c SHA256SUMS
```

`Verified OK` from cosign and `Verification succeeded!` from gh mean the file is genuine. Anything
else: do not run it, and report it as described in
[SECURITY.md](../SECURITY.md#reporting-a-vulnerability).

On Windows, `Get-FileHash shadoucmdb-<version>-windows-x64.zip -Algorithm SHA256` gives the value
to compare with `SHA256SUMS`, and cosign and gh work the same in PowerShell.

## Verifying the image

```sh
VERSION=1.2.0
IMAGE=ghcr.io/shadoukita/shadoucmdb:$VERSION
ID=(--certificate-identity "https://github.com/Shadoukita/ShadouCMDB/.github/workflows/release.yml@refs/tags/v$VERSION"
    --certificate-oidc-issuer https://token.actions.githubusercontent.com)

cosign verify "${ID[@]}" "$IMAGE"                                        # signature
gh attestation verify "oci://$IMAGE" --repo Shadoukita/ShadouCMDB \
  --signer-workflow Shadoukita/ShadouCMDB/.github/workflows/release.yml   # SLSA provenance

# The SBOM, from its signed attestation:
cosign verify-attestation --type cyclonedx "${ID[@]}" "$IMAGE" \
  | jq -r .payload | base64 -d | jq .predicate > shadoucmdb.cdx.json
```

Pull by the digest that `cosign verify` printed (`ghcr.io/shadoukita/shadoucmdb@sha256:…`), not by
tag, so you run what you verified. In Kubernetes, the Sigstore
[policy-controller](https://docs.sigstore.dev/policy-controller/overview/) or Kyverno's
`verifyImages` can enforce the same identity and issuer at admission.

## Using the SBOM

The SBOM is plain CycloneDX 1.7 JSON. Scan it for vulnerabilities without the binary, e.g.
`grype sbom:shadoucmdb-$VERSION.cdx.json` or `osv-scanner --sbom=shadoucmdb-$VERSION.cdx.json`, or
upload it to Dependency-Track. It lists the web UI's runtime packages and their peers, not the
build-only devDependencies.

## Repository settings (owner)

These are not in code. Turn them on under *Settings*:

- *Code security*: Dependency graph, Dependabot alerts, Dependabot security updates, secret
  scanning with push protection, private vulnerability reporting. CodeQL uses the workflow above,
  so leave *default setup* off.
- *Rules → Rulesets* for `main`: require a pull request and the status checks from **CI**,
  **Rust**, **Supply chain** and **CodeQL**, plus a code scanning rule that blocks on high or
  higher security alerts. Block force-pushes and deletions.
- A tag ruleset for `v*` so only maintainers can create or move release tags. Anyone who can push
  a `v*` tag can get a release signed.

## Known gaps

- **SLSA Build Level 2**, not 3: provenance comes from GitHub artifact attestations on a
  GitHub-hosted runner. Level 3 needs the build in an isolated reusable workflow.
- **The toolchain floats:** `dtolnay/rust-toolchain` installs the current stable Rust. The build
  stages of the source `Dockerfile` (`node`, `rust`) are version-tagged, not digest-pinned, and
  Dependabot cannot update them. The runtime images (distroless) are digest-pinned.
- **CI caches:** CI and Rust workflows still use build caches for speed. Only the release
  workflow builds without them.
- **Transparency log:** every signature is recorded in the public Rekor log with the repository
  and workflow name. That is fine for this public repository; a private fork should use a private
  Sigstore instance or keys.
