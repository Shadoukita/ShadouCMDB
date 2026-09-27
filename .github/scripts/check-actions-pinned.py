#!/usr/bin/env python3
"""Fail when a GitHub Actions `uses:` is not pinned to a full commit SHA.

    check-actions-pinned.py [PATH...]    default: .github/workflows .github/actions

Reads every .yml/.yaml file under PATH as YAML, so a step written in flow style
({uses: ...}), with a quoted key or with a folded value is checked like any
other. Allowed: owner/repo[/path]@<40-hex SHA>, local actions (./...) and
docker://image@sha256:<64-hex digest>. Keep the version in a trailing comment
(`# v4.4.0`) so Dependabot can bump the SHA. A file that does not parse fails.
"""
import pathlib
import re
import sys

import yaml

PINNED = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}")
DOCKER = re.compile(r"docker://[^@\s]+@sha256:[0-9a-f]{64}")


def yaml_files(paths):
    for p in map(pathlib.Path, paths):
        if p.is_file():
            yield p
        elif p.is_dir():
            yield from sorted(f for f in p.rglob("*") if f.suffix in (".yml", ".yaml") and f.is_file())


def uses_nodes(node):
    if isinstance(node, yaml.MappingNode):
        for key, value in node.value:
            if isinstance(key, yaml.ScalarNode) and key.value == "uses":
                yield value
            yield from uses_nodes(value)
    elif isinstance(node, yaml.SequenceNode):
        for item in node.value:
            yield from uses_nodes(item)


def pinned(node):
    if not isinstance(node, yaml.ScalarNode):
        return False
    ref = node.value
    return ref.startswith("./") or bool(PINNED.fullmatch(ref) or DOCKER.fullmatch(ref))


def main(paths):
    bad = checked = files = 0
    for f in yaml_files(paths):
        files += 1
        try:
            docs = list(yaml.compose_all(f.read_text(encoding="utf-8"), Loader=yaml.SafeLoader))
        except yaml.YAMLError as e:
            print(f"::error file={f}::Not valid YAML, cannot check action pins: {e}".replace("\n", " "))
            bad += 1
            continue
        for doc in docs:
            for node in uses_nodes(doc):
                checked += 1
                if not pinned(node):
                    bad += 1
                    ref = node.value if isinstance(node, yaml.ScalarNode) else f"<{node.id}>"
                    line = node.start_mark.line + 1
                    print(f"::error file={f},line={line}::Unpinned action '{ref}': "
                          "use owner/repo@<40-character commit SHA> # <version>")
    if files == 0:
        print(f"::error::No workflow files found under {' '.join(paths)}")
        return 1
    if bad:
        print(f"{bad} of {checked} uses: references in {files} files are not pinned by commit SHA")
        return 1
    print(f"all {checked} uses: references in {files} files are pinned by commit SHA")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:] or [".github/workflows", ".github/actions"]))
