#!/usr/bin/env bash
# Proves that editing the workspace-built tool regenerates its output and the consuming module
# recompiles against the new generated source.
set -euo pipefail
cd "$(dirname "$0")"
source ../../scripts/verify-common.sh

source_file=tools/codegen/main.go
backup=$(backup_file "$source_file")
trap 'cp "$backup" "$source_file"; rm -f "$backup" generated_greeting.go; sindri clean' EXIT

verify_settles
assert_binary_prints "Hello from codegen!"

echo "==> editing the tool's source"
sed -i 's/Hello from codegen!/Howdy from codegen!/' "$source_file"

echo "==> building after the edit (expecting a rebuild)"
expect_rebuild
assert_binary_prints "Howdy from codegen!"

echo "==> building once more (expecting silence again)"
expect_silent_build

echo "OK: editing the tool regenerates its output and the app recompiles against it"
