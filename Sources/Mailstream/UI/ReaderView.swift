import SwiftUI
import WebKit

/// The reader: the shell's long-lived web view, the "No Conversation Selected" placeholder while
/// nothing is shown, and the sender picture preview as a popover over the web view.
struct ReaderView: View {
    @Bindable var model: ReaderModel

    var body: some View {
        ZStack {
            ShellWebView(shell: model.shell)
                .popover(item: $model.avatarPreview,
                         attachmentAnchor: .rect(.rect(model.avatarPreview?.anchor ?? .zero)),
                         arrowEdge: .trailing) { item in
                    AvatarPreviewView(item: item)
                }
            if model.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "envelope.open")
                        .font(.system(size: 44, weight: .thin))
                    Text("No Conversation Selected")
                        .font(.system(size: 15, weight: .medium))
                }
                .foregroundStyle(Color(nsColor: .tertiaryLabelColor))
            }
        }
    }
}

/// Hosts `WebShell.webView`. The shell owns it for the app's lifetime, so SwiftUI only places it:
/// updates never touch it, and `ReaderModel.show` alone toggles `isHidden`.
private struct ShellWebView: NSViewRepresentable {
    let shell: WebShell

    func makeNSView(context: Context) -> WKWebView { shell.webView }

    func updateNSView(_ webView: WKWebView, context: Context) {}
}
