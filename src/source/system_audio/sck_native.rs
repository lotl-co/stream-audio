//! Native ScreenCaptureKit backend via Swift FFI.
//!
//! This module provides system audio capture on macOS 13+ using a custom Swift
//! library that wraps ScreenCaptureKit. The Swift code is compiled separately
//! and linked as a dynamic library.
//!
//! # Thread Safety
//!
//! - The callback may be invoked on any thread (Apple's internal queue)
//! - Samples are pushed to a lock-free ring buffer for consumption

#![allow(unsafe_code)] // FFI requires unsafe

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::{SystemAudioBackend, SystemAudioEvent};
use crate::source::CaptureStream;
use crate::StreamAudioError;

// Audio format constants (must match Swift implementation)
const SAMPLE_RATE: u32 = 48000;
const CHANNEL_COUNT: u16 = 2;
/// Ring buffer capacity: 30 seconds of stereo 48kHz audio
const BUFFER_CAPACITY: usize = SAMPLE_RATE as usize * CHANNEL_COUNT as usize * 30;

// Symmetric i16 max for audio conversion
const I16_MAX_SYMMETRIC: f32 = i16::MAX as f32;
const I16_MIN_F32: f32 = i16::MIN as f32;
const I16_MAX_F32: f32 = i16::MAX as f32;

// MARK: - FFI Types

/// Opaque session handle from Swift
#[repr(C)]
struct SCKAudioSessionRef(*mut c_void);

// Safety: The session is managed by Swift and we only pass it through FFI
unsafe impl Send for SCKAudioSessionRef {}

/// Error codes from Swift (must match SCKError in Swift)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SCKError {
    Ok = 0,
    PermissionDenied = 1,
    NoDisplays = 2,
    CaptureFailed = 3,
    AlreadyRunning = 4,
    NotRunning = 5,
    InvalidSession = 6,
}

impl SCKError {
    fn from_i32(value: i32) -> Self {
        match value {
            0 => Self::Ok,
            1 => Self::PermissionDenied,
            2 => Self::NoDisplays,
            3 => Self::CaptureFailed,
            4 => Self::AlreadyRunning,
            5 => Self::NotRunning,
            6 => Self::InvalidSession,
            _ => Self::CaptureFailed,
        }
    }
}

/// Audio callback type matching Swift's SCKAudioCallback
type SCKAudioCallback = unsafe extern "C" fn(
    context: *mut c_void,
    samples: *const f32,
    frame_count: usize,
    channels: u32,
    sample_rate: u32,
);

// MARK: - FFI Declarations

#[link(name = "sck_audio")]
extern "C" {
    fn sck_audio_create(callback: SCKAudioCallback, context: *mut c_void) -> SCKAudioSessionRef;
    fn sck_audio_destroy(session: SCKAudioSessionRef);
    fn sck_audio_start(session: SCKAudioSessionRef) -> i32;
    fn sck_audio_stop(session: SCKAudioSessionRef);
    #[allow(dead_code)] // Available for future use
    fn sck_audio_is_running(session: SCKAudioSessionRef) -> i32;
    fn sck_audio_session_error(session: SCKAudioSessionRef) -> *const c_char;
}

// MARK: - Callback Context

/// Context passed to the Swift callback
struct CallbackContext {
    /// Producer is None until start_capture() is called.
    /// Uses parking_lot::Mutex for faster, non-poisoning locks in the audio callback path.
    producer: Mutex<Option<HeapProd<i16>>>,
    overflow_count: AtomicU64,
    is_active: AtomicBool,
}

/// The C callback that receives audio from Swift
unsafe extern "C" fn audio_callback(
    context: *mut c_void,
    samples: *const f32,
    frame_count: usize,
    channels: u32,
    _sample_rate: u32,
) {
    if context.is_null() || samples.is_null() || frame_count == 0 {
        return;
    }

    let ctx = &*(context as *const CallbackContext);

    if !ctx.is_active.load(Ordering::Relaxed) {
        return;
    }

    let mut guard = ctx.producer.lock();
    let Some(ref mut producer) = *guard else {
        return; // Not yet initialized (start_capture not called)
    };

    let total_samples = frame_count * channels as usize;
    let sample_slice = std::slice::from_raw_parts(samples, total_samples);

    let mut overflow = 0u64;
    for &sample_f32 in sample_slice {
        // Convert f32 [-1.0, 1.0] to i16
        let sample_i16 = (sample_f32 * I16_MAX_SYMMETRIC).clamp(I16_MIN_F32, I16_MAX_F32) as i16;

        if producer.try_push(sample_i16).is_err() {
            overflow += 1;
        }
    }

    if overflow > 0 {
        ctx.overflow_count.fetch_add(overflow, Ordering::Relaxed);
    }
}

// MARK: - Backend Implementation

/// Native ScreenCaptureKit backend for macOS system audio capture.
pub struct SCKNativeBackend {
    session: SCKAudioSessionRef,
    /// The context used by callbacks. We keep an Arc for Rust-side access.
    context: Arc<CallbackContext>,
    /// Raw pointer given to Swift via `Arc::into_raw()`. We must reclaim this
    /// with `Arc::from_raw()` in Drop to avoid leaking the refcount.
    /// This is the same allocation as `context`, just with an extra refcount.
    context_raw_for_swift: *const CallbackContext,
}

// SAFETY: SCKNativeBackend can be sent between threads because:
// - `session` is Send (we impl'd it above - Swift manages thread safety)
// - `context` is Arc<CallbackContext> which is Send
// - `context_raw_for_swift` points to the same Arc allocation, which is Send
unsafe impl Send for SCKNativeBackend {}

impl SCKNativeBackend {
    /// Create a new native ScreenCaptureKit backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Screen Recording permission is not granted
    /// - macOS version is below 13.0
    /// - Session creation fails
    pub fn new() -> Result<Self, StreamAudioError> {
        // Ring buffer is created in start_capture() to avoid allocating 2.7MB
        // that would be immediately discarded
        let context = Arc::new(CallbackContext {
            producer: Mutex::new(None),
            overflow_count: AtomicU64::new(0),
            is_active: AtomicBool::new(false),
        });

        // Clone the Arc and convert to raw pointer for Swift.
        // This increments the refcount, giving Swift "ownership" of one reference.
        // We MUST call Arc::from_raw() in Drop to reclaim this refcount.
        let context_raw_for_swift = Arc::into_raw(Arc::clone(&context));
        let context_ptr = context_raw_for_swift as *mut c_void;

        // Create Swift session
        let session = unsafe { sck_audio_create(audio_callback, context_ptr) };

        if session.0.is_null() {
            // Reclaim the raw pointer since we won't be storing it
            unsafe { drop(Arc::from_raw(context_raw_for_swift)) };
            return Err(StreamAudioError::SystemAudioUnavailable {
                reason: "Failed to create SCK session (macOS 13+ required)".into(),
            });
        }

        Ok(Self {
            session,
            context,
            context_raw_for_swift,
        })
    }

    fn get_error_message(&self) -> Option<String> {
        unsafe {
            let ptr = sck_audio_session_error(SCKAudioSessionRef(self.session.0));
            if ptr.is_null() {
                None
            } else {
                Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
            }
        }
    }
}

impl Drop for SCKNativeBackend {
    fn drop(&mut self) {
        // Signal callbacks to stop processing immediately.
        // Any callback that checks is_active after this point will return early.
        self.context.is_active.store(false, Ordering::SeqCst);

        // Stop capture. Swift's stop() waits synchronously for SCStream.stopCapture(),
        // so after this returns, no NEW callbacks will be dispatched.
        // sck_audio_stop is idempotent - safe to call multiple times.
        unsafe {
            sck_audio_stop(SCKAudioSessionRef(self.session.0));
        }

        // Brief wait for any in-flight callbacks that were already dispatched
        // but haven't yet checked is_active. Swift's GCD queues are fast, so
        // 50ms is generous. The callback checks is_active at the top, so even
        // if timing is off, the callback will early-return without touching
        // the producer.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Destroy the Swift session. This releases Swift's SCKAudioSession object.
        unsafe {
            sck_audio_destroy(SCKAudioSessionRef(self.session.0));
        }

        // Reclaim the Arc refcount we gave to Swift via Arc::into_raw().
        // This decrements the refcount. Combined with our stored Arc dropping,
        // the CallbackContext will be freed when this Drop completes.
        // SAFETY: context_raw_for_swift was created by Arc::into_raw() in new()
        // and has not been reclaimed elsewhere.
        unsafe {
            drop(Arc::from_raw(self.context_raw_for_swift));
        }
    }
}

impl SystemAudioBackend for SCKNativeBackend {
    fn start_capture(self: Box<Self>) -> Result<(CaptureStream, HeapCons<i16>), StreamAudioError> {
        // Create a fresh ring buffer for this capture session
        let ring_buffer = HeapRb::<i16>::new(BUFFER_CAPACITY);
        let (producer, consumer) = ring_buffer.split();

        // Set the producer in the context
        {
            let mut guard = self.context.producer.lock();
            *guard = Some(producer);
        }

        self.context.is_active.store(true, Ordering::SeqCst);

        // Start capture
        let result = unsafe { sck_audio_start(SCKAudioSessionRef(self.session.0)) };
        let error = SCKError::from_i32(result);
        tracing::debug!("sck_audio_start returned: {:?} (raw: {})", error, result);

        match error {
            SCKError::Ok => {
                // Move the entire backend into CaptureStream to keep it alive
                // The backend owns the session handle and context, which must
                // stay alive for the duration of capture.
                let stream = CaptureStream::from_system_audio(self);
                Ok((stream, consumer))
            }
            SCKError::PermissionDenied => {
                self.context.is_active.store(false, Ordering::SeqCst);
                Err(StreamAudioError::SystemAudioPermissionDenied)
            }
            SCKError::NoDisplays => {
                self.context.is_active.store(false, Ordering::SeqCst);
                Err(StreamAudioError::SystemAudioUnavailable {
                    reason: "No displays available".into(),
                })
            }
            _ => {
                self.context.is_active.store(false, Ordering::SeqCst);
                let msg = self
                    .get_error_message()
                    .unwrap_or_else(|| format!("Capture failed with error {result}"));
                Err(StreamAudioError::SystemAudioRuntimeFailure {
                    context: "start capture".into(),
                    cause: msg,
                })
            }
        }
    }

    fn native_config(&self) -> (u32, u16) {
        (SAMPLE_RATE, CHANNEL_COUNT)
    }

    fn name(&self) -> &'static str {
        "SCKNative"
    }

    fn poll_events(&self) -> Vec<SystemAudioEvent> {
        let overflow = self.context.overflow_count.swap(0, Ordering::Relaxed);

        if overflow > 0 {
            vec![SystemAudioEvent::Overflow {
                dropped_frames: overflow,
            }]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sck_error_from_i32() {
        assert_eq!(SCKError::from_i32(0), SCKError::Ok);
        assert_eq!(SCKError::from_i32(1), SCKError::PermissionDenied);
        assert_eq!(SCKError::from_i32(99), SCKError::CaptureFailed);
    }

    #[test]
    fn test_constants() {
        assert_eq!(SAMPLE_RATE, 48000);
        assert_eq!(CHANNEL_COUNT, 2);
        assert!(BUFFER_CAPACITY > 0);
    }
}
