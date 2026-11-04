import AppKit
import SwiftUI

/// The conversation list: a lazy stack in a scroll view, the floating status capsule, the empty
/// label, and the open bundle's list laid over it.
struct ConversationListView: View {
    /// The margin the AppKit table's inset style put around its rows.
    static let inset: CGFloat = 10

    let model: ConversationListModel
    @FocusState private var focused: Bool
    @Environment(\.appearsActive) private var appearsActive
    @State private var position = ScrollPosition(idType: String.self)
    @State private var anchor: UnitPoint = .top
    /// A new context's content drifts up into place from here.
    @State private var drift: CGFloat = 0
    @State private var fade: Double = 1

    var body: some View {
        ZStack {
            list
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

    private var emphasized: Bool { focused && appearsActive }

    private var list: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(model.items) { item in
                    row(item)
                        .transition(.opacity)
                }
            }
            .scrollTargetLayout()
            .padding(Self.inset)
            .offset(y: drift)
            .opacity(fade)
        }
        .scrollPosition($position, anchor: anchor)
        .onScrollGeometryChange(for: ConversationListGeometry.self) { geometry in
            ConversationListGeometry(top: geometry.visibleRect.minY, contentHeight: geometry.contentSize.height,
                                     containerHeight: geometry.containerSize.height)
        } action: { _, new in
            model.geometryChanged(new)
        }
        .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.99) { model.visibilityChanged($0) }
        .onScrollPhaseChange { _, phase in
            model.scrollingChanged(phase == .tracking || phase == .interacting || phase == .decelerating)
        }
        .onContinuousHover(coordinateSpace: .local) { phase in
            switch phase {
            case .active(let point): model.pointerMoved(point)
            case .ended: model.pointerMoved(nil)
            }
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onKeyPress(keys: [.return, .space, KeyEquivalent("\u{3}")]) { press in
            guard Self.plain(press) else { return .ignored }
            model.openHighlighted()
            return .handled
        }
        .onKeyPress(keys: [.delete, .deleteForward]) { press in
            guard Self.plain(press) else { return .ignored }
            model.performOnHighlight(.trash)
            return .handled
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "esudraf")) { press in
            guard Self.plain(press), let action = Self.singleKey[press.characters] else { return .ignored }
            model.performOnHighlight(action)
            return .handled
        }
        .onMoveCommand { direction in
            switch direction {
            case .up: model.moveHighlight(by: -1)
            case .down: model.moveHighlight(by: 1)
            default: break
            }
        }
        .onChange(of: model.focusRequest) { focused = true }
        .onChange(of: focused, initial: true) { model.isFocused = focused }
        .onChange(of: appearsActive, initial: true) { model.windowActive = appearsActive }
        .onChange(of: model.reveal) { _, reveal in
            guard let reveal else { return }
            anchor = reveal.anchor
            position.scrollTo(id: reveal.id)
        }
        .onChange(of: model.contextGeneration) { driftIn() }
    }

    /// Mail-style single-key commands.
    private static let singleKey: [String: ConversationAction] = [
        "e": .archive, "s": .toggleStar, "u": .toggleRead, "d": .toggleWaiting,
        "r": .reply, "a": .replyAll, "f": .forward,
    ]

    /// No modifier keys held, as the AppKit list read them (the keypad flag doesn't count; SwiftUI
    /// reports no function flag).
    private static func plain(_ press: KeyPress) -> Bool {
        press.modifiers.subtracting(.numericPad).isEmpty
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

    @ViewBuilder private func row(_ item: ConversationListItem) -> some View {
        let hovered = model.hoveredID == item.id
        let highlighted = item.selectionID != nil && model.highlightedID == item.selectionID
        switch item {
        case .section(let title, let count, let accent):
            ListRowSectionHeader(title: title, count: count, accent: accent)
        case .shelf(let bundles):
            ListRowShelf(bundles: bundles, own: model.store.ownAddresses, hovered: hovered) { model.enterBundle($0) }
        case .conversation(let conversation):
            ConversationListMailRow(model: model, conversation: conversation, highlighted: highlighted,
                                    emphasized: emphasized, hovered: hovered) { click(item.id) }
        case .space(let space):
            ConversationListChatRow(model: model, space: space, highlighted: highlighted, emphasized: emphasized,
                                    hovered: hovered) { click(item.id) }
        }
    }

    private func click(_ id: String) {
        focused = true
        model.click(id)
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

/// A conversation's row, reading its content from the store so snippets, pictures, tags and
/// account names redraw it in place.
struct ConversationListMailRow: View {
    let model: ConversationListModel
    let conversation: Conversation
    let highlighted: Bool
    let emphasized: Bool
    let hovered: Bool
    let onClick: () -> Void

    var body: some View {
        let store = model.store
        let context = store.listContext
        let row = store.row(for: conversation)
        let tag = store.tagging.tag(for: conversation)
        let automated = tag.map { $0.category != .people } ?? false
        let photo = store.avatars.image(for: row.avatarKey, automated: automated)
        // Still on initials: look again when pictures arrive.
        if photo == nil { _ = store.avatars.revision }
        let content = ListRowContent(
            row, snippet: store.contents.snippet(for: conversation.latest.ref),
            archivable: ![.archive, .all, .trash].contains(context.role),
            account: context.selection.isUnified
                ? (store.label(account: conversation.account), AccountStyle.color(for: conversation.account, in: store)) : nil,
            tag: tag, photo: photo)
        let id = conversation.id
        return ListRowConversation(content: content, hovered: hovered, onAction: { model.perform($0, id: id) },
                                   avatars: store.avatars)
            .modifier(ListRowChrome(highlighted: highlighted, emphasized: emphasized, hovered: hovered))
            .contentShape(Rectangle())
            .onTapGesture(perform: onClick)
            .contextMenu { menu }
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
struct ConversationListChatRow: View {
    let model: ConversationListModel
    let space: ChatRow
    let highlighted: Bool
    let emphasized: Bool
    let hovered: Bool
    let onClick: () -> Void

    var body: some View {
        let store = model.store
        let photo = space.avatarKey.isEmpty ? nil : store.avatars.image(for: space.avatarKey, automated: false)
        if photo == nil, !space.avatarKey.isEmpty { _ = store.avatars.revision }
        let content = ListRowContent(
            space, account: store.listContext.selection == .allChats
                ? (store.label(account: space.key.account), AccountStyle.chatColor(for: space.key.account, in: store)) : nil,
            photo: photo)
        let key = space.key
        return ListRowConversation(content: content, hovered: hovered, avatars: store.avatars)
        .modifier(ListRowChrome(highlighted: highlighted, emphasized: emphasized, hovered: hovered))
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
