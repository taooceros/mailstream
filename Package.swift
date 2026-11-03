// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Mailstream",
    platforms: [.macOS(.v15)],
    targets: [
        .executableTarget(
            name: "Mailstream",
            path: "Sources/Mailstream",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("WebKit"),
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
            ]
        ),
    ]
)
