//! Build script for stream-audio.
//!
//! When the `sck-native` feature is enabled on macOS, this script:
//! 1. Builds the Swift `SCKAudioCapture` library
//! 2. Links the resulting dylib and required frameworks

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    #[cfg(target_os = "macos")]
    if env::var("CARGO_FEATURE_SCK_NATIVE").is_ok() {
        build_swift_library();
    }
}

#[cfg(target_os = "macos")]
fn build_swift_library() {
    // In build scripts, panicking on errors is the standard pattern since
    // there's no way to propagate errors to Cargo except by failing the build.
    #![allow(clippy::expect_used)]

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = PathBuf::from(&manifest_dir);
    let swift_dir = manifest_path.join("swift");
    let lib_dir = manifest_path.join("target/swift");

    println!("cargo:rerun-if-changed=swift/Sources/");
    println!("cargo:rerun-if-changed=swift/Package.swift");
    println!("cargo:rerun-if-changed=swift/build.sh");

    let status = Command::new("bash")
        .arg("build.sh")
        .current_dir(&swift_dir)
        .status()
        .expect("Failed to run swift build.sh - is Swift installed?");

    assert!(
        status.success(),
        "Swift build failed. Check swift/build.sh output."
    );

    let dylib_path = lib_dir.join("libsck_audio.dylib");
    assert!(
        dylib_path.exists(),
        "Swift library not found at {}. Build may have failed.",
        dylib_path.display()
    );

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=sck_audio");

    // Add rpath so the dylib can be found at runtime
    // This is a known Cargo limitation - see rust-lang/cargo#5077
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());

    println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
}
