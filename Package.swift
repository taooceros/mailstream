// swift-tools-version: 6.2
import PackageDescription

let swiftSettings: [SwiftSetting] = [
    // An app target: code is main-actor unless it says otherwise, async functions run
    // on their caller's actor unless marked @concurrent.
    .defaultIsolation(MainActor.self),
    .enableUpcomingFeature("NonisolatedNonsendingByDefault"),
    .enableUpcomingFeature("InferIsolatedConformances"),
    .enableUpcomingFeature("ExistentialAny"),
    .enableUpcomingFeature("MemberImportVisibility"),
]

let package = Package(
    name: "Mailstream",
    platforms: [.macOS(.v26)],
    products: [
        .executable(name: "Mailstream", targets: ["Mailstream"]),
        // The engine for other front ends: its C API (`FFI/Exports.swift`) is what the Rust crate
        // in `FFI/` binds, and UniFFI binds that crate for Dart and the rest.
        .library(name: "MailstreamKit", type: .dynamic, targets: ["MailstreamKit"]),
    ],
    targets: [
        // The Turso database engine through its SQLite C API; scripts/turso.sh builds the library.
        .systemLibrary(name: "CTurso", path: "Sources/CTurso"),
        .target(
            name: "MailstreamKit",
            dependencies: ["CTurso"],
            path: "Sources/MailstreamKit",
            swiftSettings: swiftSettings,
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("WebKit"),
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
                .unsafeFlags(["-L\(Context.packageDirectory)/.turso/lib"]),
            ]
        ),
        .executableTarget(
            name: "Mailstream",
            dependencies: ["MailstreamKit"],
            path: "Sources/Mailstream",
            swiftSettings: swiftSettings
        ),
    ]
)
