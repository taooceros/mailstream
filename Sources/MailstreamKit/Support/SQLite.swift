import CTurso
import Foundation

/// One connection to a database file through the Turso engine's SQLite C API. Not thread-safe:
/// an actor owns it.
nonisolated final class SQLiteConnection {
    enum Value {
        case text(String)
        case blob(Data)
        case integer(Int64)
        case null
    }

    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// One result row, read while the statement stands on it.
    struct Row {
        fileprivate let statement: OpaquePointer

        func text(_ column: Int32) -> String? {
            sqlite3_column_text(statement, column).map { String(cString: $0) }
        }

        func data(_ column: Int32) -> Data? {
            guard let bytes = sqlite3_column_blob(statement, column) else { return nil }
            return Data(bytes: bytes, count: Int(sqlite3_column_bytes(statement, column)))
        }
    }

    private let handle: OpaquePointer

    /// Every process that opens mail.db (the app, FFI front ends) shares it through Turso's
    /// multi-process WAL; Turso refuses a file open in both modes at once. On Windows only the
    /// Flutter app opens it, and Turso's Windows file layer has no shared WAL, so it stays off.
    private static let multiprocess: Void = {
        #if !os(Windows)
        turso_enable_multiprocess_wal()
        #endif
    }()

    init(path: String) throws {
        _ = Self.multiprocess
        var handle: OpaquePointer?
        let code = sqlite3_open(path, &handle)
        guard code == SQLITE_OK, let handle else {
            if let handle { sqlite3_close(handle) }
            throw Failure(message: "Cannot open \(path) (code \(code))")
        }
        // Another process writing holds the WAL briefly; wait for it rather than fail the write.
        sqlite3_busy_timeout(handle, 2000)
        self.handle = handle
    }

    deinit { sqlite3_close(handle) }

    /// Runs `sql`, which may hold several statements and takes no values.
    func script(_ sql: String) throws {
        let code = sqlite3_exec(handle, sql, nil, nil, nil)
        guard code == SQLITE_OK else { throw failure(code) }
    }

    func execute(_ sql: String, _ values: [Value] = []) throws {
        try prepared(sql, values) { statement in
            while true {
                let code = sqlite3_step(statement)
                if code == SQLITE_DONE { return }
                guard code == SQLITE_ROW else { throw failure(code) }
            }
        }
    }

    func rows<T>(_ sql: String, _ values: [Value] = [], _ read: (Row) throws -> T?) throws -> [T] {
        try prepared(sql, values) { statement in
            var out: [T] = []
            while true {
                let code = sqlite3_step(statement)
                if code == SQLITE_DONE { return out }
                guard code == SQLITE_ROW else { throw failure(code) }
                if let value = try read(Row(statement: statement)) { out.append(value) }
            }
        }
    }

    /// Runs `body` in one transaction, rolled back when it throws.
    func transaction<T>(_ body: () throws -> T) throws -> T {
        try execute("BEGIN")
        do {
            let result = try body()
            try execute("COMMIT")
            return result
        } catch {
            try? execute("ROLLBACK")
            throw error
        }
    }

    private func prepared<T>(_ sql: String, _ values: [Value], _ run: (OpaquePointer) throws -> T) throws -> T {
        var statement: OpaquePointer?
        let code = sqlite3_prepare_v2(handle, sql, -1, &statement, nil)
        guard code == SQLITE_OK, let statement else { throw failure(code) }
        defer { sqlite3_finalize(statement) }
        for (offset, value) in values.enumerated() {
            let index = Int32(offset + 1)
            let bound = switch value {
            case .text(let text): sqlite3_bind_text(statement, index, text, -1, Self.transient)
            case .blob(let data): data.withUnsafeBytes { sqlite3_bind_blob(statement, index, $0.baseAddress, Int32($0.count), Self.transient) }
            case .integer(let number): sqlite3_bind_int64(statement, index, number)
            case .null: sqlite3_bind_null(statement, index)
            }
            guard bound == SQLITE_OK else { throw failure(bound) }
        }
        return try run(statement)
    }

    private func failure(_ code: Int32) -> Failure {
        Failure(message: "\(sqlite3_errmsg(handle).map { String(cString: $0) } ?? "SQLite error") (code \(code))")
    }

    /// SQLITE_TRANSIENT: the engine copies bound text and blobs before the call returns.
    private static var transient: UnsafeMutableRawPointer? { UnsafeMutableRawPointer(bitPattern: -1) }
}
