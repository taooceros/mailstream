import Foundation

/// A JSON file the app keeps on disk: decoded whole, replaced atomically.
nonisolated enum JSONFile {
    /// The file's value; nil when it is missing or does not decode.
    static func read<T: Decodable>(_ type: T.Type, at url: URL, decoder: JSONDecoder = JSONDecoder()) -> T? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? decoder.decode(type, from: data)
    }

    /// Encodes `value` and replaces the file with it, creating the file's directory first when asked.
    static func write(_ value: some Encodable, to url: URL, encoder: JSONEncoder = JSONEncoder(),
                      creatingDirectory: Bool = false) throws {
        let data = try encoder.encode(value)
        if creatingDirectory {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        }
        try data.write(to: url, options: .atomic)
    }
}
