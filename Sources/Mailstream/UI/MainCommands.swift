import AppKit
import SwiftUI

/// The menu bar beyond the system's standard items: Settings, New Message, New Chat, Find, the View
/// menu's navigation and tagging commands, and the Message menu. Window commands are live only
/// while the main window is key, as they were when the responder chain routed them.
struct MainCommands: Commands {
    let model: MainModel
    let showSettings: () -> Void

    var body: some Commands {
        CommandGroup(replacing: .appSettings) {
            Button("Settings…", action: showSettings)
                .keyboardShortcut(",")
        }
        CommandGroup(replacing: .newItem) {
            Button("New Message") { model.compose() }
                .keyboardShortcut("n")
            Button("New Chat") { model.startChat() }
                .keyboardShortcut("n", modifiers: [.command, .option])
                .disabled(!model.isKeyWindow || !model.canStartChat)
        }
        CommandGroup(after: .pasteboard) {
            // AppKit's plain-text paste; the responder chain decides whether it applies.
            Button("Paste and Match Style") {
                NSApp.sendAction(#selector(NSTextView.pasteAsPlainText(_:)), to: nil, from: nil)
            }
            .keyboardShortcut("v", modifiers: [.command, .option, .shift])
        }
        CommandGroup(replacing: .textEditing) {
            Button("Find…") { model.focusSearch() }
                .keyboardShortcut("f")
                .disabled(!model.isKeyWindow)
        }
        SidebarCommands()
        CommandGroup(after: .sidebar) {
            Button("Back") { model.goBack() }
                .keyboardShortcut("[")
                .disabled(!model.isKeyWindow || !model.canGoBack)
            Button("Get New Mail") { model.refresh() }
                .keyboardShortcut("n", modifiers: [.command, .shift])
                .disabled(!model.isKeyWindow)
            Divider()
            Toggle("Auto-Tag with AI", isOn: autoTag)
                .disabled(!model.isKeyWindow)
            Button("Re-tag All Conversations") { model.retagAll() }
                .disabled(!model.isKeyWindow || !model.store.tagging.isEnabled)
            Divider()
        }
        CommandMenu("Message") {
            entry(.reply, "r")
            entry(.replyAll, "r", [.command, .shift])
            entry(.forward, "f", [.command, .shift])
            Divider()
            entry(.archive, "a", [.command, .control])
            entry(.trash, .delete)
            entry(.restore, nil)
            Divider()
            entry(.toggleRead, "u", [.command, .shift])
            entry(.toggleStar, "l", [.command, .shift])
            entry(.toggleWaiting, "d", [.command, .shift])
        }
    }

    private var autoTag: Binding<Bool> {
        Binding { model.store.tagging.isEnabled } set: { _ in model.toggleAITagging() }
    }

    /// One Message menu item, titled for the selection (Mark as Read / Unread, Star / Unstar, …).
    @ViewBuilder
    private func entry(_ action: ConversationAction, _ key: KeyEquivalent?, _ modifiers: EventModifiers = .command) -> some View {
        let button = Button(action.label(in: model.selectedState).title) { model.perform(action) }
            .disabled(!model.isKeyWindow || !model.isEnabled(action))
        if let key {
            button.keyboardShortcut(key, modifiers: modifiers)
        } else {
            button
        }
    }
}
