// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Mailstream",
    platforms: [.macOS(.v26)],
    targets: [
        .executableTarget(
            name: "Mailstream",
            path: "Sources/Mailstream",
            swiftSettings: [
                // An app target: code is main-actor unless it says otherwise, async functions run
                // on their caller's actor unless marked @concurrent.
                .defaultIsolation(MainActor.self),
                .enableUpcomingFeature("NonisolatedNonsendingByDefault"),
                .enableUpcomingFeature("InferIsolatedConformances"),
                .enableUpcomingFeature("ExistentialAny"),
                .enableUpcomingFeature("MemberImportVisibility"),
            ],
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("WebKit"),
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
            ]
        ),
    ]
)
