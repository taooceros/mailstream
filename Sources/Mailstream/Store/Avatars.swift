import AppKit
import CryptoKit
import dnssd
import Observation
import Synchronization

/// Sender pictures for the list and the reader, looked up lazily for what is on screen.
///
/// Sources, best first: the photo of a Google or Outlook contact (accounts granted contacts
/// access), the sender domain's BIMI logo, Gravatar, and for automated mail the
/// domain's site icon. Anything without a picture keeps its initials.
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

    /// The picture for `email` when it is already known; otherwise starts looking and returns nil,
    /// bumping `revision` if one turns up. `automated` allows the domain's site icon.
    func image(for email: String, automated: Bool) -> NSImage? {
        let key = email.lowercased()
        if let image = images.object(forKey: key as NSString) { return image }
        request(key, automated: automated)
        return nil
    }

    /// The picture for `email` if one is in memory, without looking for one.
    func cachedImage(for email: String) -> NSImage? {
        images.object(forKey: email.lowercased() as NSString)
    }

    /// The same picture as a `data:` URL, for the reader page.
    func dataURL(for email: String, automated: Bool) -> String? {
        let key = email.lowercased()
        guard let png = pngs.object(forKey: key as NSString) else {
            request(key, automated: automated)
            return nil
        }
        return "data:image/png;base64," + (png as Data).base64EncodedString()
    }

    /// The same picture at 512 px for an enlarged view, fetched from its source on demand.
    func largeImage(for email: String, automated: Bool) async -> NSImage? {
        let key = email.lowercased()
        if let image = large.object(forKey: key as NSString) { return image }
        guard let png = await resolver.avatar(for: key, automated: automated, side: 512),
              let image = NSImage(data: png) else { return nil }
        large.setObject(image, forKey: key as NSString)
        return image
    }

    private func request(_ key: String, automated: Bool) {
        guard key.contains("@"), !missing.contains(key), pending.insert(key).inserted else { return }
        let resolver = resolver
        Task {
            let png = await resolver.avatar(for: key, automated: automated)
            pending.remove(key)
            guard let png, let image = NSImage(data: png) else {
                missing.insert(key)
                return
            }
            images.setObject(image, forKey: key as NSString)
            pngs.setObject(png as NSData, forKey: key as NSString)
            revision += 1
        }
    }
}

/// Finds and caches one normalized 96 px PNG per sender. Results, including misses, live on disk
/// so a relaunch paints from cache; a miss is retried once the contact directory changes. Other
/// sizes, for enlarged views, go to the same source uncached.
actor AvatarResolver {
    /// The cached size: rows and the reader draw at most 34 pt, so 96 px covers Retina.
    static let side = 96
    private static let hit: TimeInterval = 14 * 86_400
    private static let miss: TimeInterval = 3 * 86_400
    private static let directoryAge: TimeInterval = 86_400

    private var accounts: [PhotoAccount]
    private let root: URL
    private let session: URLSession
    private let permits = AsyncSemaphore(6)
    private var directory: Task<Directory, Never>?
    private var domains: [String: Task<Data?, Never>] = [:]

    /// Contact address → where its photo is: an image URL, or `graph:<account>/<contact id>`.
    private struct Directory: Codable {
        var built: Date
        /// The accounts it loaded; any difference from the current photo accounts (one added,
        /// removed, or failed last time) rebuilds it at once.
        var accounts: [String]
        var photos: [String: String]
        /// Changes whenever the set of contact photos does, retiring cached misses.
        var stamp: String { String(photos.count) }
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

    func avatar(for email: String, automated: Bool, side: Int = AvatarResolver.side) async -> Data? {
        let automated = automated || Self.looksAutomated(email)
        guard side == Self.side else { return await find(email, automated: automated, side: side) }
        let key = "p-" + Self.hash(email)
        let stamp = await contacts().stamp
        switch cached(key, stamp: stamp) {
        case .hit(let png): return png
        case .miss: return nil
        case .unknown: break
        }
        let png: Data?
        do {
            png = try await permits.withPermit { await find(email, automated: automated, side: side) }
        } catch {
            // Cancelled while waiting for a permit: nothing was looked up, so no miss is cached.
            return nil
        }
        store(png, at: key, stamp: stamp)
        return png
    }

    private func find(_ email: String, automated: Bool, side: Int) async -> Data? {
        if let source = await contacts().photos[email], let png = await contactPhoto(source, side: side) { return png }
        let domain = email.split(separator: "@").last.map(String.init) ?? ""
        let company = !domain.isEmpty && !Self.freemail.contains(domain)
        if automated, company, let png = await domainLogo(domain, side: side) { return png }
        if let png = await gravatar(email, side: side) { return png }
        if !automated, company, let png = await bimi(domain, side: side) { return png }
        return nil
    }

    // MARK: - Disk cache

    private enum Cached { case hit(Data), miss, unknown }

    private func cached(_ key: String, stamp: String?) -> Cached {
        let png = root.appendingPathComponent(key + ".png")
        if let date = Self.modified(png), Date().timeIntervalSince(date) < Self.hit, let data = try? Data(contentsOf: png) {
            return .hit(data)
        }
        let none = root.appendingPathComponent(key + ".none")
        if let date = Self.modified(none), Date().timeIntervalSince(date) < Self.miss,
           stamp == nil || (try? String(contentsOf: none, encoding: .utf8)) == stamp {
            return .miss
        }
        return .unknown
    }

    /// `stamp` ties a miss to the directory it was made against; nil for domain-wide results.
    private func store(_ png: Data?, at key: String, stamp: String?) {
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

    /// Takes the current photo accounts; a different set reloads the directory.
    func use(_ fresh: [PhotoAccount]) {
        guard fresh.map(\.name) != accounts.map(\.name) || fresh.map(\.directory) != accounts.map(\.directory) else { return }
        accounts = fresh
        directory = nil
    }

    private func contacts() async -> Directory {
        if let directory { return await directory.value }
        let task = Task { await loadDirectory() }
        directory = task
        return await task.value
    }

    private func loadDirectory() async -> Directory {
        let url = root.appendingPathComponent("directory.json")
        let accounts = accounts
        if let data = try? Data(contentsOf: url), let saved = try? JSONDecoder().decode(Directory.self, from: data),
           Set(accounts.map(\.name)) == Set(saved.accounts), Date().timeIntervalSince(saved.built) < Self.directoryAge {
            return saved
        }
        var photos: [String: String] = [:]
        var loaded: [String] = []
        for account in accounts {
            let found = switch account.provider {
            case .google: await googlePhotos(api: account.api, directory: account.directory)
            case .microsoft: await graphPhotos(api: account.api)
            }
            guard let found else { continue }
            loaded.append(account.name)
            photos.merge(found) { current, _ in current }
        }
        let fresh = Directory(built: Date(), accounts: loaded, photos: photos)
        if let data = try? JSONEncoder().encode(fresh) { try? data.write(to: url, options: .atomic) }
        return fresh
    }

    /// Saved contacts, "other contacts" (people the user has written with) and, on Workspace
    /// accounts, the organization's directory. Nil when every one of them is refused.
    private func googlePhotos(api: API, directory: Bool) async -> [String: String]? {
        let base = "https://people.googleapis.com/v1/"
        var lists = [
            ("people/me/connections?personFields=emailAddresses,photos&pageSize=1000", "connections"),
            ("otherContacts?readMask=emailAddresses,photos&pageSize=1000", "otherContacts"),
        ]
        if directory {
            lists.append(("people:listDirectoryPeople?readMask=emailAddresses,photos&sources=DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE&pageSize=1000",
                          "people"))
        }
        var photos: [String: String] = [:]
        var answered = false
        for (path, field) in lists {
            var pageToken: String?
            for _ in 0..<30 {
                let page = pageToken.map { "&pageToken=" + ($0.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? $0) } ?? ""
                guard let url = URL(string: base + path + page), let json = await getJSON(url, api: api) else { break }
                answered = true
                for person in json[field] as? [[String: Any]] ?? [] {
                    let photo = (person["photos"] as? [[String: Any]] ?? []).first { $0["default"] as? Bool != true }
                    guard let link = photo?["url"] as? String else { continue }
                    for address in person["emailAddresses"] as? [[String: Any]] ?? [] {
                        if let email = (address["value"] as? String)?.lowercased(), photos[email] == nil { photos[email] = link }
                    }
                }
                pageToken = json["nextPageToken"] as? String
                if pageToken == nil { break }
            }
        }
        return answered ? photos : nil
    }

    /// Outlook contacts; whether one has a photo is only known when it is fetched. Nil when the
    /// request is refused.
    private func graphPhotos(api: API) async -> [String: String]? {
        var photos: [String: String] = [:]
        var answered = false
        var next = URL(string: "https://graph.microsoft.com/v1.0/me/contacts?$select=emailAddresses&$top=999")
        for _ in 0..<30 {
            guard let url = next, let json = await getJSON(url, api: api) else { break }
            answered = true
            for contact in json["value"] as? [[String: Any]] ?? [] {
                guard let id = contact["id"] as? String else { continue }
                for address in contact["emailAddresses"] as? [[String: Any]] ?? [] {
                    if let email = (address["address"] as? String)?.lowercased(), photos[email] == nil {
                        photos[email] = "graph:\(api.account)/\(id)"
                    }
                }
            }
            next = (json["@odata.nextLink"] as? String).flatMap(URL.init(string:))
        }
        return answered ? photos : nil
    }

    private func contactPhoto(_ source: String, side: Int) async -> Data? {
        if source.hasPrefix("graph:") {
            let rest = source.dropFirst("graph:".count)
            guard let slash = rest.firstIndex(of: "/") else { return nil }
            let account = String(rest[..<slash]), id = String(rest[rest.index(after: slash)...])
            guard let api = accounts.first(where: { $0.name == account })?.api,
                  let url = URL(string: "https://graph.microsoft.com/v1.0/me/contacts/\(id)/photo/$value")
            else { return nil }
            return await fetch(url, api: api).flatMap { Self.normalize($0, style: .photo, side: side) }
        }
        // Google serves any size; ask for the one drawn.
        var link = source
        if let range = link.range(of: #"=s\d+(-c)?$"#, options: .regularExpression) { link.removeSubrange(range) }
        guard let url = URL(string: link + "=s\(side)-c") else { return nil }
        return await fetch(url).flatMap { Self.normalize($0, style: .photo, side: side) }
    }

    // MARK: - Public sources

    private func gravatar(_ email: String, side: Int) async -> Data? {
        guard let url = URL(string: "https://gravatar.com/avatar/\(Self.hash(email))?s=\(side)&d=404") else { return nil }
        return await fetch(url).flatMap { Self.normalize($0, style: .photo, side: side) }
    }

    /// The BIMI logo, else the site icon, shared by every sender at the domain.
    private func domainLogo(_ domain: String, side: Int) async -> Data? {
        if let png = await bimi(domain, side: side) { return png }
        return await sharedPerDomain("i-", domain, side: side) { await self.siteIcon(Self.organization(domain), side: side) }
    }

    private func bimi(_ domain: String, side: Int) async -> Data? {
        await sharedPerDomain("b-", domain, side: side) { await self.lookUpBIMI(domain, side: side) }
    }

    /// One lookup per domain and kind, in memory and on disk; other sizes are fetched each time.
    private func sharedPerDomain(_ kind: String, _ domain: String, side: Int,
                                 _ body: @escaping @Sendable () async -> Data?) async -> Data? {
        guard side == Self.side else { return await body() }
        let key = kind + Self.hash(domain)
        if let task = domains[key] { return await task.value }
        switch cached(key, stamp: nil) {
        case .hit(let png): return png
        case .miss: return nil
        case .unknown: break
        }
        let task = Task { await body() }
        domains[key] = task
        let png = await task.value
        store(png, at: key, stamp: nil)
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
            return await fetch(url, limit: 128 * 1024).flatMap { Self.normalize($0, style: .logo, side: side) }
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
            if let data = await fetch(url), let png = Self.normalize(data, style: .icon, side: side) { return png }
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

    private func getJSON(_ url: URL, api: API) async -> [String: Any]? {
        guard let data = await fetch(url, api: api, limit: 16 * 1024 * 1024) else { return nil }
        return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }

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
        let data = answer.data
        return data
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

    private static func hash(_ text: String) -> String {
        SHA256.hash(data: Data(text.trimmingCharacters(in: .whitespaces).lowercased().utf8))
            .map { String(format: "%02x", $0) }.joined()
    }

    /// `logo` fills like a photo unless its corners are see-through (a rounded square on nothing),
    /// then it sits inside on white like an icon.
    private enum Style { case photo, logo, icon }

    /// A square PNG at the drawn size: photos and logos fill it; site icons sit inside on white,
    /// as their transparent corners and odd shapes don't survive a circular crop.
    private static func normalize(_ data: Data, style: Style, side: Int) -> Data? {
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
                return normalize(data, style: .icon, side: side)
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
