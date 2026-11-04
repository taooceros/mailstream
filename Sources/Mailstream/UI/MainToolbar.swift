import SwiftUI

/// The main window's toolbar: back; archive, trash, read, star; reply, reply all, forward; refresh
/// and compose (a new chat while Chat is selected), each group on its own glass. Search comes from
/// `.searchable` on MainView. Labels, symbols and enabled states follow the selection through
/// `ConversationAction.State`.
struct MainToolbar: ToolbarContent {
    let model: MainModel

    var body: some ToolbarContent {
        ToolbarItem(placement: .navigation) {
            Button("Back", systemImage: "chevron.left") { model.goBack() }
                .disabled(!model.canGoBack)
        }
        ToolbarSpacer(.flexible)
        ToolbarItemGroup {
            action(.archive)
            action(.trash)
            action(.toggleRead)
            action(.toggleStar)
        }
        ToolbarSpacer(.fixed)
        ToolbarItemGroup {
            action(.reply)
            action(.replyAll)
            action(.forward)
        }
        ToolbarSpacer(.fixed)
        ToolbarItemGroup {
            Button("Get New Mail", systemImage: "arrow.clockwise") { model.refresh() }
            if model.store.selection.isChat {
                Button("New Chat", systemImage: "square.and.pencil") { model.startChat() }
                    .disabled(!model.canStartChat)
            } else {
                Button("New Message", systemImage: "square.and.pencil") { model.compose() }
            }
        }
    }

    private func action(_ action: ConversationAction) -> some View {
        let label = action.label(in: model.selectedState)
        // The toolbar button stays enabled for Trash while text is edited; only ⌘⌫ yields to the text.
        let enabled = action == .trash ? model.selected != nil : model.isEnabled(action)
        return Button(label.title, systemImage: label.symbol) { model.perform(action) }
            .disabled(!enabled)
    }
}
