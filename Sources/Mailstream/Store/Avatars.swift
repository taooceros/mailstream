import AppKit
import CryptoKit
import dnssd
import Observation
import Synchronization

/// Sender pictures for the list and the reader, looked up lazily for what is on screen.
///
/// Sources, best first: the photo of a Google or Outlook contact (accounts granted contacts
/// access), the picture a person publishes themselves (Google Chat's `avatarUrl`, handed in through
/// `publish`), the sender domain's BIMI logo, Gravatar, and for automated mail the domain's site
/// icon. Anything without a picture keeps its initials.
///
/// Observable through `revision`: views showing initials look again when it changes.
@Observable
final class AvatarStore {
    /// Bumped whenever pictures arrive; arrivals within one main-actor turn are one change.
    private(set) var revision = 0

    @ObservationIgnored private let resolver: AvatarResolver
    @ObservationIgnored private let images = NSCache<NSString, NSImage>()
    @ObservationIgnored private let pngs = NSCache<NSString, NSData>()
    @ObservationIgnored private let large = NSCache<NSString, NSImage>()
    @ObservationIgnored private var missing = Set<String>()
    @ObservationIgnored private var pending = Set<String>()
    /// Address or Chat user id (`users/…`) → the picture that person publishes.
    @ObservationIgnored private var published: [String: URL] = [:]
    /// Follows the photo accounts as accounts are added, removed or regranted.
    @ObservationIgnored private var accountWatch: Task<Void, Never>?

    init(accounts: AccountStore, root: URL) {
        let resolver = AvatarResolver(accounts: Self.photoAccounts(accounts),
                                      root: root.appendingPathComponent("avatars", isDirectory: true))
        self.resolver = resolver
        images.countLimit = 400
        pngs.countLimit = 400
        large.countLimit = 12
        accountWatch = Task.immediate(name: "Avatar accounts") {
            for await _ in Observations({ accounts.records }).dropFirst() {
                await resolver.use(Self.photoAccounts(accounts))
            }
        }
    }

    isolated deinit { accountWatch?.cancel() }

    /// Google accounts with `contacts.readonly` (and the domain directory with `directory.readonly`)
    /// and Microsoft accounts with `Contacts.Read`.
    private static func photoAccounts(_ accounts: AccountStore) -> [PhotoAccount] {
        accounts.records.compactMap { record -> PhotoAccount? in
            let scopes = Set(record.scopes)
            switch record.provider {
            case .google where scopes.contains("https://www.googleapis.com/auth/contacts.readonly"):
                return accounts.api(for: record.id).map {
                    PhotoAccount(api: $0, provider: .google,
                                 directory: scopes.contains("https://www.googleapis.com/auth/directory.readonly"))
                }
            case .microsoft where scopes.contains("https://graph.microsoft.com/Contacts.Read"):
                return accounts.api(for: record.id).map { PhotoAccount(api: $0, provider: .microsoft, directory: false) }
            default:
                return nil
            }
        }
    }

    /// Makes `url`, the picture a person publishes (Google Chat's `avatarUrl`), a source for `key`:
    /// their lowercased address, or their Chat user id when there is none. A new picture for a key
    /// that had none is looked for again, and one already shown, found without it (a domain's
    /// logo, say), is looked up again behind it.
    func publish(_ url: URL, for key: String) {
        let key = key.lowercased()
        guard !key.isEmpty, published[key] != url else { return }
        published[key] = url
        missing.remove(key)
        if images.object(forKey: key as NSString) != nil { request(key, automated: false) }
    }

    /// The picture for `key` (an address, or a Chat user id with a published picture) when it is
    /// already known; otherwise starts looking and returns nil, bumping `revision` if one turns up.
    /// `automated` allows the domain's site icon.
    func image(for key: String, automated: Bool) -> NSImage? {
        let key = key.lowercased()
        if let image = images.object(forKey: key as NSString) { return image }
        request(key, automated: automated)
        return nil
    }

    /// The picture for `key` if one is in memory, without looking for one.
    func cachedImage(for key: String) -> NSImage? {
        images.object(forKey: key.lowercased() as NSString)
    }

    /// The same picture as a `data:` URL, for the reader page.
    func dataURL(for key: String, automated: Bool) -> String? {
        let key = key.lowercased()
        guard let png = pngs.object(forKey: key as NSString) else {
            request(key, automated: automated)
            return nil
        }
        return "data:image/png;base64," + (png as Data).base64EncodedString()
    }

    /// The same picture at 512 px for an enlarged view, cached on disk like the row size.
    func largeImage(for key: String, automated: Bool) async -> NSImage? {
        let key = key.lowercased()
        if let image = large.object(forKey: key as NSString) { return image }
        guard let png = await resolver.avatar(for: key, published: published[key], automated: automated, side: 512),
              let image = NSImage(data: png) else { return nil }
        large.setObject(image, forKey: key as NSString)
        return image
    }

    private func request(_ key: String, automated: Bool) {
        let photo = published[key]
        guard key.contains("@") || photo != nil, !missing.contains(key), pending.insert(key).inserted else { return }
        let resolver = resolver
        Task {
            let png = await resolver.avatar(for: key, published: photo, automated: automated)
            pending.remove(key)
            // A picture published meanwhile outranks what this lookup could find: look again with it.
            let superseded = published[key] != photo
            if superseded { request(key, automated: automated) }
            guard let png, let image = NSImage(data: png) else {
                if !superseded { missing.insert(key) }
                return
            }
            images.setObject(image, forKey: key as NSString)
            pngs.setObject(png as NSData, forKey: key as NSString)
            revision += 1
        }
    }
}

/// Finds and caches one normalized PNG per sender and size: 96 px for rows and the reader, 512 px
/// for enlarged views. A cached picture answers before anything else is read or asked, so a
/// relaunch paints from disk at once; one older than `hit` still shows while it is looked up again
/// in the background. A miss (96 px only) holds until the contact directory changes.
actor AvatarResolver {
    /// The row size: rows and the reader draw at most 34 pt, so 96 px covers Retina.
    static let side = 96
    /// Age after which a cached picture is looked up again, behind the one shown.
    private static let hit: TimeInterval = 14 * 86_400
    private static let miss: TimeInterval = 3 * 86_400
    private static let directoryAge: TimeInterval = 86_400
    /// Wait after a directory refresh that could not ask every account before trying again.
    private static let directoryRetry: TimeInterval = 300

    private var accounts: [PhotoAccount]
    private let root: URL
    private let session: URLSession
    private let permits = AsyncSemaphore(6)
    /// The directory lookups use; an old one, or one built for other accounts, keeps answering
    /// while `refresh` replaces it.
    private var directory: Directory?
    /// Reading the saved directory, shared by the lookups that start before it is in.
    private var saved: Task<Directory?, Never>?
    private var refresh: Task<Directory, Never>?
    /// Bumped when the photo accounts change, so a refresh against the old ones is dropped.
    private var generation = 0
    /// When a refresh last failed to ask every account.
    private var lastIncomplete: Date?
    /// Pictures past their age being looked up again.
    private var revalidating = Set<String>()
    /// Domain-wide lookups in flight; finished ones live in the disk cache.
    private var domains: [String: Task<Data?, Never>] = [:]

    /// Where a contact's photo is.
    private nonisolated enum PhotoSource: Codable, Hashable {
        /// A Google photo, served at any size.
        case url(URL)
        /// An Outlook contact; whether it has a photo is only known when it is fetched.
        case graphContact(account: String, id: String)
    }

    /// Contact address → where its photo is.
    private nonisolated struct Directory: Codable {
        let built: Date
        /// The accounts it asked, whether they answered or refused; any difference from the current
        /// photo accounts (one added, removed, or unreachable last time) rebuilds it at once.
        let accounts: [String]
        let photos: [String: PhotoSource]
        /// A digest of `photos`: changes whenever any contact photo does, retiring cached misses.
        let stamp: String

        init(built: Date, accounts: [String], photos: [String: PhotoSource]) {
            self.built = built
            self.accounts = accounts
            self.photos = photos
            let encoder = JSONEncoder()
            encoder.outputFormatting = .sortedKeys
            stamp = AvatarResolver.hash((try? encoder.encode(photos)) ?? Data())
        }
    }

    init(accounts: [PhotoAccount], root: URL) {
        self.accounts = accounts
        self.root = root
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10
        configuration.httpAdditionalHeaders = ["User-Agent": "Mailstream"]
        session = URLSession(configuration: configuration)
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    /// `key` is an address, or a Chat user id when `published` is the only source. The cache entry
    /// is per published picture: one found without it may be a lower source, such as a domain logo.
    func avatar(for key: String, published: URL?, automated: Bool, side: Int = AvatarResolver.side) async -> Data? {
        let automated = automated || Self.looksAutomated(key)
        let cacheKey = (side == Self.side ? "p-" : "p\(side)-") + Self.hash(published.map { key + "|" + $0.absoluteString } ?? key)
        let missStamp: String?
        switch await cached(cacheKey) {
        case .hit(let png, let fresh):
            if !fresh { revalidate(key, published: published, automated: automated, side: side, cacheKey: cacheKey) }
            return png
        case .miss(let stamp): missStamp = stamp
        case .unknown: missStamp = nil
        }
        let directory = await contacts()
        if missStamp == Self.stamp(directory, published) { return nil }
        return await lookUp(key, published: published, automated: automated, side: side, cacheKey: cacheKey,
                            in: directory, replacing: false)
    }

    /// What a miss is made against: the directory, and the published picture if any, since a miss
    /// made without it, or with another one, does not stand for a lookup with it.
    private static func stamp(_ directory: Directory, _ published: URL?) -> String {
        directory.stamp + (published.map { "|" + hash($0.absoluteString) } ?? "")
    }

    /// Looks `key` up and caches what it finds at `cacheKey`. A miss is cached only at the row size
    /// and only when nothing was there: a picture being replaced stays rather than give way to a
    /// failed fetch.
    private func lookUp(_ key: String, published: URL?, automated: Bool, side: Int, cacheKey: String,
                        in directory: Directory, replacing: Bool) async -> Data? {
        let png: Data?
        if side == Self.side {
            do {
                png = try await permits.withPermit {
                    await find(key, published: published, automated: automated, side: side, in: directory)
                }
            } catch {
                // Cancelled while waiting for a permit: nothing was looked up, so no miss is cached.
                return nil
            }
        } else {
            // An enlarged view, opened by hand, does not queue behind the rows' lookups.
            png = await find(key, published: published, automated: automated, side: side, in: directory)
        }
        if png != nil || (side == Self.side && !replacing) {
            await store(png, at: cacheKey, stamp: Self.stamp(directory, published))
        }
        return png
    }

    /// Looks up again a picture past its age, which keeps showing meanwhile; a new one shows once
    /// it is next read from disk.
    private func revalidate(_ key: String, published: URL?, automated: Bool, side: Int, cacheKey: String) {
        guard revalidating.insert(cacheKey).inserted else { return }
        Task(name: "Avatar refresh", priority: .background) {
            let directory = await contacts()
            _ = await lookUp(key, published: published, automated: automated, side: side, cacheKey: cacheKey,
                             in: directory, replacing: true)
            revalidating.remove(cacheKey)
        }
    }

    private func find(_ key: String, published: URL?, automated: Bool, side: Int, in directory: Directory) async -> Data? {
        if let source = directory.photos[key], let png = await contactPhoto(source, side: side) { return png }
        if let published, let fetched = await fetch(Self.sized(published, side: side)),
           let png = await Self.normalize(fetched, style: .logo, side: side) {
            return png
        }
        guard key.contains("@") else { return nil }
        let domain = key.split(separator: "@").last.map(String.init) ?? ""
        let company = !domain.isEmpty && !Self.freemail.contains(domain)
        if automated, company, let png = await domainLogo(domain, side: side) { return png }
        if let png = await gravatar(key, side: side) { return png }
        if !automated, company, let png = await bimi(domain, side: side) { return png }
        return nil
    }

    // MARK: - Disk cache

    /// A picture, whether younger than `hit`; or a miss younger than `miss` with the stamp it was
    /// made against.
    private nonisolated enum Cached { case hit(Data, fresh: Bool), miss(stamp: String), unknown }

    @concurrent private nonisolated func cached(_ key: String) async -> Cached {
        let png = root.appendingPathComponent(key + ".png")
        if let data = try? Data(contentsOf: png) {
            let age = Self.modified(png).map { Date().timeIntervalSince($0) } ?? .infinity
            return .hit(data, fresh: age < Self.hit)
        }
        let none = root.appendingPathComponent(key + ".none")
        if let date = Self.modified(none), Date().timeIntervalSince(date) < Self.miss {
            return .miss(stamp: (try? String(contentsOf: none, encoding: .utf8)) ?? "")
        }
        return .unknown
    }

    /// `stamp` ties a miss to the directory it was made against; nil for domain-wide results.
    @concurrent private nonisolated func store(_ png: Data?, at key: String, stamp: String?) async {
        let fm = FileManager.default
        let pngURL = root.appendingPathComponent(key + ".png")
        let noneURL = root.appendingPathComponent(key + ".none")
        if let png {
            try? png.write(to: pngURL, options: .atomic)
            try? fm.removeItem(at: noneURL)
        } else {
            try? Data((stamp ?? "").utf8).write(to: noneURL, options: .atomic)
            try? fm.removeItem(at: pngURL)
        }
    }

    private static func modified(_ url: URL) -> Date? {
        (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
    }

    // MARK: - Contacts

    private var directoryFile: URL { root.appendingPathComponent("directory.json") }

    /// Takes the current photo accounts; the directory in hand keeps answering until one built for
    /// them replaces it.
    func use(_ fresh: [PhotoAccount]) {
        guard fresh.map(\.name) != accounts.map(\.name) || fresh.map(\.directory) != accounts.map(\.directory) else { return }
        accounts = fresh
        generation += 1
        refresh?.cancel()
        refresh = nil
        lastIncomplete = nil
    }

    /// The contact directory: the one in hand, else the one saved on disk, whatever its age. One
    /// that is a day old or was built for other accounts answers while a refresh replaces it; only
    /// a lookup with nothing saved at all waits for the providers.
    private func contacts() async -> Directory {
        if directory == nil {
            let file = directoryFile
            let task = saved ?? Task { await Self.saved(at: file) }
            saved = task
            let loaded = await task.value
            if directory == nil { directory = loaded }
        }
        if let directory {
            if refresh == nil, needsRefresh(directory) { startRefresh() }
            return directory
        }
        return await (refresh ?? startRefresh()).value
    }

    private func needsRefresh(_ directory: Directory) -> Bool {
        guard Set(directory.accounts) != Set(accounts.map(\.name))
                || Date().timeIntervalSince(directory.built) >= Self.directoryAge else { return false }
        // Not on every lookup while the providers cannot be reached.
        return lastIncomplete.map { Date().timeIntervalSince($0) >= Self.directoryRetry } ?? true
    }

    @discardableResult
    private func startRefresh() -> Task<Directory, Never> {
        let mine = generation
        let task = Task(name: "Contact photos", priority: .background) { await refreshDirectory(generation: mine) }
        refresh = task
        return task
    }

    /// Asks every photo account; a directory that reached them all replaces the one in hand and is
    /// saved, one that did not is used only when there is nothing else.
    private func refreshDirectory(generation mine: Int) async -> Directory {
        let fresh = await fetchDirectory()
        // The accounts changed meanwhile: `use` dropped this refresh.
        guard mine == generation else { return fresh }
        refresh = nil
        if Set(fresh.accounts) == Set(accounts.map(\.name)) {
            lastIncomplete = nil
            directory = fresh
            await Self.save(fresh, to: directoryFile)
        } else {
            lastIncomplete = Date()
            if directory == nil { directory = fresh }
        }
        return fresh
    }

    private func fetchDirectory() async -> Directory {
        let accounts = accounts
        var photos: [String: PhotoSource] = [:]
        var asked: [String] = []
        for account in accounts {
            let found: [String: PhotoSource]?
            do {
                found = switch account.provider {
                case .google: try await googlePhotos(api: account.api, directory: account.directory)
                case .microsoft: try await graphPhotos(api: account.api)
                }
            } catch {
                continue
            }
            asked.append(account.name)
            if let found { photos.merge(found) { current, _ in current } }
        }
        return Directory(built: Date(), accounts: asked, photos: photos)
    }

    @concurrent private nonisolated static func saved(at url: URL) async -> Directory? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(Directory.self, from: data)
    }

    @concurrent private nonisolated static func save(_ directory: Directory, to url: URL) async {
        if let data = try? JSONEncoder().encode(directory) { try? data.write(to: url, options: .atomic) }
    }

    /// `request`'s answer; nil when the provider refused it (no permission, no such list), thrown
    /// when it could not be asked (offline, no token yet, throttled, server error).
    private nonisolated static func unlessRefused<Page>(_ request: () async throws -> Page) async throws -> Page? {
        do {
            return try await request()
        } catch let error as APIError where (400..<500).contains(error.status) && error.status != 429 {
            return nil
        }
    }

    /// Saved contacts, "other contacts" (people the user has written with) and, on Workspace
    /// accounts, the organization's directory. Nil when every one of them is refused; thrown when
    /// the account could not be asked.
    private func googlePhotos(api: API, directory: Bool) async throws -> [String: PhotoSource]? {
        let readMask = URLQueryItem(name: "readMask", value: "emailAddresses,photos")
        var lists = [
            ("people/me/connections", [URLQueryItem(name: "personFields", value: "emailAddresses,photos")]),
            ("otherContacts", [readMask]),
        ]
        if directory {
            lists.append(("people:listDirectoryPeople",
                          [readMask, URLQueryItem(name: "sources", value: "DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE")]))
        }
        var photos: [String: PhotoSource] = [:]
        var answered = false
        for (path, query) in lists {
            let base = URL(string: "https://people.googleapis.com/v1/" + path)!
                .appending(queryItems: query + [URLQueryItem(name: "pageSize", value: "1000")])
            var pageToken: String?
            for _ in 0..<30 {
                let url = pageToken.map { base.appending(queryItems: [URLQueryItem(name: "pageToken", value: $0)]) } ?? base
                guard let page = try await Self.unlessRefused({ try await api.get(PeoplePage.self, url, priority: .background) })
                else { break }
                answered = true
                for person in page.people {
                    guard let link = person.photos?.first(where: { $0.default != true })?.url.flatMap(URL.init(string:))
                    else { continue }
                    for address in person.emailAddresses ?? [] {
                        if let email = address.value?.lowercased(), photos[email] == nil { photos[email] = .url(link) }
                    }
                }
                pageToken = page.nextPageToken
                if pageToken == nil { break }
            }
        }
        return answered ? photos : nil
    }

    /// One page of saved contacts, other contacts or directory people.
    private nonisolated struct PeoplePage: Decodable {
        struct Person: Decodable {
            struct Photo: Decodable {
                let url: String?
                let `default`: Bool?
            }
            struct Email: Decodable { let value: String? }
            let photos: [Photo]?
            let emailAddresses: [Email]?
        }

        let people: [Person]
        let nextPageToken: String?

        private enum CodingKeys: String, CodingKey { case connections, otherContacts, people, nextPageToken }

        /// Each list names its people differently.
        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            people = try [CodingKeys.connections, .otherContacts, .people].flatMap {
                try container.decodeIfPresent([Person].self, forKey: $0) ?? []
            }
            nextPageToken = try container.decodeIfPresent(String.self, forKey: .nextPageToken)
        }
    }

    /// Outlook contacts. Nil when the request is refused; thrown when the account could not be asked.
    private func graphPhotos(api: API) async throws -> [String: PhotoSource]? {
        var photos: [String: PhotoSource] = [:]
        var answered = false
        var next: URL? = URL(string: "https://graph.microsoft.com/v1.0/me/contacts")!
            .appending(queryItems: [URLQueryItem(name: "$select", value: "emailAddresses"), URLQueryItem(name: "$top", value: "999")])
        for _ in 0..<30 {
            guard let url = next,
                  let page = try await Self.unlessRefused({ try await api.get(GraphContacts.self, url, priority: .background) })
            else { break }
            answered = true
            for contact in page.value {
                for address in contact.emailAddresses ?? [] {
                    if let email = address.address?.lowercased(), photos[email] == nil {
                        photos[email] = .graphContact(account: api.account, id: contact.id)
                    }
                }
            }
            next = page.nextLink
        }
        return answered ? photos : nil
    }

    private nonisolated struct GraphContacts: Decodable {
        struct Contact: Decodable {
            struct Email: Decodable { let address: String? }
            let id: String
            let emailAddresses: [Email]?
        }

        let value: [Contact]
        let nextLink: URL?

        private enum CodingKeys: String, CodingKey {
            case value
            case nextLink = "@odata.nextLink"
        }
    }

    private func contactPhoto(_ source: PhotoSource, side: Int) async -> Data? {
        let fetched: Data?
        switch source {
        case .url(let url):
            fetched = await fetch(Self.sized(url, side: side))
        case .graphContact(let account, let id):
            guard let api = accounts.first(where: { $0.name == account })?.api,
                  let url = URL(string: "https://graph.microsoft.com/v1.0/me/contacts/\(id)/photo/$value")
            else { return nil }
            fetched = await fetch(url, api: api)
        }
        guard let fetched else { return nil }
        return await Self.normalize(fetched, style: .photo, side: side)
    }

    /// Google serves its pictures at any size (`=s96-c` after the path, replacing whatever options
    /// the link has, such as Chat's `=k-no`): ask for the one drawn. Other links stay as they are.
    private static func sized(_ url: URL, side: Int) -> URL {
        guard url.host()?.hasSuffix("googleusercontent.com") == true else { return url }
        var link = url.absoluteString
        if let range = link.range(of: #"=[^/=]*$"#, options: .regularExpression) { link.removeSubrange(range) }
        return URL(string: link + "=s\(side)-c") ?? url
    }

    // MARK: - Public sources

    private func gravatar(_ email: String, side: Int) async -> Data? {
        guard let url = URL(string: "https://gravatar.com/avatar/\(Self.hash(email))?s=\(side)&d=404") else { return nil }
        guard let fetched = await fetch(url) else { return nil }
        return await Self.normalize(fetched, style: .photo, side: side)
    }

    /// The BIMI logo, else the site icon, shared by every sender at the domain.
    private func domainLogo(_ domain: String, side: Int) async -> Data? {
        if let png = await bimi(domain, side: side) { return png }
        return await sharedPerDomain("i-", domain, side: side) { await self.siteIcon(Self.organization(domain), side: side) }
    }

    private func bimi(_ domain: String, side: Int) async -> Data? {
        await sharedPerDomain("b-", domain, side: side) { await self.lookUpBIMI(domain, side: side) }
    }

    /// One lookup per domain and kind, in memory while it runs and on disk; other sizes are fetched
    /// each time. A logo past its age is fetched again, and kept if that finds nothing.
    private func sharedPerDomain(_ kind: String, _ domain: String, side: Int,
                                 _ body: @escaping @Sendable () async -> Data?) async -> Data? {
        guard side == Self.side else { return await body() }
        let key = kind + Self.hash(domain)
        if let task = domains[key] { return await task.value }
        let task = Task { () async -> Data? in
            var old: Data?
            switch await cached(key) {
            case .hit(let png, let fresh):
                if fresh { return png }
                old = png
            case .miss: return nil
            case .unknown: break
            }
            guard let png = await body() else {
                if old == nil { await store(nil, at: key, stamp: nil) }
                return old
            }
            await store(png, at: key, stamp: nil)
            return png
        }
        domains[key] = task
        let png = await task.value
        domains[key] = nil
        return png
    }

    /// `default._bimi.<domain>`, falling back to the organizational domain as the spec does.
    private func lookUpBIMI(_ domain: String, side: Int) async -> Data? {
        var names = [domain]
        let organization = Self.organization(domain)
        if organization != domain { names.append(organization) }
        for name in names {
            let records = await DNSText.records("default._bimi." + name)
            guard let record = records.first(where: { $0.hasPrefix("v=BIMI1") }) else { continue }
            let link = record.split(separator: ";")
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .first { $0.hasPrefix("l=") }
                .map { String($0.dropFirst(2)) }
            guard let link, link.hasPrefix("https://"), let url = URL(string: link) else { return nil }
            guard let fetched = await fetch(url, limit: 128 * 1024) else { return nil }
            return await Self.normalize(fetched, style: .logo, side: side)
        }
        return nil
    }

    /// The home page's touch icon or largest declared icon, else `/favicon.ico`; icons smaller
    /// than 32 px are too blurry to use.
    private func siteIcon(_ domain: String, side: Int) async -> Data? {
        guard let home = URL(string: "https://\(domain)/") else { return nil }
        var candidates: [URL] = []
        if let (data, finalURL) = await fetchPage(home) {
            candidates = Self.iconLinks(in: String(decoding: data, as: UTF8.self), base: finalURL)
        }
        if let favicon = URL(string: "/favicon.ico", relativeTo: home)?.absoluteURL { candidates.append(favicon) }
        for url in candidates.prefix(4) {
            if let data = await fetch(url), let png = await Self.normalize(data, style: .icon, side: side) { return png }
        }
        return nil
    }

    private static func iconLinks(in html: String, base: URL) -> [URL] {
        let tags = html.matches(of: /<link\b[^>]*>/.ignoresCase()).map { String(html[$0.range]) }
        var scored: [(score: Int, url: URL)] = []
        for tag in tags {
            guard let rel = attribute("rel", in: tag)?.lowercased(), rel.contains("icon"),
                  let href = attribute("href", in: tag), let url = URL(string: href, relativeTo: base)?.absoluteURL
            else { continue }
            let size = attribute("sizes", in: tag).flatMap { Int($0.split(separator: "x").first ?? "") } ?? 0
            let score = (rel.contains("apple-touch-icon") ? 10_000 : 0) + (rel.contains("mask") ? -20_000 : 0) + size
            scored.append((score, url))
        }
        return scored.sorted { $0.score > $1.score }.map(\.url)
    }

    private static func attribute(_ name: String, in tag: String) -> String? {
        guard let regex = try? Regex("\\b\(name)\\s*=\\s*(\"([^\"]*)\"|'([^']*)'|([^\\s>]+))").ignoresCase(),
              let match = tag.firstMatch(of: regex)
        else { return nil }
        for index in 2...4 {
            if let range = match.output[index].range { return String(tag[range]) }
        }
        return nil
    }

    // MARK: - Network

    /// The body of a successful GET, as the account's user when `api` is given; nil on any failure
    /// or past `limit` bytes.
    private func fetch(_ url: URL, api: API? = nil, limit: Int = 2 * 1024 * 1024) async -> Data? {
        let answer: (data: Data, response: HTTPURLResponse?)?
        if let api {
            answer = (try? await api.send(API.request(url), priority: .background)).map { ($0.0, $0.1) }
        } else {
            answer = (try? await session.data(for: URLRequest(url: url))).map { ($0.0, $0.1 as? HTTPURLResponse) }
        }
        guard let answer, let http = answer.response, http.statusCode == 200, answer.data.count <= limit
        else { return nil }
        return answer.data
    }

    private func fetchPage(_ url: URL) async -> (Data, URL)? {
        guard let (data, response) = try? await session.data(from: url),
              let http = response as? HTTPURLResponse, http.statusCode == 200
        else { return nil }
        return (data.prefix(512 * 1024), http.url ?? url)
    }

    // MARK: - Helpers

    private static let freemail: Set<String> = [
        "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "yahoo.com", "ymail.com",
        "icloud.com", "me.com", "mac.com", "aol.com", "proton.me", "protonmail.com", "gmx.com", "gmx.de", "yandex.ru",
        "qq.com", "foxmail.com", "163.com", "126.com", "sina.com", "yeah.net", "naver.com",
    ]

    private static func looksAutomated(_ email: String) -> Bool {
        let local = email.split(separator: "@").first.map { $0.lowercased() } ?? ""
        return local.contains(/^(no-?reply|do-?not-?reply|notifications?|alerts?|news(letter)?|updates?|info|hello|team|support|mailer|digest|account|billing|security|service)\b/)
    }

    /// The registrable domain, roughly: the last two labels, or three under a short second level
    /// such as `co.uk`.
    static func organization(_ domain: String) -> String {
        let labels = domain.split(separator: ".")
        guard labels.count > 2 else { return domain }
        let keep = labels[labels.count - 1].count == 2 && labels[labels.count - 2].count <= 3 ? 3 : 2
        return labels.suffix(keep).joined(separator: ".")
    }

    /// Hex SHA-256 of `text` trimmed and lowercased, the way Gravatar keys addresses.
    private static func hash(_ text: String) -> String {
        hash(Data(text.trimmingCharacters(in: .whitespaces).lowercased().utf8))
    }

    private static func hash(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    /// `logo` fills like a photo unless its corners are see-through (a rounded square on nothing),
    /// then it sits inside on white like an icon.
    private nonisolated enum Style { case photo, logo, icon }

    /// A square PNG at the drawn size: photos and logos fill it; site icons sit inside on white,
    /// as their transparent corners and odd shapes don't survive a circular crop. Decodes off the
    /// resolver, so lookups holding permits decode in parallel.
    @concurrent private nonisolated static func normalize(_ data: Data, style: Style, side: Int) async -> Data? {
        guard let image = NSImage(data: data), image.isValid, image.size.width > 0, image.size.height > 0 else { return nil }
        let pixels = image.representations.map { max($0.pixelsWide, $0.pixelsHigh) }.max() ?? 0
        // Vector reps report no pixels; raster icons must be big enough to scale down cleanly.
        if style == .icon, pixels > 0, pixels < 32 { return nil }
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: side, pixelsHigh: side, bitsPerSample: 8,
                                         samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                         bytesPerRow: 0, bitsPerPixel: 0),
              let context = NSGraphicsContext(bitmapImageRep: rep)
        else { return nil }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        context.imageInterpolation = .high
        let canvas = NSRect(x: 0, y: 0, width: side, height: side)
        let size = image.size
        switch style {
        case .photo, .logo:
            let scale = max(canvas.width / size.width, canvas.height / size.height)
            let drawn = NSSize(width: size.width * scale, height: size.height * scale)
            image.draw(in: NSRect(x: (canvas.width - drawn.width) / 2, y: (canvas.height - drawn.height) / 2,
                                  width: drawn.width, height: drawn.height))
        case .icon:
            NSColor.white.setFill()
            canvas.fill()
            let box = canvas.insetBy(dx: canvas.width * 0.18, dy: canvas.height * 0.18)
            let scale = min(box.width / size.width, box.height / size.height)
            let drawn = NSSize(width: size.width * scale, height: size.height * scale)
            image.draw(in: NSRect(x: box.midX - drawn.width / 2, y: box.midY - drawn.height / 2,
                                  width: drawn.width, height: drawn.height))
        }
        NSGraphicsContext.restoreGraphicsState()
        if style == .logo {
            let corners = [(1, 1), (side - 2, 1), (1, side - 2), (side - 2, side - 2)]
            if corners.contains(where: { (rep.colorAt(x: $0.0, y: $0.1)?.alphaComponent ?? 0) < 0.5 }) {
                return await normalize(data, style: .icon, side: side)
            }
        }
        return rep.representation(using: .png, properties: [:])
    }
}

/// DNS TXT lookups through the system resolver, without blocking; empty after 4 s or on failure.
nonisolated enum DNSText {
    static func records(_ name: String) async -> [String] {
        let query = TXTQuery()
        return await withTaskGroup { group in
            // Times out the query, and tears it down at once when the caller is cancelled.
            group.addTask {
                try? await Task.sleep(for: .seconds(4))
                query.cancel()
            }
            let records = await withCheckedContinuation { query.start(name, $0) }
            group.cancelAll()
            return records
        }
    }
}

/// One query. DNSService has no async API and answers only on a dispatch queue, so this bridge
/// keeps one; a `Mutex` guards the state its callbacks share with the caller.
private nonisolated final class TXTQuery: Sendable {
    private static let queue = DispatchQueue(label: "dev.mailstream.dns")

    private struct State {
        var continuation: CheckedContinuation<[String], Never>?
        var ref: DNSServiceRef?
        var records: [String] = []
        var finished = false
    }

    private let state = Mutex(State())

    /// `DNSText.records` holds the query until it finishes, so the callback's context is unretained.
    func start(_ name: String, _ continuation: CheckedContinuation<[String], Never>) {
        let finished = state.withLock { state in
            if !state.finished { state.continuation = continuation }
            return state.finished
        }
        // Cancelled before it began.
        if finished { return continuation.resume(returning: []) }
        Self.queue.async { [self] in
            // Answers arrive on this queue too, so none can run while the lock is held here.
            let started = state.withLock { state in
                guard !state.finished else { return false }
                var ref: DNSServiceRef?
                let flags = DNSServiceFlags(kDNSServiceFlagsReturnIntermediates)
                let status = DNSServiceQueryRecord(&ref, flags, 0, name, UInt16(kDNSServiceType_TXT), UInt16(kDNSServiceClass_IN),
                                                   { _, flags, _, error, _, _, _, length, data, _, context in
                    guard let context else { return }
                    Unmanaged<TXTQuery>.fromOpaque(context).takeUnretainedValue()
                        .received(flags: flags, error: error, length: length, data: data)
                }, Unmanaged.passUnretained(self).toOpaque())
                guard status == DNSServiceErrorType(kDNSServiceErr_NoError), let ref else { return false }
                state.ref = ref
                DNSServiceSetDispatchQueue(ref, Self.queue)
                return true
            }
            if !started { finish() }
        }
    }

    /// Ends the query with what it has; the ref is deallocated on its queue, as DNSService requires.
    func cancel() {
        Self.queue.async { [self] in finish() }
    }

    private func received(flags: DNSServiceFlags, error: DNSServiceErrorType, length: UInt16, data: UnsafeRawPointer?) {
        guard error == DNSServiceErrorType(kDNSServiceErr_NoError) else { return finish() }
        if let data, length > 0 {
            // Character-strings, each prefixed by its length, concatenated into one record.
            let bytes = UnsafeRawBufferPointer(start: data, count: Int(length))
            var text = Data()
            var index = 0
            while index < bytes.count {
                let count = Int(bytes[index])
                let end = min(bytes.count, index + 1 + count)
                text.append(contentsOf: bytes[(index + 1)..<end])
                index = end
            }
            let record = String(decoding: text, as: UTF8.self)
            state.withLock { $0.records.append(record) }
        }
        if flags & DNSServiceFlags(kDNSServiceFlagsMoreComing) == 0 { finish() }
    }

    private func finish() {
        let (continuation, ref, records) = state.withLock { state in
            defer { state = State(finished: true) }
            return (state.continuation, state.ref, state.records)
        }
        if let ref { DNSServiceRefDeallocate(ref) }
        continuation?.resume(returning: records)
    }
}

/// An account whose contacts give sender photos.
nonisolated struct PhotoAccount: Sendable {
    let api: API
    let provider: Provider
    /// Also read the Workspace domain's directory (Google).
    let directory: Bool

    var name: String { api.account }
}
