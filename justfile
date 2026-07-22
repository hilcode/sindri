# Directory the `install` recipe copies the `sindri` binary into
install_directory := env_var_or_default("SINDRI_INSTALL_DIR", env_var("HOME") / ".local/bin")

# List available recipes
default:
    @just --list

# Run all tests (unit + integration)
[group('test')]
test:
    cargo test

# Run only unit tests
[group('test')]
test-unit:
    cargo test --lib --bins

# Run only integration tests
[group('test')]
test-integration:
    cargo test --tests

# Run tests with coverage summary and enforce the coverage floor
[group('coverage')]
coverage:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo llvm-cov --no-report
    cargo llvm-cov report | scripts/coverage-ratchet.sh

# Coverage for unit tests only
[group('coverage')]
coverage-unit:
    cargo llvm-cov --lib --bins

# Coverage for integration tests only
[group('coverage')]
coverage-integration:
    cargo llvm-cov --tests

# Generate HTML coverage report
[group('coverage')]
coverage-html:
    cargo llvm-cov --html
    @echo "Report: target/llvm-cov/html/index.html"

# Open HTML coverage report in browser
[group('coverage')]
coverage-open:
    cargo llvm-cov --open

# Build the `sindri` binary (debug or release)
[group('build')]
build type="debug": format
    #!/usr/bin/env bash
    if [ {{type}} == 'debug' ]; then
        cargo build
    elif [ {{type}} == 'release' ]; then
        cargo build --release
    else
        echo 'Unknown build target: {{type}}'
        exit 1
    fi

# Build and install the `sindri` binary into {{install_directory}} (debug or release)
[group('build')]
install type="debug": (build type)
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p '{{install_directory}}'
    cp 'target/{{type}}/sindri' '{{install_directory}}/sindri'
    echo 'Installed sindri ({{type}}) to {{install_directory}}/sindri'

# Check code without building
[group('build')]
check:
    cargo check

# Clean build artifacts
[group('build')]
clean:
    cargo clean

# Format code
[group('quality')]
format:
    cargo fmt

# Run clippy lints
[group('quality')]
lint:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo clippy --lib --tests -- -D warnings
    # Catches `.as_ref().display()`/`.as_ref().to_string_lossy()`, including when rustfmt wraps the
    # chain across lines — a plain single-line grep would miss the wrapped form.
    matches=$(awk '
        FNR == 1 { prev = "" }
        {
            trimmed = $0
            sub(/^[[:space:]]+/, "", trimmed)
            if ($0 ~ /\.as_ref\(\)\.(display|to_string_lossy)\(\)/ ||
                (prev ~ /\.as_ref\(\)[[:space:]]*$/ && trimmed ~ /^\.(display|to_string_lossy)\(\)/)) {
                print FILENAME ":" FNR ": " $0
            }
            prev = $0
        }
    ' $(find src -name '*.rs'))
    if [[ -n "$matches" ]]; then
        echo "$matches" >&2
        echo 'error: found .as_ref().display()/.to_string_lossy() above — route through the type'"'"'s own Display/to_string() instead of AsRef<Path>' >&2
        exit 1
    fi
