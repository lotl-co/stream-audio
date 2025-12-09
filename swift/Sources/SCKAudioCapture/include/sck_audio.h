/**
 * sck_audio.h - ScreenCaptureKit Audio Capture C API
 *
 * Minimal C interface for capturing system audio on macOS via ScreenCaptureKit.
 * Designed for FFI consumption from Rust.
 *
 * Thread Safety:
 * - sck_audio_create/destroy must be called from the same thread
 * - sck_audio_start/stop are thread-safe
 * - The callback may be invoked on ANY thread (Apple's internal queue)
 * - The samples pointer is valid ONLY during the callback invocation
 * - Do not block in the callback - copy data and return quickly
 */

#ifndef SCK_AUDIO_H
#define SCK_AUDIO_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Opaque handle to audio capture session
typedef struct SCKAudioSession* SCKAudioSessionRef;

/// Callback invoked when audio samples are available.
///
/// @param context User-provided context pointer (passed to sck_audio_create)
/// @param samples Interleaved float samples in range [-1.0, 1.0] (borrowed, do not store)
/// @param frame_count Number of audio frames (total samples = frame_count * channels)
/// @param channels Number of audio channels (typically 2 for stereo)
/// @param sample_rate Sample rate in Hz (typically 48000)
///
/// @note This callback may be invoked on any thread. The samples pointer is only
///       valid during the callback. Copy data to your own buffer before returning.
typedef void (*SCKAudioCallback)(
    void* context,
    const float* samples,
    size_t frame_count,
    uint32_t channels,
    uint32_t sample_rate
);

/// Error codes returned by SCK functions
typedef enum {
    SCK_OK = 0,
    SCK_ERROR_PERMISSION_DENIED = 1,
    SCK_ERROR_NO_DISPLAYS = 2,
    SCK_ERROR_CAPTURE_FAILED = 3,
    SCK_ERROR_ALREADY_RUNNING = 4,
    SCK_ERROR_NOT_RUNNING = 5,
    SCK_ERROR_INVALID_SESSION = 6,
} SCKError;

/// Create a new audio capture session.
///
/// @param callback Function to receive audio samples (required)
/// @param context User context passed to callback (may be NULL)
/// @return Session handle, or NULL on failure. Call sck_audio_session_error for details.
SCKAudioSessionRef sck_audio_create(SCKAudioCallback callback, void* context);

/// Destroy session and free resources.
///
/// @param session Session handle from sck_audio_create. NULL is safely ignored.
void sck_audio_destroy(SCKAudioSessionRef session);

/// Start capturing audio. Blocks until capture starts or fails.
///
/// @param session Session handle
/// @return SCK_OK on success, error code otherwise
SCKError sck_audio_start(SCKAudioSessionRef session);

/// Stop capturing audio.
///
/// @param session Session handle
void sck_audio_stop(SCKAudioSessionRef session);

/// Check if capture is currently running.
///
/// @param session Session handle
/// @return 1 if running, 0 otherwise
int sck_audio_is_running(SCKAudioSessionRef session);

/// Get last error message for this session (for debugging).
///
/// @param session Session handle
/// @return Error string, valid until next call on this session. Do not free.
const char* sck_audio_session_error(SCKAudioSessionRef session);

#ifdef __cplusplus
}
#endif

#endif // SCK_AUDIO_H
