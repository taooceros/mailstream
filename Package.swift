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

let turso = LinkerSetting.unsafeFlags(["-L\(Context.packageDirectory)/.turso/lib"])

#if os(Windows)
// Windows builds the engine alone, for the Flutter front end: no AppKit app (`App/`, `UI/`) and no
// control socket or `mailstream` CLI, which talk over a Unix socket to that app. The system calls
// the Mac makes through Apple frameworks go through `CWindows`; SHA-256 through swift-crypto.
let package = Package(
    name: "Mailstream",
    products: [
        .library(name: "MailstreamKit", type: .dynamic, targets: ["MailstreamKit"]),
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-crypto.git", exact: "5.1.1"),
    ],
    targets: [
        .systemLibrary(name: "CTurso", path: "Sources/CTurso"),
        .target(
            name: "CWindows",
            path: "Sources/CWindows",
            linkerSettings: ["crypt32", "dnsapi", "icu", "ole32", "oleaut32", "shell32", "windowscodecs", "ws2_32", "advapi32"]
                .map { .linkedLibrary($0) }
        ),
        .target(
            name: "MailstreamKit",
            dependencies: ["CTurso", "CWindows", .product(name: "Crypto", package: "swift-crypto")],
            path: "Sources/MailstreamKit",
            exclude: ["App", "UI", "Control/CLI.swift", "Control/ControlSocket.swift"],
            swiftSettings: swiftSettings,
            // The Turso static library is Rust's: it needs the system libraries Rust's std links.
            linkerSettings: [turso] + ["bcrypt", "ntdll", "userenv", "ws2_32", "advapi32", "kernel32", "synchronization", "dbghelp"]
                .map { .linkedLibrary($0) }
        ),
        .testTarget(
            name: "MailstreamKitTests",
            dependencies: ["MailstreamKit"],
            swiftSettings: swiftSettings
        ),
    ]
)
#else
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
                turso,
            ]
        ),
        .executableTarget(
            name: "Mailstream",
            dependencies: ["MailstreamKit"],
            path: "Sources/Mailstream",
            swiftSettings: swiftSettings
        ),
        .testTarget(
            name: "MailstreamKitTests",
            dependencies: ["MailstreamKit"],
            swiftSettings: swiftSettings
        ),
    ]
)
#endif
