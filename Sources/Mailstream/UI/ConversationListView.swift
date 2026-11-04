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
            // The scroll view's frame starts below the toolbar, and hover measures from there; its
            // visible rect also reaches up under the toolbar by the top inset.
            ConversationListGeometry(top: geometry.contentOffset.y + geometry.contentInsets.top,
                                     topInset: geometry.contentInsets.top,
                                     contentHeight: geometry.contentSize.height,
                                     containerHeight: geometry.containerSize.height)
        } action: { _, new in
            model.geometryChanged(new)
        }
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
        switch item {
        case .section(let title, let count, let accent):
            ListRowSectionHeader(title: title, count: count, accent: accent)
        case .shelf(let bundles):
            ConversationListShelfRow(state: model.rowState(for: item), bundles: bundles, own: model.store.ownAddresses) {
                model.enterBundle($0)
            }
        case .conversation(let conversation):
            ConversationListMailRow(model: model, conversation: conversation, state: model.rowState(for: item),
                                    emphasized: emphasized) { click(item.id) }
                .equatable()
        case .space(let space):
            ConversationListChatRow(model: model, space: space, state: model.rowState(for: item),
                                    emphasized: emphasized) { click(item.id) }
                .equatable()
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

/// The Smart Inbox shelf, lifting its tiles while the pointer is over it.
struct ConversationListShelfRow: View {
    let state: ListRowState
    let bundles: [ConversationListBundle]
    let own: Set<String>
    let onOpen: (MailCategory) -> Void

    var body: some View {
        ListRowShelf(bundles: bundles, own: own, hovered: state.hovered, onOpen: onOpen)
    }
}

/// A conversation's row, drawing the content its state holds: a snippet, picture or hover
/// redraws this row alone. Equal while its state and conversation are; the click is the same
/// line's either way.
struct ConversationListMailRow: View, Equatable {
    let model: ConversationListModel
    let conversation: Conversation
    let state: ListRowState
    let emphasized: Bool
    let onClick: () -> Void

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.state === rhs.state && lhs.emphasized == rhs.emphasized && lhs.conversation == rhs.conversation
    }

    var body: some View {
        if let content = state.content {
            let id = conversation.id
            ListRowConversation(content: content, hovered: state.hovered, onAction: { model.perform($0, id: id) },
                                avatars: model.store.avatars)
                .modifier(ListRowChrome(highlighted: state.highlighted, emphasized: emphasized, hovered: state.hovered))
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
    let onClick: () -> Void

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.state === rhs.state && lhs.emphasized == rhs.emphasized && lhs.space == rhs.space
    }

    var body: some View {
        if let content = state.content {
            let key = space.key
            ListRowConversation(content: content, hovered: state.hovered, avatars: model.store.avatars)
                .modifier(ListRowChrome(highlighted: state.highlighted, emphasized: emphasized, hovered: state.hovered))
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
