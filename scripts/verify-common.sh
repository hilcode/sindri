# Shared helpers for each example's verify.sh. Sourced with the example's own directory as the
# current working directory, and with the `sindri` binary already installed and on `PATH`.

fail() {
    echo -e "FAIL: $*" >&2
    exit 1
}

# The single binary the example's go-package task has produced so far. Only `sindri package` (not
# `sindri compile`) reaches the `package` step, so that's what every helper below runs.
compiled_binary() {
    local matches
    matches=(.target/go-package/*/*)
    if [ "${#matches[@]}" -ne 1 ] || [ ! -f "${matches[0]}" ]; then
        fail "expected exactly one compiled binary under .target/go-package/*/, found: ${matches[*]}"
    fi
    echo "${matches[0]}"
}

# Build and assert the run produced no output — the incremental-correctness no-op case.
expect_silent_build() {
    local output
    output=$(sindri package)
    [ -z "$output" ] || fail "expected a silent build, got:\n$output"
}

# Build and assert the run produced output — something was expected to rebuild.
expect_rebuild() {
    local output
    output=$(sindri package)
    [ -n "$output" ] || fail "expected a rebuild, but the build was silent"
}

# Assert the currently compiled binary's output matches exactly.
assert_binary_prints() {
    local expected="$1" actual
    actual=$("$(compiled_binary)")
    [ "$actual" = "$expected" ] || fail "expected the binary to print '$expected', got: $actual"
}

# Build from clean and confirm a second build is a silent no-op — the one guarantee every example
# must satisfy, regardless of what else it demonstrates.
verify_settles() {
    sindri clean
    echo "==> building from clean"
    sindri package
    echo "==> building again, unchanged (expecting silence)"
    expect_silent_build
}

# Copy $1 aside and echo the backup path, so the caller can register its own restore trap:
#   backup=$(backup_file "$source_file")
#   trap 'cp "$backup" "$source_file"; rm -f "$backup"; sindri clean' EXIT
backup_file() {
    local backup
    backup=$(mktemp)
    cp "$1" "$backup"
    echo "$backup"
}
