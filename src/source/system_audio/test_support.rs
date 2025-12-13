//! Test support utilities for system audio tests.
//!
//! Provides runtime detection of whether system audio tests can run on the current machine.
//! Tests using these utilities will skip gracefully in CI or on unsupported configurations.

// Allow eprintln for test skip messages - this is intentional for test output
#![allow(clippy::print_stderr)]

/// Result of checking if system audio tests can run.
#[derive(Debug, Clone)]
pub struct SystemAudioTestSupport {
    /// Whether tests can run on this machine.
    pub can_run: bool,
    /// Human-readable reason for the decision.
    pub reason: &'static str,
}

impl SystemAudioTestSupport {
    /// Returns true if tests should be skipped.
    pub fn should_skip(&self) -> bool {
        !self.can_run
    }

    /// Prints skip message and returns true if tests should be skipped.
    /// Use at the start of a test: `if support.skip_with_message() { return; }`
    pub fn skip_with_message(&self) -> bool {
        if self.should_skip() {
            eprintln!("[SKIP] System audio test: {}", self.reason);
            true
        } else {
            false
        }
    }
}

/// Checks if system audio tests can run on this machine.
///
/// This performs multiple checks:
/// 1. Compile-time: `sck-native` feature and macOS target
/// 2. Runtime: CI environment detection
/// 3. Runtime: Screen Recording permission status
///
/// # Example
///
/// ```ignore
/// use stream_audio::source::system_audio::test_support::can_run_system_audio_tests;
///
/// #[test]
/// fn test_system_audio_capture() {
///     let support = can_run_system_audio_tests();
///     if support.skip_with_message() {
///         return;
///     }
///     // ... test code ...
/// }
/// ```
#[allow(clippy::missing_const_for_fn)] // Can't be const due to env var checks
pub fn can_run_system_audio_tests() -> SystemAudioTestSupport {
    can_run_system_audio_tests_impl()
}

#[cfg(not(all(target_os = "macos", feature = "sck-native")))]
fn can_run_system_audio_tests_impl() -> SystemAudioTestSupport {
    SystemAudioTestSupport {
        can_run: false,
        reason: "sck-native feature not enabled or not macOS",
    }
}

#[cfg(all(target_os = "macos", feature = "sck-native"))]
fn can_run_system_audio_tests_impl() -> SystemAudioTestSupport {
    use crate::platform::permissions::{check_screen_capture_permission, ScreenCapturePermission};

    // Check 1: CI environment detection
    // Most CI systems set one of these environment variables
    const CI_ENV_VARS: [&str; 5] = [
        "CI",
        "GITHUB_ACTIONS",
        "JENKINS_URL",
        "GITLAB_CI",
        "CIRCLECI",
    ];
    for var in CI_ENV_VARS {
        if std::env::var(var).is_ok() {
            return SystemAudioTestSupport {
                can_run: false,
                reason: "running in CI environment (no display/permissions)",
            };
        }
    }

    // Check 2: Permission status (without prompting)
    match check_screen_capture_permission() {
        ScreenCapturePermission::Granted => SystemAudioTestSupport {
            can_run: true,
            reason: "all checks passed",
        },
        ScreenCapturePermission::Denied => SystemAudioTestSupport {
            can_run: false,
            reason: "Screen Recording permission not granted (check System Settings)",
        },
        ScreenCapturePermission::Unknown => SystemAudioTestSupport {
            can_run: false,
            reason: "could not determine permission status",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_support_struct_methods() {
        let supported = SystemAudioTestSupport {
            can_run: true,
            reason: "test",
        };
        assert!(!supported.should_skip());

        let unsupported = SystemAudioTestSupport {
            can_run: false,
            reason: "test",
        };
        assert!(unsupported.should_skip());
    }

    #[test]
    fn test_can_run_returns_valid_result() {
        // Just verify it doesn't panic and returns a valid struct
        let result = can_run_system_audio_tests();
        assert!(!result.reason.is_empty());
    }
}
