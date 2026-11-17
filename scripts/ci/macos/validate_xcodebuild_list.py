#!/usr/bin/env python3
"""Validate `xcodebuild -list -json` output for the CI macOS entry.

Using the JSON form avoids brittle text matching against xcodebuild's indented
human output. The scheme must be both a project target and a *shared* scheme
visible to xcodebuild; the SwiftPM stub is never accepted.

Exit codes: 0 = scheme/target present, 1 = missing, 2 = malformed input.
"""

from __future__ import annotations

import json
import sys


def fail(message: str, code: int) -> int:
    print(f"FAIL: {message}", file=sys.stderr)
    return code


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} <xcodebuild-list.json> <scheme>", file=sys.stderr)
        return 2
    path, scheme = argv
    try:
        with open(path, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except OSError as exc:
        return fail(f"cannot read {path}: {exc}", 2)
    except json.JSONDecodeError as exc:
        return fail(f"{path} is not valid JSON: {exc}", 2)

    if not isinstance(data, dict):
        return fail("xcodebuild -list -json root is not an object", 2)
    project = data.get("project")
    if not isinstance(project, dict):
        return fail("xcodebuild -list -json has no project object", 2)

    targets = project.get("targets")
    schemes = project.get("schemes")
    if not isinstance(targets, list) or not isinstance(schemes, list):
        return fail("project.targets/project.schemes are not arrays", 2)

    if scheme not in targets:
        return fail(f"project has no target named {scheme!r} (targets: {targets})", 1)
    if scheme not in schemes:
        return fail(
            f"scheme {scheme!r} is not visible to xcodebuild (schemes: {schemes}); "
            "a shared scheme must be committed under xcshareddata/xcschemes",
            1,
        )

    print(f"PASS: target and shared scheme {scheme!r} are present")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
