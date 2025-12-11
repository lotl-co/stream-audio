import Foundation
import ScreenCaptureKit
import CoreMedia
import AVFoundation

// MARK: - C Types (must match sck_audio.h)

public typealias SCKAudioCallback = @convention(c) (
    UnsafeMutableRawPointer?,  // context
    UnsafePointer<Float>?,     // samples
    Int,                        // frame_count
    UInt32,                     // channels
    UInt32                      // sample_rate
) -> Void

@frozen
public enum SCKError: Int32 {
    case ok = 0
    case permissionDenied = 1
    case noDisplays = 2
    case captureFailed = 3
    case alreadyRunning = 4
    case notRunning = 5
    case invalidSession = 6
}

// MARK: - Audio Session

@available(macOS 13.0, *)
final class SCKAudioSession: NSObject, SCStreamOutput, SCStreamDelegate {
    private var stream: SCStream?
    private let callback: SCKAudioCallback
    private let context: UnsafeMutableRawPointer?
    private var isRunning = false
    private var lastErrorMessage: String = ""

    // Store C string for FFI return
    private var lastErrorCString: [CChar] = []

    // Audio format constants
    private let sampleRate: Int = 48000
    private let channelCount: Int = 2

    init(callback: @escaping SCKAudioCallback, context: UnsafeMutableRawPointer?) {
        self.callback = callback
        self.context = context
        super.init()
    }

    /// Synchronous start - blocks until capture starts or fails
    func start() -> SCKError {
        guard !isRunning else {
            setError("Capture already running")
            return .alreadyRunning
        }

        let semaphore = DispatchSemaphore(value: 0)
        var result: SCKError = .ok

        Task {
            do {
                // Get available content
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)

                guard let display = content.displays.first else {
                    self.setError("No displays available")
                    result = .noDisplays
                    semaphore.signal()
                    return
                }

                // Create filter for all audio from all apps
                let filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])

                // Configure for audio-only capture (minimal video)
                let config = SCStreamConfiguration()
                config.width = 2   // Minimum valid size
                config.height = 2
                config.minimumFrameInterval = CMTime(value: 1, timescale: 1) // 1 FPS minimum
                config.capturesAudio = true
                config.sampleRate = self.sampleRate
                config.channelCount = self.channelCount
                config.excludesCurrentProcessAudio = true  // Avoid feedback

                // Create stream
                let stream = SCStream(filter: filter, configuration: config, delegate: self)

                // Add output handlers - BOTH screen and audio are needed
                // Without screen output, audio callbacks won't fire (macOS Sonoma+)
                try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: DispatchQueue.global(qos: .utility))
                try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: DispatchQueue.global(qos: .userInteractive))

                // Store stream and set running BEFORE starting capture
                // (callbacks may arrive before startCapture() returns)
                self.stream = stream
                self.isRunning = true

                // Start capture
                try await stream.startCapture()
                result = .ok
            } catch let error as SCStreamError {
                self.isRunning = false
                self.stream = nil
                self.setError(error.localizedDescription)
                result = self.mapSCStreamError(error)
            } catch {
                self.isRunning = false
                self.stream = nil
                self.setError(error.localizedDescription)
                result = .captureFailed
            }
            semaphore.signal()
        }

        semaphore.wait()
        return result
    }

    func stop() {
        // Idempotent - safe to call multiple times
        let wasRunning = isRunning
        isRunning = false

        guard let stream = stream else { return }
        self.stream = nil

        if wasRunning {
            // Use semaphore to wait for async stop to complete
            let semaphore = DispatchSemaphore(value: 0)
            Task {
                try? await stream.stopCapture()
                semaphore.signal()
            }
            // Wait up to 1 second for stop to complete
            _ = semaphore.wait(timeout: .now() + 1.0)
        }
    }

    var running: Bool { isRunning }

    var lastError: UnsafePointer<CChar>? {
        guard !lastErrorCString.isEmpty else { return nil }
        return lastErrorCString.withUnsafeBufferPointer { $0.baseAddress }
    }

    private func setError(_ message: String) {
        lastErrorMessage = message
        lastErrorCString = Array(message.utf8CString)
    }

    private func mapSCStreamError(_ error: SCStreamError) -> SCKError {
        switch error.code {
        case .userDeclined:
            return .permissionDenied
        case .attemptToStartStreamState, .attemptToStopStreamState:
            return .alreadyRunning
        default:
            return .captureFailed
        }
    }

    // MARK: - SCStreamOutput

    private var callbackCount = 0
    private var lastLogTime: Date = Date()

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        // Ignore screen callbacks (needed for audio to work on macOS Sonoma+)
        guard type == .audio else { return }

        callbackCount += 1
        let frameCount = CMSampleBufferGetNumSamples(sampleBuffer)

        // Log every callback for first 2 seconds, then every 50th callback
        let timeSinceStart = Date().timeIntervalSince(lastLogTime)
        if callbackCount <= 20 || callbackCount % 50 == 0 {
            NSLog("[SCK] Callback #%d: frames=%d, running=%d, time=%.2fs",
                  callbackCount, frameCount, isRunning ? 1 : 0, timeSinceStart)
        }

        guard isRunning else {
            NSLog("[SCK] Callback #%d REJECTED: isRunning=false", callbackCount)
            return
        }

        // Use Apple's proper APIs to handle any audio format adaptively
        do {
            try sampleBuffer.withAudioBufferList { audioBufferList, blockBuffer in
                // Get actual format from the sample buffer (adaptive detection)
                guard let formatDesc = sampleBuffer.formatDescription,
                      let asbdPtr = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc) else {
                    return
                }
                var asbd = asbdPtr.pointee
                guard let format = AVAudioFormat(streamDescription: &asbd) else {
                    return
                }

                let frameCount = AVAudioFrameCount(CMSampleBufferGetNumSamples(sampleBuffer))
                guard frameCount > 0 else { return }

                // Create PCM buffer with detected format
                guard let pcmBuffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount) else {
                    return
                }
                pcmBuffer.frameLength = frameCount

                // Copy PCM data into the buffer (handles format conversion)
                let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(
                    sampleBuffer,
                    at: 0,
                    frameCount: Int32(frameCount),
                    into: pcmBuffer.mutableAudioBufferList
                )
                guard status == noErr else { return }

                // Extract float data (AVAudioPCMBuffer provides float32 via floatChannelData)
                guard let floatData = pcmBuffer.floatChannelData else { return }

                let channels = Int(format.channelCount)
                let frames = Int(frameCount)

                // Convert non-interleaved to interleaved for our callback contract
                var interleaved = [Float](repeating: 0, count: frames * channels)
                for frame in 0..<frames {
                    for ch in 0..<channels {
                        interleaved[frame * channels + ch] = floatData[ch][frame]
                    }
                }

                // Call the C callback with interleaved float32 data
                interleaved.withUnsafeBufferPointer { ptr in
                    self.callback(self.context, ptr.baseAddress, frames, UInt32(channels), UInt32(format.sampleRate))
                }
            }
        } catch {
            // Silently ignore errors - audio capture continues
        }
    }

    // MARK: - SCStreamDelegate

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        setError("Stream stopped: \(error.localizedDescription)")
        isRunning = false
        self.stream = nil
    }
}

// MARK: - C Exports

@_cdecl("sck_audio_create")
public func sck_audio_create(
    callback: SCKAudioCallback?,
    context: UnsafeMutableRawPointer?
) -> UnsafeMutableRawPointer? {
    guard #available(macOS 13.0, *) else { return nil }
    guard let callback = callback else { return nil }

    let session = SCKAudioSession(callback: callback, context: context)
    return Unmanaged.passRetained(session).toOpaque()
}

@_cdecl("sck_audio_destroy")
public func sck_audio_destroy(session: UnsafeMutableRawPointer?) {
    guard #available(macOS 13.0, *) else { return }
    guard let session = session else { return }

    let obj = Unmanaged<SCKAudioSession>.fromOpaque(session).takeRetainedValue()
    obj.stop()
    // ARC releases when obj goes out of scope
}

@_cdecl("sck_audio_start")
public func sck_audio_start(session: UnsafeMutableRawPointer?) -> Int32 {
    guard #available(macOS 13.0, *) else { return SCKError.captureFailed.rawValue }
    guard let session = session else { return SCKError.invalidSession.rawValue }

    let obj = Unmanaged<SCKAudioSession>.fromOpaque(session).takeUnretainedValue()
    return obj.start().rawValue
}

@_cdecl("sck_audio_stop")
public func sck_audio_stop(session: UnsafeMutableRawPointer?) {
    guard #available(macOS 13.0, *) else { return }
    guard let session = session else { return }

    let obj = Unmanaged<SCKAudioSession>.fromOpaque(session).takeUnretainedValue()
    obj.stop()
}

@_cdecl("sck_audio_is_running")
public func sck_audio_is_running(session: UnsafeMutableRawPointer?) -> Int32 {
    guard #available(macOS 13.0, *) else { return 0 }
    guard let session = session else { return 0 }

    let obj = Unmanaged<SCKAudioSession>.fromOpaque(session).takeUnretainedValue()
    return obj.running ? 1 : 0
}

@_cdecl("sck_audio_session_error")
public func sck_audio_session_error(session: UnsafeMutableRawPointer?) -> UnsafePointer<CChar>? {
    guard #available(macOS 13.0, *) else { return nil }
    guard let session = session else { return nil }

    let obj = Unmanaged<SCKAudioSession>.fromOpaque(session).takeUnretainedValue()
    return obj.lastError
}
