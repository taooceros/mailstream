import Foundation

/// The environment omp needs. A GUI app inherits launchd's minimal PATH, so the user's
/// login-shell PATH is recovered once, in the background from startup on.
nonisolated struct ToolEnvironment: Sendable {
    private let resolution: Task<[String: String], Never>

    /// Starts recovering the environment; `environment` waits for it.
    static func resolve() -> ToolEnvironment {
        ToolEnvironment(resolution: Task(name: "Recover login PATH") { await recover() })
    }

    var environment: [String: String] {
        get async { await resolution.value }
    }

    /// Executable files called `name` on the recovered PATH, in PATH order, one per resolved target.
    func executables(named name: String) async -> [String] {
        let fm = FileManager.default
        var seen = Set<String>()
        return (await environment["PATH"] ?? "").split(separator: ":")
            .map { "\($0)/\(name)" }
            .filter { fm.isExecutableFile(atPath: $0)
                && seen.insert(URL(fileURLWithPath: $0).resolvingSymlinksInPath().path).inserted }
    }

    /// Runs a short-lived command for its output, giving up after three seconds.
    static func runQuickly(_ executable: String, _ arguments: [String], environment: [String: String]) async -> String? {
        try? await withThrowingTaskGroup { group in
            group.addTask {
                let output = try await ProcessRunner.run(executable: URL(fileURLWithPath: executable),
                                                         arguments: arguments, environment: environment)
                return String(decoding: output.stdout, as: UTF8.self)
            }
            group.addTask {
                try await Task.sleep(for: .seconds(3))
                throw CancellationError()
            }
            defer { group.cancelAll() }
            return try await group.next()
        }
    }

    @concurrent
    private static func recover() async -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        var dirs: [String] = []
        if let loginPath = await loginShellPath(environment: env) {
            dirs += loginPath.split(separator: ":").map(String.init)
        }
        dirs += ["\(home)/.local/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        dirs += (env["PATH"] ?? "").split(separator: ":").map(String.init)
        var seen = Set<String>()
        dirs = dirs.filter { !$0.isEmpty && seen.insert($0).inserted }
        env["PATH"] = dirs.joined(separator: ":")
        env["NO_COLOR"] = "1"
        return env
    }

    private static func loginShellPath(environment: [String: String]) async -> String? {
        let shell = environment["SHELL"] ?? "/bin/zsh"
        let text = await runQuickly(shell, ["-l", "-c", "printf '__MS_PATH__%s__MS_PATH__' \"$PATH\""],
                                    environment: environment) ?? ""
        let parts = text.components(separatedBy: "__MS_PATH__")
        return parts.count >= 3 ? parts[1] : nil
    }
}
