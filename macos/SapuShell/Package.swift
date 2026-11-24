// swift-tools-version:5.9
// The SwiftUI shell around sapu-core. Build it with ../build-shell.sh, which
// first builds the Rust library and generates `Sources/SapuFFI/sapu_ffi.swift`
// and `build/sapu_ffiFFI.xcframework`.
import PackageDescription

let package = Package(
    name: "SapuShell",
    platforms: [.macOS(.v13)],
    targets: [
        .binaryTarget(name: "sapu_ffiFFI", path: "build/sapu_ffiFFI.xcframework"),
        .target(
            name: "SapuFFI",
            dependencies: ["sapu_ffiFFI"],
            path: "Sources/SapuFFI",
            exclude: ["README.md"],
            linkerSettings: [
                .linkedFramework("Foundation"),
                .linkedFramework("AppKit"),
            ]
        ),
        .executableTarget(name: "SapuShell", dependencies: ["SapuFFI"], path: "Sources/SapuShell"),
    ]
)
