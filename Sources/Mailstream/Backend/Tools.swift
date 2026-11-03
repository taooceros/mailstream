import Foundation

/// The environment omp needs. A GUI app inherits launchd's minimal PATH, so the user's
/// login-shell PATH is recovered once at startup.
struct ToolEnvironment: Sendable {
    let environment: [String: String]

    static func resolve() -> ToolEnvironment {
        var env = ProcessInfo.processInfo.environment
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        var dirs: [String] = []
        if let loginPath = loginShellPath() {
            dirs += loginPath.split(separator: ":").map(String.init)
        }
        dirs += ["\(home)/.local/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        dirs += (env["PATH"] ?? "").split(separator: ":").map(String.init)
        var seen = Set<String>()
        dirs = dirs.filter { !$0.isEmpty && seen.insert($0).inserted }
        env["PATH"] = dirs.joined(separator: ":")
        env["NO_COLOR"] = "1"
        return ToolEnvironment(environment: env)
    }

    /// Executable files called `name` on the recovered PATH, in PATH order, one per resolved target.
    func executables(named name: String) -> [String] {
        let fm = FileManager.default
        var seen = Set<String>()
        return (environment["PATH"] ?? "").split(separator: ":")
            .map { "\($0)/\(name)" }
            .filter { fm.isExecutableFile(atPath: $0)
                && seen.insert(URL(fileURLWithPath: $0).resolvingSymlinksInPath().path).inserted }
    }

    /// Runs a short-lived command, giving up after three seconds.
    static func runQuickly(_ executable: String, _ arguments: [String], environment: [String: String]? = nil) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        if let environment { process.environment = environment }
        let out = Pipe()
        process.standardOutput = out
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return nil }
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { process.waitUntilExit(); done.signal() }
        if done.wait(timeout: .now() + 3) == .timedOut {
            process.terminate()
            return nil
        }
        return String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    }

    private static func loginShellPath() -> String? {
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        let text = runQuickly(shell, ["-l", "-c", "printf '__MS_PATH__%s__MS_PATH__' \"$PATH\""]) ?? ""
        let parts = text.components(separatedBy: "__MS_PATH__")
        return parts.count >= 3 ? parts[1] : nil
    }
}
