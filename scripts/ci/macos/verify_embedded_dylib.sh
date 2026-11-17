#!/usr/bin/env bash
# Prove that the dylib inside the built app is the library built from this
# checkout, not a stale copy (for example the git-tracked Native/ dylib).
#
#   verify_embedded_dylib.sh <freshly-built-dylib> <embedded-dylib>
#
# The CI entry pre-signs the freshly built dylib with the same ad-hoc identity
# the Xcode embed phase uses, and the embed phase preserves an existing ad-hoc
# signature instead of re-signing it, so a byte-level hash comparison is a valid
# provenance check. Any tampering or substitution changes the hash and fails.
set -euo pipefail

if [[ $# -ne 2 ]]; then
	echo "usage: $0 <fresh-dylib> <embedded-dylib>" >&2
	exit 2
fi

FRESH="$1"
EMBEDDED="$2"

for candidate in "$FRESH" "$EMBEDDED"; do
	[[ -f "$candidate" ]] || {
		echo "FAIL: dylib not found: $candidate" >&2
		exit 1
	}
done

hash_file() {
	if command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | awk '{print $1}'
	elif command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{print $1}'
	else
		echo "FAIL: no SHA-256 tool available (shasum/sha256sum)" >&2
		exit 2
	fi
}

FRESH_HASH="$(hash_file "$FRESH")"
EMBEDDED_HASH="$(hash_file "$EMBEDDED")"

if [[ "$FRESH_HASH" != "$EMBEDDED_HASH" ]]; then
	echo "FAIL: embedded dylib is not the current build" >&2
	echo "  fresh:    $FRESH $FRESH_HASH" >&2
	echo "  embedded: $EMBEDDED $EMBEDDED_HASH" >&2
	exit 1
fi

echo "OK: embedded dylib matches the current build (sha256 $EMBEDDED_HASH)"
