import AppKit
import SwiftUI

/// The conversation list: its lines in an AppKit scroll view, the floating status capsule, the
/// empty label, and the open bundle's list laid over it.
struct ConversationListView: View {
    /// The margin the AppKit table's inset style put around its rows.
    static let inset: CGFloat = 10

    let model: ConversationListModel
    @Environment(\.appearsActive) private var appearsActive
    /// A new context's content drifts up into place from here.
    @State private var drift: CGFloat = 0
    @State private var fade: Double = 1

    var body: some View {
        ZStack {
            Color.clear
                .background { list }
                .overlay {
                    if let empty = model.emptyText {
                        Text(empty)
                            .font(.design(.headline, weight: .medium))
                            .foregroundStyle(Color.design(.faint))
                            .multilineTextAlignment(.center)
                            .allowsHitTesting(false)
                    }
                }
                .overlay(alignment: .bottom) { status }
            if let layer = model.bundleLayer {
                ConversationListBundlePage(layer: layer)
                    .transition(.asymmetric(
                        insertion: .scale(scale: 0.94)
                            .animation(.interpolatingSpring(mass: 1, stiffness: 300, damping: 32))
                            .combined(with: .opacity.animation(.house(.standard))),
                        removal: .scale(scale: 0.96).combined(with: .opacity)))
                    .zIndex(1)
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { model.pageWidth = $0 }
    }

    /// The scroll view runs up under the toolbar, which the lines show through; the overlays stay
    /// below it.
    private var list: some View {
        ConversationListScroller(model: model, itemsSerial: model.itemsSerial, reveal: model.reveal,
                                 focusRequest: model.focusRequest, appearsActive: appearsActive)
            .ignoresSafeArea(.container, edges: .top)
            .offset(y: drift)
            .opacity(fade)
            .onChange(of: appearsActive, initial: true) { model.windowActive = appearsActive }
            .onChange(of: model.contextGeneration) { driftIn() }
    }

    /// A different mailbox or search: the new content fades and drifts up into place.
    private func driftIn() {
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            drift = 10
            fade = 0.4
        }
        Task { @MainActor in
            withAnimation(.interpolatingSpring(mass: 1, stiffness: 320, damping: 34)) { drift = 0 }
            withAnimation(.house(.brief)) { fade = 1 }
        }
    }

    @ViewBuilder private var status: some View {
        ZStack {
            if let line = model.statusLine {
                ListRowStatusPill(text: line.text, busy: line.busy)
                    .transition(.asymmetric(insertion: .opacity.combined(with: .offset(y: 8)),
                                            removal: .opacity.animation(.house(.standard))))
            }
        }
        .padding(.horizontal, 20)
        .padding(.bottom, 14)
        .animation(.house(.brief), value: model.statusLine != nil)
    }
}

/// The open bundle's list as a page over the inbox: opaque, following the back swipe.
struct ConversationListBundlePage: View {
    let layer: ConversationListBundleLayer

    var body: some View {
        let shadow = Design.Shadow.page
        ConversationListView(model: layer.model)
            .background(Color.design(.page))
            .compositingGroup()
            .contentShape(Rectangle())
            .shadow(color: .black.opacity(layer.lifted ? Double(shadow.opacity) : 0), radius: shadow.radius)
            .offset(x: layer.offset)
    }
}

/// The Smart Inbox shelf, lifting its tiles while the pointer is over it.
struct ConversationListShelfRow: View {
    let bundles: [ConversationListBundle]
    let own: Set<String>
    let hovered: Bool
    let onOpen: (MailCategory) -> Void

    var body: some View {
        ListRowShelf(bundles: bundles, own: own, hovered: hovered, onOpen: onOpen)
    }
}

/// A conversation's row, drawing the content its state holds. Equal while its state, hover and
/// conversation are; the click is the same line's either way.
struct ConversationListMailRow: View, Equatable {
    let model: ConversationListModel
    let conversation: Conversation
    let state: ListRowState
    let emphasized: Bool
    let hovered: Bool
    let onClick: () -> Void

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.state === rhs.state && lhs.emphasized == rhs.emphasized && lhs.hovered == rhs.hovered
            && lhs.conversation == rhs.conversation
    }

    var body: some View {
        if let content = state.content {
            let id = conversation.id
            ListRowConversation(content: content, hovered: hovered, onAction: { model.perform($0, id: id) },
                                avatars: model.store.avatars)
                .modifier(ListRowChrome(highlighted: state.highlighted, emphasized: emphasized, hovered: hovered))
                .contentShape(Rectangle())
                .onTapGesture(perform: onClick)
                .contextMenu { menu }
        }
    }

    @ViewBuilder private var menu: some View {
        let store = model.store
        let state = ConversationAction.State(conversation, tag: store.tagging.tag(for: conversation))
        item(.reply, state)
        item(.replyAll, state)
        item(.forward, state)
        Divider()
        item(.toggleRead, state)
        item(.toggleStar, state)
        if store.tagging.canMarkWaiting(conversation) { item(.toggleWaiting, state) }
        Divider()
        if store.canArchive(conversation) { item(.archive, state) }
        if store.canRestore(conversation) { item(.restore, state) }
        item(.trash, state)
    }

    private func item(_ action: ConversationAction, _ state: ConversationAction.State) -> some View {
        let label = action.label(in: state)
        let id = conversation.id
        return Button { model.perform(action, id: id) } label: { Label(label.title, systemImage: label.symbol) }
    }
}

/// A chat space's row.
struct ConversationListChatRow: View, Equatable {
    let model: ConversationListModel
    let space: ChatRow
    let state: ListRowState
    let emphasized: Bool
    let hovered: Bool
    let onClick: () -> Void

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.state === rhs.state && lhs.emphasized == rhs.emphasized && lhs.hovered == rhs.hovered
            && lhs.space == rhs.space
    }

    var body: some View {
        if let content = state.content {
            let key = space.key
            ListRowConversation(content: content, hovered: hovered, avatars: model.store.avatars)
                .modifier(ListRowChrome(highlighted: state.highlighted, emphasized: emphasized, hovered: hovered))
                .contentShape(Rectangle())
                .onTapGesture(perform: onClick)
                .contextMenu {
                    Button("Open") { model.openChat(key) }
                    if space.isUnread {
                        let label = ConversationAction.toggleRead.label(in: .init(unread: true))
                        Button { model.markChatRead(key) } label: { Label(label.title, systemImage: label.symbol) }
                    }
                }
        }
    }
}
