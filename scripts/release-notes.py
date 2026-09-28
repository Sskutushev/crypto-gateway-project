#!/usr/bin/env python3
"""Print the CHANGELOG.md section of one version, for a GitHub Release.

    python scripts/release-notes.py 0.1.0            # or v0.1.0
    python scripts/release-notes.py v0.1.0 --out notes.md

Fails when the section is missing or empty, or when the version differs from
[workspace.package] version in Cargo.toml: a release must not publish an image
whose notes or binaries describe another version.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$")


def changelog_section(text: str, version: str) -> str | None:
    lines = text.splitlines()
    header = re.compile(rf"^## \[{re.escape(version)}\](\s|$)")
    for start, line in enumerate(lines):
        if header.match(line):
            end = start + 1
            while end < len(lines) and not lines[end].startswith("## "):
                end += 1
            # Link reference definitions at the end of the file belong to no section.
            body = [line for line in lines[start + 1 : end] if not re.match(r"^\[[^\]]+\]: ", line)]
            section = "\n".join(body).strip()
            return section or None
    return None


def cargo_version(text: str) -> str | None:
    match = re.search(r"^\[workspace\.package\][^\[]*?^version\s*=\s*\"([^\"]+)\"", text, re.M | re.S)
    return match.group(1) if match else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    version = args.version.removeprefix("refs/tags/").removeprefix("v")
    if not SEMVER.match(version):
        print(f"error: '{args.version}' is not a semantic version", file=sys.stderr)
        return 1

    section = changelog_section((ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), version)
    if section is None:
        print(f"error: CHANGELOG.md has no non-empty '## [{version}]' section", file=sys.stderr)
        return 1

    cargo = cargo_version((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    if cargo != version:
        print(f"error: Cargo.toml [workspace.package] version is {cargo!r}, the tag is {version!r}", file=sys.stderr)
        return 1

    if args.out:
        args.out.write_text(section + "\n", encoding="utf-8", newline="\n")
    else:
        sys.stdout.write(section + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
