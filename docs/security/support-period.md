# Support period

The support period is the time during which a ShadouCMDB release line receives security fixes
(CRA Art. 13(8)).

**Support period:** `TODO(owner): support period, e.g. "5 years from the release of each minor version"`.

## What we commit to

- **Stated up front.** Every release's notes and the table in [SECURITY.md](../../SECURITY.md#supported-versions)
  give the end-of-support date as month and year. The date is fixed when the release ships and is
  never shortened afterwards.
- **At least five years**, unless the product is expected to be in use for less time (CRA
  Art. 13(8)). Release candidates (`-rc.*`) are not covered: they are supported only until the
  final release they lead to.
- **Security fixes are free** and ship without delay, as patch releases (`x.y.Z`) of each supported
  line, separate from feature releases where technically possible (Annex I Part II (8)).
- **Fixes stay available.** Every security release remains downloadable from GitHub Releases and
  GHCR for at least ten years after it is published, or until the end of the support period if
  that is later (Art. 13(9)).
- **Advisories for every fixed vulnerability**, as GitHub security advisories (with a CVE where one
  is assigned), naming the affected and fixed versions, severity and remediation.

## What "supported" means

| | Supported release line | Unsupported release line |
| --- | --- | --- |
| Security fixes | Yes | No. Upgrade to a supported line |
| Advisories list it as affected | Yes | Yes, where we know, so you can tell you are exposed |
| Bug fixes | Latest line only | No |
| Upgrade path to the latest release | Yes, with `shadoucmdb migrate` | Via each intermediate minor version |

## End of support

We announce the end of support for a release line at least six months in advance in the release
notes and in the GitHub repository. After the end date the line gets no further fixes; the
releases stay downloadable as described above.

## How updates reach you

ShadouCMDB [never contacts us](telemetry.md), so it cannot update itself or tell you about an
update. To hear about security releases:

- watch the GitHub repository for *Releases* and *Security alerts*, or subscribe to its
  release feed (`https://github.com/Shadoukita/ShadouCMDB/releases.atom`);
- `TODO(owner): customer notification channel, e.g. a security-announce mailing list`.

Check every download against `SHA256SUMS` (see [deployment](../deployment.md#release-downloads)).
Once signed releases are available (SHAA-77 workstream 1), verify the signature as well.
