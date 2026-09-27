#!/usr/bin/env bash
# Write one CycloneDX JSON SBOM for everything in the binary: the Rust crates
# (backend/Cargo.lock, every shipped target) and the web UI's runtime npm
# packages (package-lock.json, no devDependencies).
#
#   sbom.sh VERSION OUTFILE        e.g. sbom.sh 0.1.0 dist/shadoucmdb-0.1.0.cdx.json
#
# Needs on PATH: cargo, cargo-cyclonedx, cyclonedx (cyclonedx-cli). Needs
# `npm ci` at the root (the frontend tree) and in tools/sbom (the pinned
# cyclonedx-npm generator) beforehand.
set -euo pipefail

version=$1
out=$(realpath -m "$2")
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# SOURCE_DATE_EPOCH keeps the timestamp reproducible (the commit time).
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct)}

(cd "$root/backend" && cargo cyclonedx --format json --spec-version 1.5 --target all --override-filename sbom-rust -q)
mv "$root/backend/sbom-rust.json" "$work/rust.json"

(cd "$root" && tools/sbom/node_modules/.bin/cyclonedx-npm --omit dev --workspace frontend \
  --spec-version 1.5 --output-reproducible --output-file "$work/webui.json")

mkdir -p "$(dirname "$out")"
cyclonedx merge --name shadoucmdb --version "$version" \
  --input-files "$work/rust.json" "$work/webui.json" --output-format json --output-file "$out" >/dev/null
cyclonedx validate --input-file "$out" --fail-on-errors

python3 - "$out" <<'EOF'
import json, sys
bom = json.load(open(sys.argv[1]))
comps = bom.get("components", [])
if len(comps) < 50:
    sys.exit(f"SBOM has only {len(comps)} components; a generator step went wrong")
print(f"{sys.argv[1]}: CycloneDX {bom['specVersion']}, {len(comps)} components")
EOF
