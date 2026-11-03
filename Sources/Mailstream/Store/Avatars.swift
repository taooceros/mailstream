import AppKit
import CryptoKit
import dnssd

extension Notification.Name {
    /// A sender's avatar image arrived; `userInfo["email"]` is the lowercased address.
    static let avatarResolved = Notification.Name("AvatarStore.resolved")
}

/// Sender pictures for the list and the reader, looked up lazily for what is on screen.
///
/// Sources, best first: the photo of a Google or Outlook contact (ortie accounts named
/// `<name>-contacts`), the sender domain's BIMI logo, Gravatar, and for automated mail the
/// domain's site icon. Anything without a picture keeps its initials.
@MainActor
final class AvatarStore {
    private let resolver: AvatarResolver
    private let images = NSCache<NSString, NSImage>()
    private let pngs = NSCache<NSString, NSData>()
    private var missing = Set<String>()
    private var pending = Set<String>()

    init(tools: ToolEnvironment, root: URL) {
        resolver = AvatarResolver(tools: tools, root: root.appendingPathComponent("avatars", isDirectory: true))
        images.countLimit = 400
        pngs.countLimit = 400
    }

    /// The picture for `email` when it is already known; otherwise starts looking and returns nil,
    /// posting `.avatarResolved` if one turns up. `automated` allows the domain's site icon.
    func image(for email: String, automated: Bool) -> NSImage? {
        let key = email.lowercased()
        if let image = images.object(forKey: key as NSString) { return image }
        request(key, automated: automated)
        return nil
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
            NotificationCenter.default.post(name: .avatarResolved, object: self, userInfo: ["email": key])
        }
    }
}

/// Finds and caches one normalized 96 px PNG per sender. Results, including misses, live on disk
/// so a relaunch paints from cache; a miss is retried once the contact directory changes.
actor AvatarResolver {
    private static let side = 96
    private static let hit: TimeInterval = 14 * 86_400
    private static let miss: TimeInterval = 3 * 86_400
    private static let directoryAge: TimeInterval = 86_400

    private let tools: ToolEnvironment
    private let root: URL
    private let session: URLSession
    private let permits = AsyncSemaphore(6)
    private var directory: Task<Directory, Never>?
    private var domains: [String: Task<Data?, Never>] = [:]
    private var tokens: [String: (token: String, until: Date)] = [:]

    /// Contact address → where its photo is: an image URL, or `graph:<account>/<contact id>`.
    private struct Directory: Codable {
        var built: Date
        /// The ortie accounts it loaded; a configured account missing here (new, or failed last
        /// time) rebuilds it at once.
        var accounts: [String]
        var photos: [String: String]
        /// Changes whenever the set of contact photos does, retiring cached misses.
        var stamp: String { String(photos.count) }
    }

    init(tools: ToolEnvironment, root: URL) {
        self.tools = tools
        self.root = root
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10
        configuration.httpAdditionalHeaders = ["User-Agent": "Mailstream"]
        session = URLSession(configuration: configuration)
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    func avatar(for email: String, automated: Bool) async -> Data? {
        let key = "p-" + Self.hash(email)
        let stamp = await contacts().stamp
        switch cached(key, stamp: stamp) {
        case .hit(let png): return png
        case .miss: return nil
        case .unknown: break
        }
        await permits.acquire()
        let png = await find(email, automated: automated || Self.looksAutomated(email))
        await permits.release()
        store(png, at: key, stamp: stamp)
        return png
    }

    private func find(_ email: String, automated: Bool) async -> Data? {
        if let source = await contacts().photos[email], let png = await contactPhoto(source) { return png }
        let domain = email.split(separator: "@").last.map(String.init) ?? ""
        let company = !domain.isEmpty && !Self.freemail.contains(domain)
        if automated, company, let png = await domainLogo(domain) { return png }
        if let png = await gravatar(email) { return png }
        if !automated, company, let png = await bimi(domain) { return png }
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

    private func contacts() async -> Directory {
        if let directory { return await directory.value }
        let task = Task { await loadDirectory() }
        directory = task
        return await task.value
    }

    private func loadDirectory() async -> Directory {
        let url = root.appendingPathComponent("directory.json")
        let accounts = Self.photoAccounts()
        if let data = try? Data(contentsOf: url), let saved = try? JSONDecoder().decode(Directory.self, from: data),
           Set(accounts.map(\.name)).isSubset(of: saved.accounts), Date().timeIntervalSince(saved.built) < Self.directoryAge {
            return saved
        }
        var photos: [String: String] = [:]
        var loaded: [String] = []
        for account in accounts {
            guard let token = await token(account.name) else { continue }
            let found = switch account.provider {
            case .google: await googlePhotos(token: token)
            case .microsoft: await graphPhotos(token: token, account: account.name)
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
    /// accounts, the organization's directory. Nil when the token is refused for all of them.
    private func googlePhotos(token: String) async -> [String: String]? {
        let base = "https://people.googleapis.com/v1/"
        let lists = [
            ("people/me/connections?personFields=emailAddresses,photos&pageSize=1000", "connections"),
            ("otherContacts?readMask=emailAddresses,photos&pageSize=1000", "otherContacts"),
            ("people:listDirectoryPeople?readMask=emailAddresses,photos&sources=DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE&pageSize=1000",
             "people"),
        ]
        var photos: [String: String] = [:]
        var answered = false
        for (path, field) in lists {
            var pageToken: String?
            for _ in 0..<30 {
                let page = pageToken.map { "&pageToken=" + ($0.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? $0) } ?? ""
                guard let url = URL(string: base + path + page), let json = await getJSON(url, token: token) else { break }
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
    /// token is refused.
    private func graphPhotos(token: String, account: String) async -> [String: String]? {
        var photos: [String: String] = [:]
        var answered = false
        var next = URL(string: "https://graph.microsoft.com/v1.0/me/contacts?$select=emailAddresses&$top=999")
        for _ in 0..<30 {
            guard let url = next, let json = await getJSON(url, token: token) else { break }
            answered = true
            for contact in json["value"] as? [[String: Any]] ?? [] {
                guard let id = contact["id"] as? String else { continue }
                for address in contact["emailAddresses"] as? [[String: Any]] ?? [] {
                    if let email = (address["address"] as? String)?.lowercased(), photos[email] == nil {
                        photos[email] = "graph:\(account)/\(id)"
                    }
                }
            }
            next = (json["@odata.nextLink"] as? String).flatMap(URL.init(string:))
        }
        return answered ? photos : nil
    }

    private func contactPhoto(_ source: String) async -> Data? {
        if source.hasPrefix("graph:") {
            let rest = source.dropFirst("graph:".count)
            guard let slash = rest.firstIndex(of: "/") else { return nil }
            let account = String(rest[..<slash]), id = String(rest[rest.index(after: slash)...])
            guard let token = await token(account),
                  let url = URL(string: "https://graph.microsoft.com/v1.0/me/contacts/\(id)/photo/$value")
            else { return nil }
            return await fetch(url, token: token).flatMap { Self.normalize($0, style: .photo) }
        }
        // Google serves any size; ask for the one drawn.
        var link = source
        if let range = link.range(of: #"=s\d+(-c)?$"#, options: .regularExpression) { link.removeSubrange(range) }
        guard let url = URL(string: link + "=s\(Self.side)-c") else { return nil }
        return await fetch(url).flatMap { Self.normalize($0, style: .photo) }
    }

    // MARK: - Public sources

    private func gravatar(_ email: String) async -> Data? {
        guard let url = URL(string: "https://gravatar.com/avatar/\(Self.hash(email))?s=\(Self.side)&d=404") else { return nil }
        return await fetch(url).flatMap { Self.normalize($0, style: .photo) }
    }

    /// The BIMI logo, else the site icon, shared by every sender at the domain.
    private func domainLogo(_ domain: String) async -> Data? {
        if let png = await bimi(domain) { return png }
        return await sharedPerDomain("i-", domain) { await self.siteIcon(Self.organization(domain)) }
    }

    private func bimi(_ domain: String) async -> Data? {
        await sharedPerDomain("b-", domain) { await self.lookUpBIMI(domain) }
    }

    /// One lookup per domain and kind, in memory and on disk.
    private func sharedPerDomain(_ kind: String, _ domain: String, _ body: @escaping @Sendable () async -> Data?) async -> Data? {
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
    private func lookUpBIMI(_ domain: String) async -> Data? {
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
            return await fetch(url, limit: 128 * 1024).flatMap { Self.normalize($0, style: .logo) }
        }
        return nil
    }

    /// The home page's touch icon or largest declared icon, else `/favicon.ico`; icons smaller
    /// than 32 px are too blurry to use.
    private func siteIcon(_ domain: String) async -> Data? {
        guard let home = URL(string: "https://\(domain)/") else { return nil }
        var candidates: [URL] = []
        if let (data, finalURL) = await fetchPage(home) {
            candidates = Self.iconLinks(in: String(decoding: data, as: UTF8.self), base: finalURL)
        }
        if let favicon = URL(string: "/favicon.ico", relativeTo: home)?.absoluteURL { candidates.append(favicon) }
        for url in candidates.prefix(4) {
            if let data = await fetch(url), let png = Self.normalize(data, style: .icon) { return png }
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

    private func token(_ account: String) async -> String? {
        if let cached = tokens[account], cached.until > Date() { return cached.token }
        guard let ortie = tools.executables(named: "ortie").first,
              let output = try? await ProcessRunner.run(executable: URL(fileURLWithPath: ortie),
                                                        arguments: ["-a", account, "token", "show"],
                                                        environment: tools.environment),
              output.status == 0
        else { return nil }
        let token = String(decoding: output.stdout, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else { return nil }
        // ortie refreshes on read; reuse a token well inside its hour.
        tokens[account] = (token, Date().addingTimeInterval(40 * 60))
        return token
    }

    private func getJSON(_ url: URL, token: String) async -> [String: Any]? {
        guard let data = await fetch(url, token: token, limit: 16 * 1024 * 1024) else { return nil }
        return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }

    /// The body of a successful GET, or nil on any failure or past `limit` bytes.
    private func fetch(_ url: URL, token: String? = nil, limit: Int = 2 * 1024 * 1024) async -> Data? {
        var request = URLRequest(url: url)
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        guard let (data, response) = try? await session.data(for: request),
              let http = response as? HTTPURLResponse, http.statusCode == 200, data.count <= limit
        else { return nil }
        return data
    }

    private func fetchPage(_ url: URL) async -> (Data, URL)? {
        guard let (data, response) = try? await session.data(from: url),
              let http = response as? HTTPURLResponse, http.statusCode == 200
        else { return nil }
        return (data.prefix(512 * 1024), http.url ?? url)
    }

    // MARK: - Helpers

    private enum Provider { case google, microsoft }

    /// ortie accounts named `*-contacts`, by the provider their authorization endpoint belongs to.
    private static func photoAccounts() -> [(name: String, provider: Provider)] {
        let env = ProcessInfo.processInfo.environment["ORTIE_CONFIG"]?.split(separator: ":").first.map(String.init)
        let path = env.map { ($0 as NSString).expandingTildeInPath }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config/ortie/config.toml").path
        guard let text = try? String(contentsOfFile: path, encoding: .utf8) else { return [] }
        var accounts: [(String, Provider)] = []
        var current: String?
        for line in text.split(separator: "\n").map({ $0.trimmingCharacters(in: .whitespaces) }) {
            if line.hasPrefix("[accounts."), line.hasSuffix("]") {
                let name = String(line.dropFirst("[accounts.".count).dropLast())
                current = name.hasSuffix("-contacts") ? name : nil
            } else if let name = current, line.hasPrefix("endpoints.authorization") {
                if line.contains("accounts.google.com") { accounts.append((name, .google)) }
                if line.contains("login.microsoftonline.com") { accounts.append((name, .microsoft)) }
                current = nil
            }
        }
        return accounts
    }

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
    private static func normalize(_ data: Data, style: Style) -> Data? {
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
                return normalize(data, style: .icon)
            }
        }
        return rep.representation(using: .png, properties: [:])
    }
}

/// DNS TXT lookups through the system resolver, answered on a private queue without blocking.
enum DNSText {
    static func records(_ name: String) async -> [String] {
        await withCheckedContinuation { continuation in
            TXTQuery(continuation).start(name)
        }
    }
}

/// One query; every field is touched only on `queue`.
private final class TXTQuery: @unchecked Sendable {
    private static let queue = DispatchQueue(label: "dev.mailstream.dns")
    private var continuation: CheckedContinuation<[String], Never>?
    private var ref: DNSServiceRef?
    private var records: [String] = []

    init(_ continuation: CheckedContinuation<[String], Never>) {
        self.continuation = continuation
    }

    func start(_ name: String) {
        Self.queue.async { [self] in
            // The callback's context holds a reference until the query is torn down in `finish`.
            let context = Unmanaged.passRetained(self).toOpaque()
            var ref: DNSServiceRef?
            let flags = DNSServiceFlags(kDNSServiceFlagsReturnIntermediates)
            let status = DNSServiceQueryRecord(&ref, flags, 0, name, UInt16(kDNSServiceType_TXT), UInt16(kDNSServiceClass_IN),
                                               { _, flags, _, error, _, _, _, length, data, _, context in
                guard let context else { return }
                Unmanaged<TXTQuery>.fromOpaque(context).takeUnretainedValue()
                    .received(flags: flags, error: error, length: length, data: data)
            }, context)
            guard status == DNSServiceErrorType(kDNSServiceErr_NoError), let ref else {
                Unmanaged<TXTQuery>.fromOpaque(context).release()
                finish()
                return
            }
            self.ref = ref
            DNSServiceSetDispatchQueue(ref, Self.queue)
            Self.queue.asyncAfter(deadline: .now() + 4) { self.finish() }
        }
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
            records.append(String(decoding: text, as: UTF8.self))
        }
        if flags & DNSServiceFlags(kDNSServiceFlagsMoreComing) == 0 { finish() }
    }

    private func finish() {
        guard let continuation else { return }
        self.continuation = nil
        if let ref {
            DNSServiceRefDeallocate(ref)
            self.ref = nil
            Unmanaged.passUnretained(self).release()
        }
        continuation.resume(returning: records)
    }
}
