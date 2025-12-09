#!/bin/bash
# Build the SCKAudioCapture Swift library
set -e
cd "$(dirname "$0")"

# Build release configuration
swift build -c release

# Determine the library output path
LIB_PATH=".build/release/libSCKAudioCapture.dylib"

if [ ! -f "$LIB_PATH" ]; then
    echo "Error: Library not found at $LIB_PATH"
    echo "Available files in .build/release:"
    ls -la .build/release/ 2>/dev/null || echo "(directory not found)"
    exit 1
fi

# Copy to target location for Cargo to find
mkdir -p ../target/swift
cp "$LIB_PATH" ../target/swift/libsck_audio.dylib

# Fix the install name so dyld can find it at runtime
# The library's internal name needs to match what we link against
install_name_tool -id "@rpath/libsck_audio.dylib" ../target/swift/libsck_audio.dylib

echo "Built: target/swift/libsck_audio.dylib"
