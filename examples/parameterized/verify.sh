#!/usr/bin/env bash
# Proves that two parameter bindings of the same module coexist side by side rather than
# overwriting each other.
set -euo pipefail
cd "$(dirname "$0")"
source ../../scripts/verify-common.sh

build_file=sindri.build
backup=$(backup_file "$build_file")
trap 'cp "$backup" "$build_file"; rm -f "$backup"; sindri clean' EXIT

verify_settles
assert_binary_prints "Hello from a parameterized Sindri build!"
debug_count=$(find .target/go-package -mindepth 1 -maxdepth 1 -type d | wc -l)
[ "$debug_count" -eq 1 ] || fail "expected exactly one binding directory after the first build, got $debug_count"

echo "==> switching to mode = release and building again"
sed -i 's/mode = "debug"/mode = "release"/' "$build_file"
sindri package
both_count=$(find .target/go-package -mindepth 1 -maxdepth 1 -type d | wc -l)
[ "$both_count" -eq 2 ] || fail "expected both binding directories to coexist, got $both_count"

echo "OK: the debug and release bindings coexist side by side under go-package/"
