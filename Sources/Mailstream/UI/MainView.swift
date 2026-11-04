import AppKit
import SwiftUI

/// Two panes in the Spark style: mailboxes | a full-width content area that shows the
/// conversation list (or the chat spaces), with the reader as a layer sliding over it.
struct MainView: View {
    @Bindable var model: MainModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let titles = model.titles()
        NavigationSplitView {
            SidebarView(store: model.store)
                .navigationSplitViewColumnWidth(min: 190, ideal: 230, max: 320)
        } detail: {
            content
                .frame(minWidth: 520)
                .toolbar { MainToolbar(model: model) }
        }
        .navigationTitle(titles.title)
        .navigationSubtitle(titles.subtitle)
        .searchable(text: $model.searchText, isPresented: $model.searchPresented, placement: .toolbar, prompt: "Search")
        .onChange(of: model.searchText) { _, query in model.search(query) }
        .onSubmit(of: .search) { model.search(model.searchText) }
        .alert("The action could not be completed", isPresented: alertShown, presenting: model.alert) { _ in
            Button("OK") {}
        } message: { problem in
            Text(problem.message)
        }
        .alert("Delete this conversation permanently?", isPresented: deleteShown) {
            Button("Delete", role: .destructive) { model.confirmPermanentDelete() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Messages already in the trash are removed for good.")
        }
        .frame(minWidth: 760, minHeight: 480)
        .background(MainWindowReader { model.attach($0) })
        .onAppear {
            model.openMainWindow = { openWindow(id: MailstreamApp.mainWindowID) }
            model.presentAlert()
        }
    }

    /// The list stays in place under the reader, which is always mounted (its web view stays warm)
    /// and only shown while a conversation or space is open.
    private var content: some View {
        ZStack {
            ConversationListView(model: model.list)
                .scaleEffect(model.listScale)
            ReaderView(model: model.reader)
                .background(Color(nsColor: .textBackgroundColor))
                .scaleEffect(model.readerScale)
                .opacity(model.readerShown ? model.readerOpacity : 0)
                .designShadow(model.readerLifted ? .page : Design.Shadow(opacity: 0, radius: 0))
                .offset(x: model.readerOffset)
                .allowsHitTesting(model.readerShown)
                .accessibilityHidden(!model.readerShown)
        }
        .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { model.pageFrame = $0 }
    }

    private var alertShown: Binding<Bool> {
        Binding { model.alert != nil } set: { shown in
            guard !shown else { return }
            model.alert = nil
            model.sheetEnded()
        }
    }

    private var deleteShown: Binding<Bool> {
        Binding { model.pendingDelete != nil } set: { shown in
            guard !shown else { return }
            model.pendingDelete = nil
            model.sheetEnded()
        }
    }
}

/// Hands MainView's `NSWindow` to the model: the frame autosave name, the event monitors' window
/// check, sheets and the undo manager need it.
private struct MainWindowReader: NSViewRepresentable {
    let onWindow: (NSWindow) -> Void

    func makeNSView(context: Context) -> WindowProbe {
        let view = WindowProbe()
        view.onWindow = onWindow
        return view
    }

    func updateNSView(_ view: WindowProbe, context: Context) {}

    final class WindowProbe: NSView {
        var onWindow: (NSWindow) -> Void = { _ in }
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let window { onWindow(window) }
        }
    }
}
