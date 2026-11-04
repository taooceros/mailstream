// AppKit: `AccountStyle` hands `NSColor`s to the AppKit list and reader, which still use them.
import AppKit
import SwiftUI

/// Stable per-account color, used for sidebar dots, list markers and reader chips.
@MainActor
enum AccountStyle {
    private static let palette = ["#007AFF", "#FF9500", "#AF52DE", "#34C759", "#FF2D55", "#30B0C7"]

    static func hex(for account: String, in store: MailStore) -> String {
        hex(at: store.accounts.firstIndex { $0.name == account } ?? 0)
    }

    /// The color of the account at `index` in sidebar order.
    static func hex(at index: Int) -> String {
        palette[index % palette.count]
    }

    static func color(for account: String, in store: MailStore) -> NSColor {
        NSColor(hex: hex(for: account, in: store))
    }

    /// A chat account takes the color of its mail account, else the next color after the mail
    /// accounts'.
    static func chatHex(for chatAccount: String, in store: MailStore) -> String {
        if store.account(named: chatAccount) != nil { return hex(for: chatAccount, in: store) }
        let index = store.chat.accounts.firstIndex { $0.name == chatAccount } ?? 0
        return palette[(store.accounts.count + index) % palette.count]
    }

    static func chatColor(for chatAccount: String, in store: MailStore) -> NSColor {
        NSColor(hex: chatHex(for: chatAccount, in: store))
    }
}

/// A row the user can select. An account's inbox shows both under Inbox and under the account, so
/// the row, not its `Selection`, is the list's selection value.
enum SidebarItem: Hashable {
    case unified(MailboxRole)
    /// An account's inbox under the unified Inbox.
    case accountInbox(MailboxKey)
    /// All chat accounts' spaces.
    case chat
    case chatAccount(String)
    case mailbox(MailboxKey)

    var selection: Selection {
        switch self {
        case .unified(let role): .unified(role)
        case .accountInbox(let key), .mailbox(let key): .mailbox(key)
        case .chat: .allChats
        case .chatAccount(let name): .chat(name)
        }
    }
}

/// A row with a disclosure triangle.
enum SidebarDisclosure: Hashable {
    case unifiedInbox
    case chat
    /// The Accounts section.
    case accounts
    case account(String)
    /// An account's Labels or Folders group.
    case folders(String)
}

/// Spark-style sidebar: unified mailboxes first (Inbox expands into each account's inbox), then
/// Google Chat (expanding into each chat account when there are several), then every account's
/// full folder tree, collapsed by default.
struct SidebarView: View {
    let store: MailStore

    /// Open disclosures, kept for the window's life (not across launches). A row that leaves the
    /// tree and comes back reopens as it was.
    @State private var expanded: Set<SidebarDisclosure> = [.unifiedInbox, .chat, .accounts]
    /// The row last clicked, which tells apart two rows showing the same mailbox.
    @State private var picked: SidebarItem?

    private static let unifiedRoles: [MailboxRole] = [.inbox, .starred, .drafts, .sent, .archive, .junk, .trash]

    var body: some View {
        List(selection: selection) {
            ForEach(Self.unifiedRoles, id: \.self) { role in
                unified(role)
            }
            chat
            if !store.accounts.isEmpty {
                Section(isExpanded: isExpanded(.accounts)) {
                    ForEach(store.accounts) { account in
                        accountTree(account)
                    }
                } header: {
                    Text("Accounts")
                        .font(.design(.caption, weight: .semibold))
                        .foregroundStyle(Color.design(.muted))
                        // The source list's 30 pt group row, its label 4 pt above the bottom.
                        .padding(.bottom, 4)
                        .frame(height: 30, alignment: .bottomLeading)
                }
            }
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        // Small sidebar rows pad their content by 8 pt; with the 20 pt row content below they make the
        // source list's 28 pt rows (medium ones come out 32 pt).
        .environment(\.sidebarRowSize, .small)
    }

    // MARK: Selection

    /// The store's selection; the sidebar always has one, so clearing it is ignored.
    private var selection: Binding<SidebarItem?> {
        Binding {
            if let picked, picked.selection == store.selection { return picked }
            return item(showing: store.selection)
        } set: { item in
            guard let item else { return }
            picked = item
            store.select(item.selection)
        }
    }

    /// The first row showing `selection`, top to bottom: an account's inbox under an open Inbox
    /// before the same mailbox under its account.
    private func item(showing selection: Selection) -> SidebarItem {
        switch selection {
        case .unified(let role): return .unified(role)
        case .chat(nil): return .chat
        case .chat(let name?): return .chatAccount(name)
        case .mailbox(let key):
            let inboxes = store.unifiedMailboxes(.inbox)
            let underInbox = expanded.contains(.unifiedInbox) && inboxes.count > 1 && inboxes.contains { $0.key == key }
            return underInbox ? .accountInbox(key) : .mailbox(key)
        }
    }

    private func isExpanded(_ disclosure: SidebarDisclosure) -> Binding<Bool> {
        Binding {
            expanded.contains(disclosure)
        } set: { open in
            if open { expanded.insert(disclosure) } else { expanded.remove(disclosure) }
        }
    }

    // MARK: Rows

    @ViewBuilder
    private func unified(_ role: MailboxRole) -> some View {
        let boxes = store.unifiedMailboxes(role)
        if role == .inbox && boxes.count > 1 {
            DisclosureGroup(isExpanded: isExpanded(.unifiedInbox)) {
                ForEach(boxes, id: \.key) { box in
                    SidebarRow(title: store.label(account: box.account),
                               leading: .dot(Color(nsColor: AccountStyle.color(for: box.account, in: store))),
                               badge: box.unread ?? 0, secondary: true,
                               selected: isSelected(.accountInbox(box.key)))
                        .help(store.account(named: box.account)?.email ?? "")
                        .tag(SidebarItem.accountInbox(box.key))
                }
            } label: {
                unifiedRow(role)
            }
        } else if !boxes.isEmpty {
            unifiedRow(role)
        }
    }

    private func unifiedRow(_ role: MailboxRole) -> some View {
        SidebarRow(title: role == .inbox ? "Inbox" : role.title, leading: .symbol(role.symbol),
                   badge: badge(.unified(role), role: role), selected: isSelected(.unified(role)))
            .tag(SidebarItem.unified(role))
    }

    @ViewBuilder
    private var chat: some View {
        let accounts = store.chat.accounts
        if accounts.count > 1 {
            DisclosureGroup(isExpanded: isExpanded(.chat)) {
                ForEach(accounts, id: \.name) { account in
                    SidebarRow(title: store.label(account: account.name),
                               leading: .dot(Color(nsColor: AccountStyle.chatColor(for: account.name, in: store))),
                               badge: store.badgeCount(.chat(account.name)), secondary: true,
                               selected: isSelected(.chatAccount(account.name)))
                        .help(account.name)
                        .tag(SidebarItem.chatAccount(account.name))
                }
            } label: {
                chatRow
            }
        } else if !accounts.isEmpty {
            chatRow
        }
    }

    private var chatRow: some View {
        SidebarRow(title: "Chat", leading: .symbol("bubble.left.and.bubble.right"), badge: store.badgeCount(.allChats),
                   selected: isSelected(.chat))
            .tag(SidebarItem.chat)
    }

    /// The account (not selectable), its role mailboxes, then its other mailboxes under Labels or Folders.
    @ViewBuilder
    private func accountTree(_ account: Account) -> some View {
        let all = store.mailboxes[account.name] ?? []
        let others = all.filter { $0.role == .other }
        if all.isEmpty {
            accountRow(account)
        } else {
            DisclosureGroup(isExpanded: isExpanded(.account(account.name))) {
                ForEach(all.filter { $0.role != .other }, id: \.key) { mailboxRow($0) }
                if !others.isEmpty {
                    DisclosureGroup(isExpanded: isExpanded(.folders(account.name))) {
                        ForEach(others, id: \.key) { mailboxRow($0) }
                    } label: {
                        SidebarRow(title: account.provider == .google ? "Labels" : "Folders", leading: .symbol("tag"), badge: 0)
                    }
                }
            } label: {
                accountRow(account)
            }
        }
    }

    private func accountRow(_ account: Account) -> some View {
        let address = Address(name: account.identity?.name ?? account.label, email: account.email)
        return SidebarRow(title: account.label,
                          leading: .avatar(String(Avatar.initials(for: address).prefix(1)),
                                           Color(nsColor: AccountStyle.color(for: account.name, in: store))),
                          badge: 0)
            .help(account.email)
    }

    private func mailboxRow(_ mailbox: Mailbox) -> some View {
        SidebarRow(title: mailbox.displayName, leading: .symbol(mailbox.role.symbol),
                   badge: badge(.mailbox(mailbox.key), role: mailbox.role), selected: isSelected(.mailbox(mailbox.key)))
            .tag(SidebarItem.mailbox(mailbox.key))
    }

    private func isSelected(_ item: SidebarItem) -> Bool { selection.wrappedValue == item }

    /// Counts show where unread mail matters: inboxes, folders, starred, and drafts (every draft).
    private func badge(_ selection: Selection, role: MailboxRole) -> Int {
        guard [.inbox, .other, .important, .starred, .drafts].contains(role) else { return 0 }
        return store.badgeCount(selection)
    }
}

/// One sidebar row: a symbol or account marker, the title, and a count when there is one.
struct SidebarRow: View {
    enum Leading {
        case symbol(String)
        /// A short bar in the account's color, matching the list's account tick.
        case dot(Color)
        /// The account's initial on its color.
        case avatar(String, Color)
    }

    let title: String
    let leading: Leading
    let badge: Int
    /// Rows naming an account under a unified node: smaller, quieter type.
    var secondary = false
    /// The list's selected row.
    var selected = false

    @Environment(\.appearsActive) private var appearsActive

    /// The selected row of the key window, on the accent fill (the source list's emphasized style).
    private var emphasized: Bool { selected && appearsActive }

    // Text and symbols use the hierarchical styles that `Design`'s text, muted and faint are
    // (label, secondary and tertiary label colors). The list renders them on the accent fill as
    // white, at their level's opacity; emphasized rows use the primary level, `onAccent` white.
    var body: some View {
        HStack(spacing: 7) {
            marker
                .frame(width: 20)
            Text(title)
                .font(.design(secondary ? .footnote : .body))
                .foregroundStyle(secondary && !emphasized ? HierarchicalShapeStyle.secondary : .primary)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 6)
            if badge > 0 {
                Text(badge.formatted())
                    .font(.designDigits(.caption, weight: .medium))
                    .foregroundStyle(emphasized ? HierarchicalShapeStyle.primary : .tertiary)
                    .fixedSize()
            }
        }
        .padding(.leading, 2)
        .padding(.trailing, 6)
        .frame(height: 20)
    }

    @ViewBuilder
    private var marker: some View {
        switch leading {
        case .symbol(let symbol):
            // The source list drew its symbols a scale up from the label, shrunk to fit its 20 pt
            // column (the wide chat bubbles).
            ViewThatFits(in: .horizontal) {
                Image(systemName: symbol).imageScale(.large)
                Image(systemName: symbol).imageScale(.medium)
            }
            .font(.design(.body))
            .foregroundStyle(emphasized ? HierarchicalShapeStyle.primary : .secondary)
        case .dot(let color):
            Capsule()
                .fill(color)
                .frame(width: 2.5, height: 12)
                .frame(width: 18, height: 18)
        case .avatar(let initial, let color):
            RoundedRectangle(cornerRadius: Design.Radius.small.rawValue)
                .fill(color)
                .overlay {
                    Text(initial)
                        .font(.design(.micro, weight: .bold))
                        .foregroundStyle(.white)
                }
                .padding(1)
                .frame(width: 18, height: 18)
        }
    }
}
