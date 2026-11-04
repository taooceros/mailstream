import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The compose window's content: the header fields, the body, the attachments and the toolbar.
struct ComposeView: View {
    @Bindable var model: ComposeModel

    /// Leading inset of the header's labels and the body's text.
    static let margin: CGFloat = 20

    var body: some View {
        VStack(spacing: 0) {
            header
                .padding(.top, 4)
            ComposeBody(model: model)
                .disabled(model.sending)
                .overlay(alignment: .top) {
                    if model.sending {
                        ProgressView().controlSize(.small).padding(.top, 40)
                    }
                }
            if !model.attachments.isEmpty {
                attachmentChips
            }
        }
        .frame(minWidth: 480, maxWidth: .infinity, minHeight: 360, maxHeight: .infinity)
        .background { sendShortcuts }
        .navigationTitle(model.title)
        .navigationSubtitle(model.subtitle)
        .toolbar { toolbar }
        .fileImporter(isPresented: $model.choosingFiles, allowedContentTypes: [.data, .package],
                      allowsMultipleSelection: true) { result in
            if case .success(let urls) = result { model.add(urls) }
        }
        .alert(model.alert?.title ?? "", isPresented: alertShown, presenting: model.alert) { _ in
            Button("OK") {}
        } message: { alert in
            Text(alert.detail)
        }
        .confirmationDialog(model.discardTitle, isPresented: $model.confirmingDiscard, titleVisibility: .visible) {
            // Discard is the default button, so Return takes it: closing already asked for it, and a
            // button marked destructive never answers Return. The red icon carries the warning.
            Button("Discard") { model.discard() }
                .keyboardShortcut(.defaultAction)
            Button("Keep Editing", role: .cancel) {}
        } message: {
            Text(model.discardDetail)
        }
        .dialogIcon(model.discardIcon.map { Image(nsImage: $0).renderingMode(.original) })
    }

    private var alertShown: Binding<Bool> {
        Binding { model.alert != nil } set: { if !$0 { model.alert = nil } }
    }

    // MARK: - Header

    private var header: some View {
        VStack(spacing: 0) {
            row("From") {
                fromPicker
                Spacer(minLength: 0)
            }
            row("To") {
                RecipientField(model: model, field: .to).disabled(model.sending)
            }
            row("Cc") {
                RecipientField(model: model, field: .cc).disabled(model.sending)
                Button("Bcc", action: model.toggleBcc)
                    .buttonStyle(InlineButtonStyle())
                    .help("Show Bcc")
            }
            if model.showsBcc {
                row("Bcc") {
                    RecipientField(model: model, field: .bcc).disabled(model.sending)
                }
            }
            TextField("Subject", text: subject)
                .textFieldStyle(.plain)
                .font(.design(.headline, weight: .semibold))
                .disabled(model.sending)
                // Same start as the body text (the field pads 2 pt).
                .padding(.leading, Self.margin - 2)
                .padding(.trailing, Self.margin)
                .frame(height: 44)
                .overlay(alignment: .bottom) { separator }
        }
    }

    /// Typing in the subject counts as an edit; the prefilled subject does not.
    private var subject: Binding<String> {
        Binding { model.subject } set: {
            model.subject = $0
            model.edited = true
        }
    }

    /// One header line: a label and the field over a hairline.
    private func row(_ title: String, @ViewBuilder field: () -> some View) -> some View {
        HStack(spacing: 8) {
            Text(title + ":")
                .font(.design(.body))
                .foregroundStyle(Color.design(.muted))
                .frame(width: 52, alignment: .trailing)
            field()
        }
        .padding(.horizontal, Self.margin)
        .frame(height: 34)
        .overlay(alignment: .bottom) { separator }
    }

    private var separator: some View {
        Divider().padding(.leading, Self.margin)
    }

    private var fromPicker: some View {
        Picker("From", selection: $model.account) {
            ForEach(model.store.accounts, id: \.name) { account in
                Text(account.identity.map(Contact.label) ?? account.name)
                    .help(account.name)
                    .tag(account.name)
            }
        }
        .pickerStyle(.menu)
        .buttonStyle(.borderless)
        .labelsHidden()
        .font(.design(.body))
        .fixedSize()
        .disabled(!model.choosesAccount || model.sending)
    }

    // MARK: - Attachments

    private var attachmentChips: some View {
        HStack(spacing: 6) {
            ForEach(model.attachments) { attachment in
                chip(attachment)
            }
        }
        .padding(EdgeInsets(top: 8, leading: Self.margin, bottom: 10, trailing: Self.margin))
        .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
        .clipped()
    }

    /// A rounded chip: the file's icon, name and size, and a button removing it.
    private func chip(_ attachment: ComposeAttachment) -> some View {
        let name = attachment.url.lastPathComponent
        return HStack(spacing: 6) {
            Image(nsImage: attachment.icon)
                .resizable()
                .frame(width: 16, height: 16)
            Text(name)
                .font(.design(.footnote))
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 180)
                .fixedSize(horizontal: false, vertical: true)
            if let size = attachment.size {
                Text(size)
                    .font(.design(.caption))
                    .foregroundStyle(Color.design(.muted))
            }
            Button {
                model.remove(attachment)
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .accessibilityLabel("Remove")
            }
            .buttonStyle(.plain)
            .foregroundStyle(Color.design(.faint))
            .help("Remove \(name)")
        }
        .padding(EdgeInsets(top: 5, leading: 8, bottom: 5, trailing: 6))
        .designCard(.control)
        .fixedSize()
    }

    // MARK: - Toolbar and keys

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            Button {
                model.choosingFiles = true
            } label: {
                Label("Attach", systemImage: "paperclip")
            }
            .help("Attach Files (or drop them on the message)")
        }
        ToolbarItem(placement: .primaryAction) {
            Button(action: model.send) {
                Label("Send", systemImage: "paperplane.fill")
            }
            .buttonStyle(.borderedProminent)
            .help("Send (⌘↩)")
            .disabled(model.sending)
        }
    }

    /// Sends on ⌘↩ (the keypad's Enter too), as the inline reply does, and on ⇧⌘D, as Mail does.
    /// Window shortcuts run before the main menu's, so ⇧⌘D sends here.
    private var sendShortcuts: some View {
        ZStack {
            Button("Send", action: model.send).keyboardShortcut(.return, modifiers: .command)
            Button("Send", action: model.send).keyboardShortcut(KeyEquivalent("\u{3}"), modifiers: .command)
            Button("Send", action: model.send).keyboardShortcut("d", modifiers: [.command, .shift])
        }
        .opacity(0)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// The small rounded "Bcc" button beside the Cc field.
private struct InlineButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.design(.caption, weight: .bold))
            .foregroundStyle(Color.design(.muted))
            .padding(.horizontal, 4)
            .padding(.vertical, 2)
            .background(Capsule().fill(Color.design(configuration.isPressed ? .line : .chip)))
    }
}
