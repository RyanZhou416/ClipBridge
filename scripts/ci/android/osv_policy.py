#!/usr/bin/env python3
"""Evaluate an OSV-Scanner JSON report against the ClipBridge CI policy.

Policy (docs/ci-architecture.md section 7.1): known HIGH/CRITICAL findings block
the Android job; a finding whose severity cannot be established is treated as
UNKNOWN and also blocks. Low/medium findings are printed but do not block. When
several severity signals disagree, the most severe one wins.

The report must have been produced from the resolved Gradle lockfiles, not from
version declarations. ``--lockfile`` makes that a checked input: every
(ecosystem, module, version) triple in the lockfile has to appear in the scan
report, otherwise the scanner did not actually see the full (direct +
transitive) graph and the check fails instead of reporting a false "clean".
Malformed package/vulnerability/group entries also fail the check instead of
being skipped as "no vulnerabilities".

Exit codes:
  0  scan is valid and has no blocking finding
  1  at least one HIGH/CRITICAL/UNKNOWN finding
  2  the report/lockfile input is invalid, empty or incomplete
"""

from __future__ import annotations

import argparse
import json
import math
import sys

BLOCKING = {"critical", "high", "unknown"}
SEVERITY_RANK = {"none": 0, "low": 1, "medium": 2, "high": 3, "critical": 4, "unknown": 5}

# CVSS v3.0/v3.1 base metric weights. OSV advisories that only ship a vector can
# be classified without a full database lookup; vectors we cannot parse stay
# UNKNOWN and therefore block.
_AV = {"N": 0.85, "A": 0.62, "L": 0.55, "P": 0.2}
_AC = {"L": 0.77, "H": 0.44}
_UI = {"N": 0.85, "R": 0.62}
_CIA = {"H": 0.56, "L": 0.22, "N": 0.0}


class InputError(Exception):
    """The report or lockfile cannot be used as evidence."""


def _roundup(value: float) -> float:
    # CVSS v3.1 Appendix A: smallest number, specified to one decimal place,
    # greater than or equal to input.
    integer = int(round(value * 100000))
    if integer % 10000 == 0:
        return integer / 100000.0
    return (math.floor(integer / 10000) + 1) / 10.0


def parse_cvss3_vector(vector: object) -> float | None:
    if not isinstance(vector, str):
        return None
    prefix = vector.split("/", 1)[0]
    if prefix not in ("CVSS:3.0", "CVSS:3.1"):
        return None
    metrics: dict[str, str] = {}
    for part in vector.split("/")[1:]:
        if ":" in part:
            key, value = part.split(":", 1)
            metrics[key] = value
    try:
        scope_changed = metrics["S"] == "C"
        pr = metrics["PR"]
        pr_weight = (
            {"N": 0.85, "L": 0.68, "H": 0.50}[pr]
            if scope_changed
            else {"N": 0.85, "L": 0.62, "H": 0.27}[pr]
        )
        iss = 1.0 - (
            (1.0 - _CIA[metrics["C"]])
            * (1.0 - _CIA[metrics["I"]])
            * (1.0 - _CIA[metrics["A"]])
        )
        if scope_changed:
            impact = 7.52 * (iss - 0.029) - 3.25 * (iss - 0.02) ** 15
        else:
            impact = 6.42 * iss
        exploitability = (
            8.22
            * _AV[metrics["AV"]]
            * _AC[metrics["AC"]]
            * pr_weight
            * _UI[metrics["UI"]]
        )
        if impact <= 0:
            return 0.0
        if scope_changed:
            return min(_roundup(min(1.08 * (impact + exploitability), 10.0)), 10.0)
        return min(_roundup(min(impact + exploitability, 10.0)), 10.0)
    except KeyError:
        return None


def score_to_severity(value: object) -> str | None:
    """Map a CVSS base score to a severity label.

    ``0`` is deliberately treated as unknown: OSV-Scanner reports a zero
    ``max_severity`` when it has no severity data, and policy requires unknown
    severities to block rather than pass.
    """
    try:
        if isinstance(value, bool):
            return None
        if isinstance(value, (int, float)):
            score = float(value)
        elif isinstance(value, str):
            text = value.strip()
            if not text:
                return None
            if text.upper().startswith("CVSS:"):
                parsed = parse_cvss3_vector(text)
                if parsed is None:
                    return None
                score = parsed
            else:
                score = float(text)
        else:
            return None
    except (TypeError, ValueError):
        return None
    if not math.isfinite(score) or score <= 0:
        return None
    if score >= 9.0:
        return "critical"
    if score >= 7.0:
        return "high"
    if score >= 4.0:
        return "medium"
    return "low"


def label_to_severity(value: object) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        return None
    label = value.strip().lower()
    if not label:
        return None
    if label in ("critical", "high", "medium", "moderate", "low"):
        return "medium" if label == "moderate" else label
    if label == "none":
        return "none"
    # An unfamiliar/unknown label is not evidence of safety.
    return "unknown"


def lockfile_packages(path: str) -> set[tuple[str, str, str]]:
    """Return the (ecosystem, module, version) triples Gradle resolved."""
    packages: set[tuple[str, str, str]] = set()
    try:
        with open(path, "r", encoding="utf-8") as handle:
            for raw_line in handle:
                line = raw_line.strip()
                if not line or line.startswith("#") or line.startswith("empty="):
                    continue
                coordinate = line.split("=", 1)[0]
                parts = coordinate.split(":")
                if len(parts) < 3 or not all(parts[:3]):
                    continue
                packages.add(("maven", f"{parts[0]}:{parts[1]}", parts[2]))
    except OSError as exc:
        raise InputError(f"cannot read lockfile {path}: {exc}") from exc
    if not packages:
        raise InputError(f"lockfile {path} contains no module coordinates")
    return packages


def iter_package_entries(report: object):
    if not isinstance(report, dict):
        raise InputError("report root is not a JSON object")
    results = report.get("results")
    if results is None:
        # osv-scanner can emit {} when nothing was found; that provides no
        # evidence that the lockfile was parsed, so the coverage check fails.
        return
    if not isinstance(results, list):
        raise InputError("report.results is not an array")
    for result in results:
        if not isinstance(result, dict):
            raise InputError("report.results contains a non-object entry")
        if "packages" in result:
            packages = result["packages"]
            if not isinstance(packages, list):
                raise InputError("result.packages is not an array")
            for entry in packages:
                if not isinstance(entry, dict):
                    raise InputError("result.packages contains a non-object entry")
                yield entry
        elif "package" in result:
            # v1-compatible shape: the result entry itself is a package.
            yield result
        else:
            raise InputError("result entry has neither packages[] nor package")


def _vulnerability_severity(vulnerability: dict, groups: list) -> tuple[str, str]:
    ids: set[str] = set()
    if isinstance(vulnerability.get("id"), str):
        ids.add(vulnerability["id"])
    aliases = vulnerability.get("aliases")
    if aliases is not None:
        if not isinstance(aliases, list):
            raise InputError("vulnerability.aliases is not an array")
        ids.update(alias for alias in aliases if isinstance(alias, str))

    candidates: list[tuple[str, str]] = []
    unknown_signals: list[str] = []

    for group in groups:
        if not isinstance(group, dict):
            raise InputError("package.groups contains a non-object entry")
        group_ids = group.get("ids")
        if group_ids is not None:
            if not isinstance(group_ids, list):
                raise InputError("group.ids is not an array")
            matched = ids.intersection(item for item in group_ids if isinstance(item, str))
            if not matched:
                continue
        elif not ids:
            continue
        if "max_severity" not in group:
            continue
        raw = group.get("max_severity")
        severity = score_to_severity(raw)
        if severity:
            candidates.append((severity, "cvss"))
        else:
            # A group with an empty/zero/unreadable score is not evidence of
            # safety, but it must not override a severity we did parse.
            unknown_signals.append("malformed-cvss")

    for key in ("database_specific", "ecosystem_specific"):
        specific = vulnerability.get(key)
        if specific is None:
            continue
        if not isinstance(specific, dict):
            raise InputError(f"vulnerability.{key} is not an object")
        severity = label_to_severity(specific.get("severity"))
        if severity == "unknown":
            unknown_signals.append("unrecognised-label")
        elif severity is not None:
            candidates.append((severity, "label"))

    severity_entries = vulnerability.get("severity")
    if severity_entries is not None:
        if not isinstance(severity_entries, list):
            raise InputError("vulnerability.severity is not an array")
        for entry in severity_entries:
            if not isinstance(entry, dict):
                raise InputError("vulnerability.severity contains a non-object entry")
            raw = entry.get("score")
            parsed = None
            if isinstance(raw, (int, float)) and not isinstance(raw, bool):
                parsed = score_to_severity(raw)
            elif isinstance(raw, str):
                parsed = score_to_severity(raw)
            if parsed:
                candidates.append((parsed, "cvss"))
            else:
                # CVSS v4 vectors are not scored here. If another signal gave
                # us a usable severity, keep it; otherwise the finding is
                # unknown and blocks.
                unknown_signals.append("unparsed-vector")

    if candidates:
        severity, source = max(candidates, key=lambda item: SEVERITY_RANK.get(item[0], 4))
        return severity, source
    if unknown_signals:
        return "unknown", unknown_signals[0]
    return "unknown", "missing"


def evaluate(report: dict, lock_packages: set[tuple[str, str, str]], label: str) -> tuple[int, dict]:
    scanned: set[tuple[str, str, str]] = set()
    findings: list[dict] = []
    package_count = 0

    for entry in iter_package_entries(report):
        package = entry.get("package")
        if not isinstance(package, dict):
            raise InputError("package entry has no package object")
        name = package.get("name")
        version = package.get("version")
        ecosystem = package.get("ecosystem")
        if not isinstance(name, str) or not name:
            raise InputError("package entry has no valid name")
        if not isinstance(version, str) or not version:
            raise InputError(f"package {name} has no valid version")
        if not isinstance(ecosystem, str) or not ecosystem:
            raise InputError(f"package {name} has no valid ecosystem")

        package_count += 1
        scanned.add((ecosystem.strip().lower(), name, version))

        vulnerabilities = entry.get("vulnerabilities", [])
        if vulnerabilities is None:
            vulnerabilities = []
        if not isinstance(vulnerabilities, list):
            raise InputError(f"package {name} has non-array vulnerabilities")
        groups = entry.get("groups", [])
        if groups is None:
            groups = []
        if not isinstance(groups, list):
            raise InputError(f"package {name} has non-array groups")

        for vulnerability in vulnerabilities:
            if not isinstance(vulnerability, dict):
                raise InputError(f"package {name} has a non-object vulnerability")
            severity, source = _vulnerability_severity(vulnerability, groups)
            findings.append(
                {
                    "package": name,
                    "version": version,
                    "id": vulnerability.get("id", "<no-id>"),
                    "severity": severity,
                    "severity_source": source,
                }
            )

    missing = sorted(lock_packages - scanned)
    summary = {
        "label": label,
        "packages_in_lockfile": len(lock_packages),
        "packages_scanned": package_count,
        "findings": findings,
        "blocking": [item for item in findings if item["severity"] in BLOCKING],
        "missing_packages": ["|".join(item) for item in missing],
    }

    if package_count == 0:
        raise InputError(
            f"[{label}] scanner reported zero packages; the lockfile was not actually scanned"
        )
    if missing:
        preview = ", ".join("|".join(item) for item in missing[:10])
        suffix = "" if len(missing) <= 10 else f" (+{len(missing) - 10} more)"
        raise InputError(
            f"[{label}] lockfile packages missing from (or version-mismatched in) "
            f"the scan report: {preview}{suffix}"
        )

    print(
        f"OSV [{label}] packages_scanned={package_count} "
        f"lockfile_packages={len(lock_packages)} vulnerabilities={len(findings)}"
    )
    counts = {"critical": 0, "high": 0, "medium": 0, "low": 0, "unknown": 0, "none": 0}
    for item in findings:
        counts[item["severity"]] = counts.get(item["severity"], 0) + 1
    print(
        "OSV [{0}] severity critical={1} high={2} medium={3} low={4} unknown={5}".format(
            label,
            counts["critical"],
            counts["high"],
            counts["medium"],
            counts["low"],
            counts["unknown"],
        )
    )
    for item in findings:
        marker = "BLOCK" if item["severity"] in BLOCKING else "warn "
        print(
            f"  {marker} {item['severity']:8s} {item['package']}@{item['version']} "
            f"{item['id']} (severity via {item['severity_source']})"
        )

    return (1 if summary["blocking"] else 0), summary


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", required=True, help="OSV-Scanner JSON output")
    parser.add_argument(
        "--lockfile",
        required=True,
        help="Gradle lockfile the report must cover",
    )
    parser.add_argument("--label", default="gradle", help="report label for logs")
    parser.add_argument("--json-out", help="optional path for a machine-readable summary")
    args = parser.parse_args(argv)

    try:
        lock_packages = lockfile_packages(args.lockfile)
        try:
            with open(args.report, "r", encoding="utf-8") as handle:
                report = json.load(handle)
        except OSError as exc:
            raise InputError(f"cannot read report {args.report}: {exc}") from exc
        except json.JSONDecodeError as exc:
            raise InputError(f"report {args.report} is not valid JSON: {exc}") from exc
        status, summary = evaluate(report, lock_packages, args.label)
    except InputError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 2

    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as handle:
            json.dump(summary, handle, indent=2, sort_keys=True)
            handle.write("\n")

    if status != 0:
        print(
            f"FAIL: [{args.label}] {len(summary['blocking'])} blocking vulnerability finding(s) "
            "(high/critical/unknown)",
            file=sys.stderr,
        )
        return 1
    print(f"PASS: [{args.label}] no high/critical/unknown findings")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
