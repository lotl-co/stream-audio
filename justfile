# Run all checks
check:
    cargo fmt -- --check
    cargo clippy -- -D warnings
    cargo test
    cargo doc --no-deps

# Format code
fmt:
    cargo fmt
