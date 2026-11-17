#!/usr/bin/env python3
"""Offline tests for the Android CI policy helpers.

These do not need the Android toolchain: they feed crafted OSV reports and a
synthetic APK to scripts/ci/android/osv_policy.py and verify_apk.py and assert
the documented pass/block/input-error behaviour. Windows/Linux/macOS all work.
"""

from __future__ import annotations

import json
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve()
ANDROID_HELPERS = HERE.parents[1]  # scripts/ci/android
POLICY = ANDROID_HELPERS / "osv_policy.py"
APK_VERIFIER = ANDROID_HELPERS / "verify_apk.py"

LOCK_VERSIONS = {
    "androidx.appcompat:appcompat": "1.6.1",
    "androidx.core:core": "1.9.0",
    "net.java.dev.jna:jna": "5.18.1",
}
LOCK_TEXT = (
    "androidx.appcompat:appcompat:1.6.1=debugRuntimeClasspath,releaseRuntimeClasspath\n"
    "androidx.core:core:1.9.0=debugRuntimeClasspath,releaseRuntimeClasspath\n"
    "net.java.dev.jna:jna:5.18.1=debugRuntimeClasspath,releaseRuntimeClasspath\n"
    "empty=releaseAnnotationProcessorClasspath\n"
)
LOCK_MODULES = list(LOCK_VERSIONS)


def package(name: str, vulnerabilities=None, groups=None, version=None) -> dict:
    entry = {
        "package": {
            "name": name,
            "version": version or LOCK_VERSIONS[name],
            "ecosystem": "Maven",
        },
        "groups": groups or [],
    }
    if vulnerabilities is not None:
        entry["vulnerabilities"] = vulnerabilities
    return entry


def report(packages) -> dict:
    return {"results": [{"source": {"path": "app/gradle.lockfile", "type": "lockfile"}, "packages": packages}]}


def run_policy(report_obj, lock_text=LOCK_TEXT, extra_env=None) -> subprocess.CompletedProcess:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        report_path = tmp_path / "report.json"
        lock_path = tmp_path / "gradle.lockfile"
        summary_path = tmp_path / "summary.json"
        if isinstance(report_obj, str):
            report_path.write_text(report_obj, encoding="utf-8")
        else:
            report_path.write_text(json.dumps(report_obj), encoding="utf-8")
        lock_path.write_text(lock_text, encoding="utf-8")
        return subprocess.run(
            [
                sys.executable,
                str(POLICY),
                "--report",
                str(report_path),
                "--lockfile",
                str(lock_path),
                "--label",
                "test",
                "--json-out",
                str(summary_path),
            ],
            capture_output=True,
            text=True,
            env=extra_env,
        )


def clean_packages():
    return [package(name) for name in LOCK_MODULES]


def expect_policy(name: str, report_obj, expected_rc: int, lock_text=LOCK_TEXT, contains: str = "") -> None:
    result = run_policy(report_obj, lock_text=lock_text)
    if result.returncode != expected_rc:
        raise AssertionError(
            f"[{name}] expected exit {expected_rc}, got {result.returncode}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    combined = result.stdout + result.stderr
    if contains and contains not in combined:
        raise AssertionError(f"[{name}] output does not contain {contains!r}:\n{combined}")
    print(f"PASS osv-policy: {name} -> exit {expected_rc}")


def test_policy() -> None:
    expect_policy("clean graph", report(clean_packages()), 0, contains="PASS")

    high = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[{"id": "GHSA-test-high", "aliases": ["CVE-2026-0001"]}],
                groups=[{"ids": ["GHSA-test-high"], "max_severity": "8.1"}],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("high severity blocks", high, 1, contains="blocking")

    critical_label = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {"id": "GHSA-test-critical", "database_specific": {"severity": "CRITICAL"}}
                ],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("critical label blocks", critical_label, 1)

    unknown = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[{"id": "GHSA-test-unknown"}],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("unknown severity blocks", unknown, 1, contains="unknown")

    medium = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[{"id": "GHSA-test-medium"}],
                groups=[{"ids": ["GHSA-test-medium"], "max_severity": "5.5"}],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("medium does not block", medium, 0, contains="warn")

    vector_high = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {
                        "id": "GHSA-test-vector",
                        "severity": [
                            {
                                "type": "CVSS_V3",
                                "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
                            }
                        ],
                    }
                ],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("CVSS 3.1 vector blocks", vector_high, 1)

    vector_low = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {
                        "id": "GHSA-test-low",
                        "severity": [
                            {
                                "type": "CVSS_V3",
                                "score": "CVSS:3.1/AV:L/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N",
                            }
                        ],
                    }
                ],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("CVSS 3.1 low vector does not block", vector_low, 0)

    cvss4_only = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {
                        "id": "GHSA-test-v4",
                        "severity": [
                            {
                                "type": "CVSS_V4",
                                "score": "CVSS:4.0/AV:N/AC:L/AT:P/PR:N/UI:N/VC:N/VI:N/VA:H/SC:N/SI:N/SA:N",
                            }
                        ],
                    }
                ],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("unscored CVSS v4 alone is unknown and blocks", cvss4_only, 1, contains="unknown")

    low_with_cvss4 = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {
                        "id": "GHSA-test-v4-low",
                        "database_specific": {"severity": "LOW"},
                        "severity": [
                            {
                                "type": "CVSS_V4",
                                "score": "CVSS:4.0/AV:N/AC:L/AT:P/PR:N/UI:N/VC:N/VI:N/VA:L/SC:N/SI:N/SA:N",
                            }
                        ],
                    }
                ],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("unscored CVSS v4 does not override a known LOW", low_with_cvss4, 0)

    high_with_cvss4 = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {
                        "id": "GHSA-test-v4-high",
                        "severity": [
                            {
                                "type": "CVSS_V4",
                                "score": "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:H/SC:N/SI:N/SA:N",
                            }
                        ],
                    }
                ],
                groups=[{"ids": ["GHSA-test-v4-high"], "max_severity": "8.2"}],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("scored group severity wins over unscored v4", high_with_cvss4, 1, contains="high")

    label_beats_numeric_low = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {
                        "id": "GHSA-test-mixed",
                        "database_specific": {"severity": "HIGH"},
                    }
                ],
                groups=[{"ids": ["GHSA-test-mixed"], "max_severity": "3.1"}],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("label HIGH overrides numeric low", label_beats_numeric_low, 1)

    unknown_label = report(
        [
            package(
                "net.java.dev.jna:jna",
                vulnerabilities=[
                    {"id": "GHSA-test-weird-label", "database_specific": {"severity": "SEVERITY_UNKNOWN"}}
                ],
            )
        ]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("unrecognised severity label blocks", unknown_label, 1)

    version_mismatch = report([package(name, version="9.9.9") for name in LOCK_MODULES])
    expect_policy("version-mismatched scan fails coverage", version_mismatch, 2, contains="missing")

    malformed_vulnerabilities = report(
        [package("net.java.dev.jna:jna", vulnerabilities={})]
        + [package(name) for name in LOCK_MODULES if name != "net.java.dev.jna:jna"]
    )
    expect_policy("non-array vulnerabilities fail", malformed_vulnerabilities, 2)

    result_without_package = {"results": [{"source": {"path": "app/gradle.lockfile", "type": "lockfile"}}]}
    expect_policy("result without package data fails", result_without_package, 2)

    entry_without_package_object = {"results": [{"packages": [{"vulnerabilities": []}]}]}
    expect_policy("entry without package object fails", entry_without_package_object, 2)

    incomplete = report([package("androidx.appcompat:appcompat")])
    expect_policy("missing lockfile coverage fails", incomplete, 2, contains="missing")

    expect_policy("zero scanned packages fails", {"results": []}, 2, contains="zero packages")

    expect_policy("malformed report fails", "{not json", 2, contains="not valid JSON")

    expect_policy("empty lockfile fails", report(clean_packages()), 2, lock_text="empty=releaseRuntimeClasspath\n")


def elf_bytes(machine: int, marker: bytes = b"") -> bytes:
    ident = bytearray(16)
    ident[:4] = b"\x7fELF"
    ident[4] = 2  # ELFCLASS64
    ident[5] = 1  # ELFDATA2LSB
    ident[6] = 1  # EV_CURRENT
    header = bytes(ident) + struct.pack("<HHI", 3, machine, 1)
    return header + b"\x00" * (64 - len(header)) + marker


def make_apk(tmp_path: Path, *, built_abis=("arm64-v8a", "x86_64"), zip_abis=None, manifest=True, mutate=None):
    zip_abis = zip_abis or built_abis
    apk = tmp_path / "app-debug.apk"
    built = {}
    so_files = {}
    for abi in built_abis:
        machine = 183 if abi == "arm64-v8a" else 62
        data = elf_bytes(machine)
        so_path = tmp_path / f"built-{abi}.so"
        so_path.write_bytes(data)
        built[abi] = so_path
        so_files[abi] = data
    if mutate:
        so_files.update(mutate)
    with zipfile.ZipFile(apk, "w") as archive:
        if manifest:
            archive.writestr("AndroidManifest.xml", b"<manifest/>")
        archive.writestr("classes.dex", b"dex\n035\x00")
        for abi in zip_abis:
            archive.writestr(f"lib/{abi}/libcore_ffi_android.so", so_files[abi])
    return apk, built


def run_apk(apk: Path, built: dict) -> subprocess.CompletedProcess:
    args = [sys.executable, str(APK_VERIFIER), "--apk", str(apk)]
    for abi, path in sorted(built.items()):
        args += ["--expected", f"{abi}={path}"]
    return subprocess.run(args, capture_output=True, text=True)


def expect_apk(
    name: str,
    expected_rc: int,
    *,
    built_abis=("arm64-v8a", "x86_64"),
    zip_abis=None,
    manifest=True,
    mutate=None,
) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        apk, built = make_apk(
            tmp_path,
            built_abis=built_abis,
            zip_abis=zip_abis,
            manifest=manifest,
            mutate=mutate,
        )
        result = run_apk(apk, built)
        if result.returncode != expected_rc:
            raise AssertionError(
                f"[{name}] expected exit {expected_rc}, got {result.returncode}\n"
                f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
            )
        print(f"PASS apk-verifier: {name} -> exit {expected_rc}")


def test_apk() -> None:
    expect_apk("both fresh ARM64 ABIs pass", 0)
    expect_apk(
        "wrong machine fails",
        1,
        mutate={"arm64-v8a": elf_bytes(62)},
    )
    expect_apk("missing x86_64 entry fails", 1, zip_abis=("arm64-v8a",))
    expect_apk("missing manifest fails", 1, manifest=False)
    expect_apk(
        "stale library bytes fail",
        1,
        mutate={"x86_64": elf_bytes(62, marker=b"stale")},
    )


def test_real_lockfiles() -> None:
    repo_root = HERE.parents[4]
    for relative in (
        "platforms/android/ClipBridgeShellAndroid/app/gradle.lockfile",
        "platforms/android/ClipBridgeShellAndroid/buildscript-gradle.lockfile",
    ):
        lock_path = repo_root / relative
        if not lock_path.is_file():
            print(f"SKIP osv-policy: {relative} not present")
            continue
        lock_text = lock_path.read_text(encoding="utf-8")
        entries = []
        for line in lock_text.splitlines():
            line = line.strip()
            if not line or line.startswith("#") or line.startswith("empty="):
                continue
            parts = line.split("=", 1)[0].split(":")
            if len(parts) >= 3 and all(parts[:3]):
                entries.append(
                    {
                        "package": {
                            "name": f"{parts[0]}:{parts[1]}",
                            "version": parts[2],
                            "ecosystem": "Maven",
                        }
                    }
                )
        result = run_policy({"results": [{"packages": entries}]}, lock_text=lock_text)
        if result.returncode != 0:
            raise AssertionError(
                f"[real {relative}] expected exit 0, got {result.returncode}\n"
                f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
            )
        print(f"PASS osv-policy: real lockfile {relative} ({len(entries)} packages)")


def main() -> int:
    test_policy()
    test_real_lockfiles()
    test_apk()
    print("OK: Android CI helper fixtures passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
