# List available recipes
default:
    @just --list

# Run all tests (unit + integration)
test:
    cargo test

# Run only unit tests
test-unit:
    cargo test --lib --bins

# Run only integration tests
test-integration:
    cargo test --tests

# Run tests with coverage summary
cov:
    cargo llvm-cov

# Coverage for unit tests only
cov-unit:
    cargo llvm-cov --lib --bins

# Coverage for integration tests only
cov-integration:
    cargo llvm-cov --tests

# Generate HTML coverage report
cov-html:
    cargo llvm-cov --html
    @echo "Report: target/llvm-cov/html/index.html"

# Open HTML coverage report in browser
cov-open:
    cargo llvm-cov --open

# Build in release mode
build:
    cargo build --release

# Check code without building
check:
    cargo check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Format code
fmt:
    cargo fmt

# Clean build artifacts
clean:
    cargo clean
