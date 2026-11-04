import SwiftUI

/// The app's scenes and menu bar. Started from `main.swift` after the CLI check, so no `@main`.
struct MailstreamApp: App {
    static let mainWindowID = "main"

    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        Window("Mailstream", id: Self.mainWindowID) {
            MainView(model: delegate.main)
        }
        .defaultSize(width: 1280, height: 820)
        // Opens at every launch, like the AppKit window did; it never restored its own state.
        .defaultLaunchBehavior(.presented)
        .restorationBehavior(.disabled)
        .windowToolbarStyle(.unified)
        .commands {
            MainCommands(model: delegate.main, showSettings: delegate.showSettings)
        }
    }
}
