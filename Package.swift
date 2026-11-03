// swift-tools-version: 6.0
import PackageDescription

/// `libmailstream_engine.a`, built from `Engine/` by `scripts/engine.sh`.
let engineLibrary = Context.packageDirectory + "/Engine/target/release"

let package = Package(
    name: "Mailstream",
    platforms: [.macOS(.v15)],
    targets: [
        .systemLibrary(name: "CMailstreamEngine", path: "Engine/include"),
        .executableTarget(
            name: "Mailstream",
            dependencies: ["CMailstreamEngine"],
            path: "Sources/Mailstream",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("WebKit"),
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
                .linkedLibrary("iconv"),
                .unsafeFlags(["-L", engineLibrary]),
            ]
        ),
    ]
)
