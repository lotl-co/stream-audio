//! macOS permission checks for audio capture.
//!
//! Core Audio Taps (used for system audio loopback capture) require Screen Recording
//! permission on macOS. Without this permission, audio streams will receive silent
//! (all-zero) buffers.

/// Permission status for screen/audio capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenCapturePermission {
    /// Permission has been granted.
    Granted,
    /// Permission has been denied or not yet requested.
    Denied,
    /// Unable to determine permission status (API unavailable).
    Unknown,
}

// FFI declarations for CoreGraphics permission APIs (macOS 10.15+)
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    /// Returns true if the app has screen capture permission.
    /// Does not prompt the user.
    fn CGPreflightScreenCaptureAccess() -> bool;

    /// Requests screen capture permission from the user.
    /// Shows a system prompt if permission hasn't been determined.
    /// Returns true if permission is granted.
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Checks if Screen Recording permission is currently granted.
///
/// This does NOT prompt the user - it only checks the current status.
/// Use [`request_screen_capture_permission`] to prompt.
///
/// # Returns
///
/// - `Granted` if the app has permission
/// - `Denied` if permission was denied or not yet requested
///
/// # Example
///
/// ```no_run
/// use stream_audio::platform::permissions::{check_screen_capture_permission, ScreenCapturePermission};
///
/// match check_screen_capture_permission() {
///     ScreenCapturePermission::Granted => println!("Ready to capture!"),
///     ScreenCapturePermission::Denied => println!("Please grant Screen Recording permission"),
///     ScreenCapturePermission::Unknown => println!("Could not check permission"),
/// }
/// ```
#[must_use]
pub fn check_screen_capture_permission() -> ScreenCapturePermission {
    // SAFETY: CGPreflightScreenCaptureAccess is a safe CoreGraphics API
    // that only reads system state. Available on macOS 10.15+.
    #[allow(unsafe_code)]
    let granted = unsafe { CGPreflightScreenCaptureAccess() };

    if granted {
        ScreenCapturePermission::Granted
    } else {
        ScreenCapturePermission::Denied
    }
}

/// Requests Screen Recording permission from the user.
///
/// If permission hasn't been determined yet, this shows a system prompt.
/// If already granted or denied, returns the current status without prompting.
///
/// **Note:** On macOS, the app typically needs to be restarted after
/// permission is granted for it to take effect.
///
/// # Returns
///
/// - `Granted` if permission was granted (either now or previously)
/// - `Denied` if permission was denied
///
/// # Example
///
/// ```no_run
/// use stream_audio::platform::permissions::{request_screen_capture_permission, ScreenCapturePermission};
///
/// if request_screen_capture_permission() == ScreenCapturePermission::Denied {
///     eprintln!("Screen Recording permission required for system audio capture.");
///     eprintln!("Please enable in System Settings > Privacy & Security > Screen Recording");
/// }
/// ```
#[must_use]
pub fn request_screen_capture_permission() -> ScreenCapturePermission {
    // SAFETY: CGRequestScreenCaptureAccess is a safe CoreGraphics API
    // that may show a system dialog. Available on macOS 10.15+.
    #[allow(unsafe_code)]
    let granted = unsafe { CGRequestScreenCaptureAccess() };

    if granted {
        ScreenCapturePermission::Granted
    } else {
        ScreenCapturePermission::Denied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_check_returns_valid_status() {
        // Just verify it doesn't panic - actual result depends on system state
        let status = check_screen_capture_permission();
        assert!(matches!(
            status,
            ScreenCapturePermission::Granted | ScreenCapturePermission::Denied
        ));
    }
}
