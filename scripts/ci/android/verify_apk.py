#!/usr/bin/env python3
"""Assert that the debug APK carries this build's Rust cdylibs for fixed ABIs.

The Android job passes ``--expected <abi>=<freshly built .so>`` for every ABI in
its explicit contract. This checks more than a filename: the packaged entry must
be a 64-bit little-endian ELF shared object for the right machine, and its
SHA-256 must match the library built from the current checkout. A stale or
wrong-architecture library cannot be mistaken for a pass.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
import zipfile

ELF_MAGIC = b"\x7fELF"
ET_DYN = 3
MACHINES = {
    "arm64-v8a": 183,  # EM_AARCH64
    "x86_64": 62,  # EM_X86_64
}
MACHINE_NAMES = {183: "AArch64", 62: "x86-64"}


class CheckError(Exception):
    pass


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def check_elf(abi: str, data: bytes) -> None:
    if len(data) < 64:
        raise CheckError(f"lib/{abi}/libcore_ffi_android.so is too small to be ELF")
    if data[:4] != ELF_MAGIC:
        raise CheckError(f"lib/{abi}/libcore_ffi_android.so is not an ELF file")
    elf_class = data[4]
    elf_data = data[5]
    if elf_class != 2:
        raise CheckError(f"lib/{abi}/libcore_ffi_android.so is not ELF64 (class={elf_class})")
    if elf_data != 1:
        raise CheckError(f"lib/{abi}/libcore_ffi_android.so is not little-endian (data={elf_data})")
    e_type, e_machine = struct.unpack_from("<HH", data, 16)
    if e_type != ET_DYN:
        raise CheckError(
            f"lib/{abi}/libcore_ffi_android.so is not a shared object (e_type={e_type})"
        )
    expected = MACHINES[abi]
    if e_machine != expected:
        actual = MACHINE_NAMES.get(e_machine, str(e_machine))
        raise CheckError(
            f"lib/{abi}/libcore_ffi_android.so targets {actual} "
            f"(e_machine={e_machine}), expected {MACHINE_NAMES[expected]}"
        )


def verify(apk: str, expected: dict[str, str]) -> dict:
    try:
        archive = zipfile.ZipFile(apk)
    except (OSError, zipfile.BadZipFile) as exc:
        raise CheckError(f"cannot open APK {apk}: {exc}") from exc

    with archive:
        names = set(archive.namelist())
        if "AndroidManifest.xml" not in names:
            raise CheckError("APK has no AndroidManifest.xml")
        if not any(name.endswith(".dex") for name in names):
            raise CheckError("APK has no classes*.dex; the debug build produced no code")

        entries: dict[str, str] = {}
        for abi, so_path in sorted(expected.items()):
            entry = f"lib/{abi}/libcore_ffi_android.so"
            if entry not in names:
                raise CheckError(f"APK is missing {entry}")
            data = archive.read(entry)
            check_elf(abi, data)

            with open(so_path, "rb") as handle:
                built = handle.read()
            built_hash = sha256(built)
            packaged_hash = sha256(data)
            if built_hash != packaged_hash:
                raise CheckError(
                    f"{entry} does not match the library built from this checkout "
                    f"(packaged={packaged_hash[:16]}…, built={built_hash[:16]}…). "
                    "A stale jniLibs copy or binary rewriting would invalidate the evidence."
                )
            entries[abi] = packaged_hash
            print(f"OK: {entry} is {MACHINE_NAMES[MACHINES[abi]]} ELF64 sha256={packaged_hash}")

        return {"apk": apk, "abis": entries, "entries": len(names)}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", required=True)
    parser.add_argument(
        "--expected",
        action="append",
        default=[],
        metavar="ABI=PATH",
        help="ABI and the .so built in this run (repeatable)",
    )
    parser.add_argument("--json-out")
    args = parser.parse_args(argv)

    expected: dict[str, str] = {}
    for item in args.expected:
        if "=" not in item:
            print(f"FAIL: --expected expects ABI=PATH, got {item!r}", file=sys.stderr)
            return 2
        abi, path = item.split("=", 1)
        if abi not in MACHINES:
            print(f"FAIL: unsupported ABI {abi!r}", file=sys.stderr)
            return 2
        expected[abi] = path
    if not expected:
        print("FAIL: no --expected ABI was provided", file=sys.stderr)
        return 2

    try:
        summary = verify(args.apk, expected)
    except (CheckError, OSError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1

    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as handle:
            json.dump(summary, handle, indent=2, sort_keys=True)
            handle.write("\n")
    print(f"OK: APK contains {len(summary['abis'])} fresh Rust ABI librar(y/ies)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
