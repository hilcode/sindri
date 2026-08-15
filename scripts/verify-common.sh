# Shared helpers for each example's verify.sh. Sourced with the example's own directory as the
# current working directory, and with the `sindri` binary already installed and on `PATH`.

fail() {
    echo -e "FAIL: $*" >&2
    exit 1
}

# The single binary the example's go-compile task has produced so far.
compiled_binary() {
    local matches
    matches=(.target/go-compile/*/*)
    if [ "${#matches[@]}" -ne 1 ] || [ ! -f "${matches[0]}" ]; then
        fail "expected exactly one compiled binary under .target/go-compile/*/, found: ${matches[*]}"
    fi
    echo "${matches[0]}"
}

# Compile and assert the run produced no output — the incremental-correctness no-op case.
expect_silent_compile() {
    local output
    output=$(sindri compile)
    [ -z "$output" ] || fail "expected a silent compile, got:\n$output"
}

# Compile and assert the run produced output — something was expected to rebuild.
expect_rebuild() {
    local output
    output=$(sindri compile)
    [ -n "$output" ] || fail "expected a rebuild, but the compile was silent"
}

# Assert the currently compiled binary's output matches exactly.
assert_binary_prints() {
    local expected="$1" actual
    actual=$("$(compiled_binary)")
    [ "$actual" = "$expected" ] || fail "expected the binary to print '$expected', got: $actual"
}

# Build from clean and confirm a second compile is a silent no-op — the one guarantee every example
# must satisfy, regardless of what else it demonstrates.
verify_settles() {
    rm -rf .target
    echo "==> compiling from clean"
    sindri compile
    echo "==> compiling again, unchanged (expecting silence)"
    expect_silent_compile
}

# Copy $1 aside and echo the backup path, so the caller can register its own restore trap:
#   backup=$(backup_file "$source_file")
#   trap 'cp "$backup" "$source_file"; rm -f "$backup"; rm -rf .target' EXIT
backup_file() {
    local backup
    backup=$(mktemp)
    cp "$1" "$backup"
    echo "$backup"
}
