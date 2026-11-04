import SwiftUI

/// A sender picture the page asked to enlarge; `anchor` is the avatar's box in the web view's
/// points, top-left origin (see `ReaderModel.presentAvatar`).
struct AvatarPreviewItem: Identifiable {
    let id = UUID()
    let address: Address
    let small: NSImage
    let automated: Bool
    let anchor: CGRect
    let avatars: AvatarStore
}

/// Click-to-enlarge for sender pictures (the port of `AvatarPreview`): the picture, the name and
/// the address. It opens at once on the small cached picture and cross-fades to the 512 px one
/// when it arrives.
struct AvatarPreviewView: View {
    private static let side: CGFloat = 220

    let item: AvatarPreviewItem
    @State private var large: NSImage?

    var body: some View {
        VStack(spacing: 4) {
            Image(nsImage: large ?? item.small)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: Self.side, height: Self.side)
                .clipShape(.rect(cornerRadius: Design.Radius.panel.rawValue, style: .continuous))
                .id(large == nil)
                .transition(.opacity)
                .padding(.bottom, 12 - 4)
            Text(item.address.displayName)
                .font(.design(.headline, weight: .semibold))
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: Self.side)
            // Chat apps and people Google withholds the address of are keyed by their user id.
            if item.address.email.contains("@") {
                Text(item.address.email)
                    .font(.design(.footnote))
                    .foregroundStyle(Color.design(.muted))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .frame(maxWidth: Self.side)
            }
        }
        .multilineTextAlignment(.center)
        .padding(14)
        .task(id: item.id) {
            guard let image = await item.avatars.largeImage(for: item.address.email, automated: item.automated) else { return }
            withAnimation(.house(.brief)) { large = image }
        }
    }
}
