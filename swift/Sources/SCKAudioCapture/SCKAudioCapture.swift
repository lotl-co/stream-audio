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

                // Add audio output handler
                try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: DispatchQueue.global(qos: .userInteractive))

                // Start capture
                try await stream.startCapture()

                self.stream = stream
                self.isRunning = true
                result = .ok
            } catch let error as SCStreamError {
                self.setError(error.localizedDescription)
                result = self.mapSCStreamError(error)
            } catch {
                self.setError(error.localizedDescription)
                result = .captureFailed
            }
            semaphore.signal()
        }

        semaphore.wait()
        return result
    }

    func stop() {
        guard isRunning, let stream = stream else { return }

        isRunning = false
        self.stream = nil

        Task {
            try? await stream.stopCapture()
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

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }
        guard isRunning else { return }

        // Extract audio buffer list
        guard let blockBuffer = sampleBuffer.dataBuffer else { return }

        var length = 0
        var dataPointer: UnsafeMutablePointer<Int8>?

        let status = CMBlockBufferGetDataPointer(
            blockBuffer,
            atOffset: 0,
            lengthAtOffsetOut: nil,
            totalLengthOut: &length,
            dataPointerOut: &dataPointer
        )

        guard status == noErr, let data = dataPointer else { return }

        // Audio data is interleaved float samples
        let floatPointer = UnsafeRawPointer(data).assumingMemoryBound(to: Float.self)
        let frameCount = length / (MemoryLayout<Float>.size * channelCount)

        // Call the C callback
        callback(context, floatPointer, frameCount, UInt32(channelCount), UInt32(sampleRate))
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
