#!/usr/bin/env bash
# Proves that editing only the local library dependency rebuilds the app, that the rebuild
# actually changes the app's behaviour (not just that some task re-ran), and that the tree
# then settles.
set -euo pipefail
cd "$(dirname "$0")"
source ../../scripts/verify-common.sh

source_file=lib/greeting/greeting.go
backup=$(backup_file "$source_file")
trap 'cp "$backup" "$source_file"; rm -f "$backup"; sindri clean' EXIT

verify_settles
assert_binary_prints "Hello, Sindri!"

echo "==> editing the library only"
sed -i 's/Hello/Hi/' "$source_file"

echo "==> compiling after the edit (expecting a rebuild)"
expect_rebuild
assert_binary_prints "Hi, Sindri!"

echo "==> compiling once more (expecting silence again)"
expect_silent_compile

echo "OK: editing the dependency rebuilds the app, whose output reflects the change, then settles"
