# stream-audio justfile
# Run `just --list` to see all available commands

# Default recipe: run all checks
default: check

# Run all checks (CI-safe, no hardware required)
check:
    cargo fmt -- --check
    cargo clippy -- -D warnings
    cargo test
    cargo doc --no-deps

# Run all checks with system audio feature (CI-safe, tests auto-skip)
check-all:
    cargo fmt -- --check
    cargo clippy --features sck-native -- -D warnings
    cargo test --features sck-native
    cargo doc --no-deps --features sck-native

# Format code
fmt:
    cargo fmt

# Run tests (basic, no system audio)
test:
    cargo test

# Run tests with system audio support (auto-skips if unavailable)
test-all:
    cargo test --features sck-native

# Run only system audio tests (useful for local development)
test-system-audio:
    @echo "Running system audio tests (will skip if permissions not granted)..."
    cargo test --features sck-native system_audio -- --nocapture

# Run system audio tests with verbose output
test-system-audio-verbose:
    @echo "Running system audio tests with debug output..."
    RUST_LOG=debug cargo test --features sck-native system_audio -- --nocapture

# Run the interactive example (system audio)
example-interactive:
    cargo run --example interactive --features sck-native

# Run the interactive example with debug logging
example-interactive-debug:
    SCK_AUDIO_DEBUG=1 cargo run --example interactive --features sck-native

# Build Swift library (usually done automatically by build.rs)
build-swift:
    cd swift && bash build.sh

# Clean build artifacts
clean:
    cargo clean
    rm -rf swift/.build
    rm -rf target/swift

# Show system audio test status (checks if tests can run)
check-system-audio-support:
    @echo "Checking system audio test support..."
    @cargo test --features sck-native can_run_returns_valid_result -- --nocapture 2>&1 | grep -E "(SKIP|passed|all checks)"

# Run clippy with all features
clippy:
    cargo clippy --features sck-native -- -D warnings

# Generate documentation
doc:
    cargo doc --no-deps --features sck-native --open
