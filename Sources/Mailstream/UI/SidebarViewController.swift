import AppKit

/// Stable per-account color, used for sidebar dots, list markers and reader chips.
@MainActor
enum AccountStyle {
    private static let palette = ["#007AFF", "#FF9500", "#AF52DE", "#34C759", "#FF2D55", "#30B0C7"]

    static func hex(for account: String, in store: MailStore) -> String {
        let index = store.accounts.firstIndex { $0.name == account } ?? 0
        return palette[index % palette.count]
    }

    static func color(for account: String, in store: MailStore) -> NSColor {
        NSColor(hex: hex(for: account, in: store))
    }
}

/// Spark-style sidebar: unified mailboxes first (Inbox expands into each account's inbox), then
/// every account's full folder tree, collapsed by default.
@MainActor
final class SidebarViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate {
    final class Node {
        enum Kind {
            case unified(MailboxRole)
            case accountInbox(Mailbox)
            case section(String)
            case account(Account)
            case mailbox(Mailbox)
            case folders(account: String, title: String)
        }

        var kind: Kind
        var children: [Node]

        init(_ kind: Kind, children: [Node] = []) {
            self.kind = kind
            self.children = children
        }

        var key: String {
            switch kind {
            case .unified(let role): "unified:\(role)"
            case .accountInbox(let mailbox), .mailbox(let mailbox): "mailbox:\(mailbox.account)\u{1}\(mailbox.id)"
            case .section(let title): "section:\(title)"
            case .account(let account): "account:\(account.name)"
            case .folders(let account, _): "folders:\(account)"
            }
        }

        var selection: Selection? {
            switch kind {
            case .unified(let role): .unified(role)
            case .accountInbox(let mailbox), .mailbox(let mailbox): .mailbox(mailbox.key)
            default: nil
            }
        }
    }

    private static let unifiedRoles: [MailboxRole] = [.inbox, .starred, .drafts, .sent, .archive, .junk, .trash]

    private let store: MailStore
    private let outline = NSOutlineView()
    private var roots: [Node] = []
    private var updatingSelection = false
    private var expanded: Set<String> = ["unified:inbox", "section:Accounts"]

    init(store: MailStore) {
        self.store = store
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func loadView() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("main"))
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .sourceList
        outline.rowSizeStyle = .default
        outline.floatsGroupRows = false
        outline.indentationPerLevel = 12
        outline.dataSource = self
        outline.delegate = self
        outline.allowsEmptySelection = false

        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = true
        view = scroll

        NotificationCenter.default.addObserver(self, selector: #selector(reload), name: .storeAccountsChanged, object: store)
        reload()
    }

    /// Same tree as before (only counts or names moved): refresh the nodes and the visible cells in
    /// place. A different tree reloads and restores expansion.
    @objc private func reload() {
        let nodes = buildNodes()
        if Self.shape(nodes) == Self.shape(roots) {
            Self.adopt(nodes, into: roots)
            for row in 0..<outline.numberOfRows {
                guard let node = outline.item(atRow: row) as? Node,
                      let cell = outline.view(atColumn: 0, row: row, makeIfNecessary: false) else { continue }
                configure(cell, for: node)
            }
            return
        }
        roots = nodes
        updatingSelection = true
        outline.reloadData()
        expandRemembered(roots)
        selectCurrent()
        updatingSelection = false
    }

    private func buildNodes() -> [Node] {
        var nodes: [Node] = []
        for role in Self.unifiedRoles {
            let boxes = store.unifiedMailboxes(role)
            guard !boxes.isEmpty else { continue }
            let children = role == .inbox && boxes.count > 1 ? boxes.map { Node(.accountInbox($0)) } : []
            nodes.append(Node(.unified(role), children: children))
        }
        var accounts: [Node] = []
        for account in store.accounts {
            let all = store.mailboxes[account.name] ?? []
            var children = all.filter { $0.role != .other }.map { Node(.mailbox($0)) }
            let others = all.filter { $0.role == .other }
            if !others.isEmpty {
                let title = account.kind == .gmail ? "Labels" : "Folders"
                children.append(Node(.folders(account: account.name, title: title), children: others.map { Node(.mailbox($0)) }))
            }
            accounts.append(Node(.account(account), children: children))
        }
        if !accounts.isEmpty { nodes.append(Node(.section("Accounts"), children: accounts)) }
        return nodes
    }

    private static func shape(_ nodes: [Node]) -> [String] {
        nodes.flatMap { [$0.key] + shape($0.children) + ["/"] }
    }

    /// Copies fresh values into the existing nodes, which the outline view holds by identity.
    private static func adopt(_ fresh: [Node], into current: [Node]) {
        for (new, old) in zip(fresh, current) {
            old.kind = new.kind
            adopt(new.children, into: old.children)
        }
    }

    private func expandRemembered(_ nodes: [Node]) {
        for node in nodes where expanded.contains(node.key) {
            outline.expandItem(node)
            expandRemembered(node.children)
        }
    }

    private func selectCurrent() {
        for row in 0..<outline.numberOfRows {
            if let node = outline.item(atRow: row) as? Node, node.selection == store.selection {
                outline.selectRowIndexes([row], byExtendingSelection: false)
                return
            }
        }
    }

    // MARK: Data source

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        (item as? Node)?.children.count ?? roots.count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        (item as? Node)?.children[index] ?? roots[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        !((item as? Node)?.children.isEmpty ?? true)
    }

    func outlineView(_ outlineView: NSOutlineView, isGroupItem item: Any) -> Bool {
        if case .section = (item as? Node)?.kind { return true }
        return false
    }

    func outlineView(_ outlineView: NSOutlineView, shouldSelectItem item: Any) -> Bool {
        (item as? Node)?.selection != nil
    }

    func outlineViewItemDidExpand(_ notification: Notification) {
        if let node = notification.userInfo?["NSObject"] as? Node { expanded.insert(node.key) }
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        if let node = notification.userInfo?["NSObject"] as? Node { expanded.remove(node.key) }
    }

    // MARK: Delegate

    func outlineView(_ outlineView: NSOutlineView, heightOfRowByItem item: Any) -> CGFloat {
        if case .section = (item as? Node)?.kind { return 30 }
        return 28
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let node = item as? Node else { return nil }
        let cell: NSView
        if case .section = node.kind {
            cell = outlineView.makeView(withIdentifier: SidebarHeaderCell.identifier, owner: nil) as? SidebarHeaderCell
                ?? SidebarHeaderCell()
        } else {
            cell = outlineView.makeView(withIdentifier: SidebarCell.identifier, owner: nil) as? SidebarCell ?? SidebarCell()
        }
        configure(cell, for: node)
        return cell
    }

    private func configure(_ view: NSView, for node: Node) {
        if case .section(let title) = node.kind {
            (view as? SidebarHeaderCell)?.textField?.stringValue = title
            return
        }
        guard let cell = view as? SidebarCell else { return }
        cell.toolTip = nil
        switch node.kind {
        case .section:
            break
        case .unified(let role):
            let title = role == .inbox ? "Inbox" : role.title
            cell.configure(title: title, leading: .symbol(role.symbol), badge: badge(.unified(role), role: role))
        case .accountInbox(let mailbox):
            cell.configure(title: mailbox.account,
                           leading: .dot(AccountStyle.color(for: mailbox.account, in: store)),
                           badge: mailbox.unread ?? 0, secondary: true)
            cell.toolTip = store.account(named: mailbox.account)?.email
        case .account(let account):
            cell.configure(title: account.name,
                           leading: .avatar(Address(name: account.name, email: account.email ?? account.name),
                                            AccountStyle.color(for: account.name, in: store)),
                           badge: 0)
            cell.toolTip = account.email
        case .mailbox(let mailbox):
            cell.configure(title: mailbox.displayName, leading: .symbol(mailbox.role.symbol),
                           badge: badge(.mailbox(mailbox.key), role: mailbox.role))
        case .folders(_, let title):
            cell.configure(title: title, leading: .symbol("tag"), badge: 0)
        }
    }

    private func badge(_ selection: Selection, role: MailboxRole) -> Int {
        guard [.inbox, .other, .important, .starred, .drafts].contains(role) else { return 0 }
        return store.badgeCount(selection)
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !updatingSelection, let node = outline.item(atRow: outline.selectedRow) as? Node,
              let selection = node.selection else { return }
        store.select(selection)
    }
}

final class SidebarHeaderCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("SidebarHeader")

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        let label = NSTextField(labelWithString: "")
        label.font = .systemFont(ofSize: 11, weight: .semibold)
        label.textColor = .tertiaryLabelColor
        label.lineBreakMode = .byTruncatingTail
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        textField = label
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -4),
            label.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }
}

final class SidebarCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("SidebarCell")

    enum Leading {
        case symbol(String)
        case dot(NSColor)
        case avatar(Address, NSColor)
    }

    private let icon = NSImageView()
    private let marker = SidebarMarker()
    private let label = NSTextField(labelWithString: "")
    private let badge = NSTextField(labelWithString: "")
    private var secondary = false

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        icon.symbolConfiguration = .init(pointSize: 13, weight: .regular)
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        badge.font = .monospacedDigitSystemFont(ofSize: 11, weight: .medium)
        badge.alignment = .right
        badge.setContentCompressionResistancePriority(.required, for: .horizontal)
        for view in [icon, marker, label, badge] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        imageView = icon
        textField = label
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 20),
            marker.centerXAnchor.constraint(equalTo: icon.centerXAnchor),
            marker.centerYAnchor.constraint(equalTo: centerYAnchor),
            marker.widthAnchor.constraint(equalToConstant: 18),
            marker.heightAnchor.constraint(equalToConstant: 18),
            label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 7),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            badge.leadingAnchor.constraint(greaterThanOrEqualTo: label.trailingAnchor, constant: 6),
            badge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            badge.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(title: String, leading: Leading, badge count: Int, secondary: Bool = false) {
        label.stringValue = title
        self.secondary = secondary
        switch leading {
        case .symbol(let symbol):
            icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
            icon.isHidden = false
            marker.isHidden = true
        case .dot(let color):
            icon.image = nil
            marker.style = .dot(color)
            marker.isHidden = false
        case .avatar(let address, let color):
            icon.image = nil
            marker.style = .avatar(address, color)
            marker.isHidden = false
        }
        badge.stringValue = count > 0 ? count.formatted() : ""
        badge.isHidden = count == 0
        applyColors()
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { applyColors() }
    }

    private func applyColors() {
        let emphasized = backgroundStyle == .emphasized
        icon.contentTintColor = emphasized ? .alternateSelectedControlTextColor : .secondaryLabelColor
        label.font = .systemFont(ofSize: secondary ? 12 : 13, weight: .regular)
        label.textColor = emphasized ? .alternateSelectedControlTextColor : (secondary ? .secondaryLabelColor : .labelColor)
        badge.textColor = emphasized ? .alternateSelectedControlTextColor : .tertiaryLabelColor
    }
}

/// Account marker in the sidebar: a short colored bar (matching the list's account tick), or a lettered avatar.
final class SidebarMarker: NSView {
    enum Style {
        case dot(NSColor)
        case avatar(Address, NSColor)
    }

    var style: Style = .dot(.systemBlue) { didSet { needsDisplay = true } }

    override func draw(_ dirtyRect: NSRect) {
        switch style {
        case .dot(let color):
            color.setFill()
            NSBezierPath(roundedRect: NSRect(x: bounds.midX - 1.25, y: bounds.midY - 6, width: 2.5, height: 12),
                         xRadius: 1.25, yRadius: 1.25).fill()
        case .avatar(let address, let color):
            let rect = bounds.insetBy(dx: 1, dy: 1)
            color.setFill()
            NSBezierPath(roundedRect: rect, xRadius: 5, yRadius: 5).fill()
            let text = NSAttributedString(string: String(Avatar.initials(for: address).prefix(1)), attributes: [
                .font: NSFont.systemFont(ofSize: 10, weight: .bold),
                .foregroundColor: NSColor.white,
            ])
            let size = text.size()
            text.draw(at: NSPoint(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2))
        }
    }
}
