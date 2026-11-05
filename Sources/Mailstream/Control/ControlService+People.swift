import CryptoKit
import Foundation

/// `contacts.search` and `avatars`: the people and pictures the native compose window and lists
/// use, for front ends that talk to the app over the socket.
extension ControlService {
    /// How long `contacts.search` reuses the people it loaded; typing asks once per keystroke.
    private static let contactsAge: Duration = .seconds(60)

    /// Where `avatars` writes the PNGs it hands out by path.
    private nonisolated static let avatarFolder = URL.cachesDirectory.appending(path: "Mailstream/avatars", directoryHint: .isDirectory)

    func searchContacts(_ args: Arguments) async -> JSON {
        let query = args.string("query") ?? ""
        guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
        let limit = min(max(args.int("limit") ?? 10, 1), 12)
        let completion = RecipientCompletion()
        completion.contacts = await contacts()
        return .array(completion.suggestions(for: query).prefix(limit).map { contact in
            let address = contact.address
            let name = address.displayName == address.email ? nil : address.displayName
            return ["name": name.json, "email": .string(address.email)]
        })
    }

    /// Everyone compose completes to, loaded once per `contactsAge` and shared by searches that
    /// arrive while it loads.
    private func contacts() async -> [Contact] {
        let now = ContinuousClock.now
        if let load = contactsLoad, now - load.started < Self.contactsAge { return await load.task.value }
        let store = store
        let task = Task { await store.contacts() }
        contactsLoad = (task, now)
        return await task.value
    }

    func avatars(_ args: Arguments) async throws -> JSON {
        let emails = Array(Set((args.list("emails") ?? []).map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
            .filter { !$0.isEmpty }))
        guard !emails.isEmpty else { throw ControlError.usage("Give at least one address.") }
        let side = (args.int("size") ?? 64) <= AvatarResolver.side ? AvatarResolver.side : 512
        let avatars = store.avatars
        var out: [String: JSON] = [:]
        await forEachConcurrently(emails) { (email: String) -> URL? in
            guard let png = await avatars.png(for: email, automated: false, side: side) else { return nil }
            return await Self.write(png, for: email, side: side)
        } consume: { email, result in
            let url: URL? = (try? result.get()) ?? nil
            out[email] = url.map { .string($0.path) } ?? .null
        }
        return .object(out)
    }

    /// `png` at a stable path per address and size; rewritten only when the picture changed.
    @concurrent private nonisolated static func write(_ png: Data, for email: String, side: Int) async -> URL? {
        let digest = SHA256.hash(data: Data(email.utf8)).prefix(16).map { String(format: "%02x", $0) }.joined()
        let url = avatarFolder.appending(path: "\(digest)-\(side).png")
        if (try? Data(contentsOf: url)) == png { return url }
        do {
            try FileManager.default.createDirectory(at: avatarFolder, withIntermediateDirectories: true)
            try png.write(to: url, options: .atomic)
            return url
        } catch {
            return nil
        }
    }
}
