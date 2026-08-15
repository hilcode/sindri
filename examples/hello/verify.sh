#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
source ../../scripts/verify-common.sh

verify_settles
assert_binary_prints "Hello from Sindri!"
echo "OK: hello builds, prints the expected greeting, and settles"
