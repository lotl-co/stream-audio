// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "SCKAudioCapture",
    platforms: [.macOS(.v12)],  // ScreenCaptureKit requires macOS 12.3+
    products: [
        .library(
            name: "SCKAudioCapture",
            type: .dynamic,
            targets: ["SCKAudioCapture"]
        ),
    ],
    targets: [
        .target(
            name: "SCKAudioCapture",
            path: "Sources/SCKAudioCapture",
            publicHeadersPath: "include",
            linkerSettings: [
                .linkedFramework("ScreenCaptureKit"),
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("AVFoundation"),
            ]
        ),
    ]
)
