import Foundation

/// `contacts.search`: the people the native compose window completes to, for other front ends.
extension ControlService {
    /// How long `contacts.search` reuses the people it loaded; typing asks once per keystroke.
    private static let contactsAge: Duration = .seconds(60)

    func searchContacts(_ args: Arguments) async -> JSON {
        let query = args.string("query") ?? ""
        guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
        let limit = min(max(args.int("limit") ?? 10, 1), 12)
        let contacts = await contacts()
        return .array(Contact.suggestions(for: query, in: contacts).prefix(limit).map { contact in
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
}
