import SwiftUI

/// Hosts `SidebarView` in the main window's split view until the window itself is SwiftUI.
final class SidebarViewController: NSHostingController<SidebarView> {
    init(store: MailStore) {
        super.init(rootView: SidebarView(store: store))
        // The split view item sets the sidebar's width (190–320, autosaved); the list must not size it.
        sizingOptions = []
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }
}
