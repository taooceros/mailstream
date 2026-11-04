import Foundation
import Synchronization

nonisolated struct ProcessOutput: Sendable {
    let status: Int32
    let stdout: Data
    /// Empty unless the run captured it.
    let stderr: Data
}

/// Runs a child process without parking a thread on it: the pipes are drained as async byte
/// streams (so none fills up and stalls the child) and exit arrives through the termination
/// handler. Returns once the child has exited and its pipes hit EOF. Cancellation or the timeout
/// ends the run at once: the child gets SIGTERM (SIGKILL if it outlives `killGrace`) and the pipes
/// are closed, so neither a child ignoring the signal nor a grandchild holding them keeps it waiting.
nonisolated enum ProcessRunner {
    struct TimedOut: LocalizedError {
        let name: String
        var errorDescription: String? { "\(name) timed out" }
    }

    private static let killGrace = Duration.seconds(2)

    static func run(
        executable: URL,
        arguments: [String],
        environment: [String: String],
        currentDirectory: URL? = nil,
        capturesStderr: Bool = false,
        timeout: Duration
    ) async throws -> ProcessOutput {
        try await withThrowingTaskGroup { group in
            group.addTask {
                try await launch(executable: executable, arguments: arguments, environment: environment,
                                 currentDirectory: currentDirectory, capturesStderr: capturesStderr)
            }
            group.addTask {
                try await Task.sleep(for: timeout)
                throw TimedOut(name: executable.lastPathComponent)
            }
            defer { group.cancelAll() }
            guard let output = try await group.next() else { throw CancellationError() }
            return output
        }
    }

    @concurrent
    private static func launch(
        executable: URL,
        arguments: [String],
        environment: [String: String],
        currentDirectory: URL?,
        capturesStderr: Bool
    ) async throws -> ProcessOutput {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        process.environment = environment
        if let currentDirectory { process.currentDirectoryURL = currentDirectory }
        let out = Pipe(), err = capturesStderr ? Pipe() : nil
        process.standardOutput = out
        process.standardError = err ?? FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        let (exits, exit) = AsyncStream.makeStream(of: Int32.self, bufferingPolicy: .bufferingNewest(1))
        process.terminationHandler = { process in
            exit.yield(process.terminationStatus)
            exit.finish()
        }
        // Shared with the cancellation handler, which may run on any thread; nil once cancelled.
        let child = Mutex<Process?>(process)
        return try await withTaskCancellationHandler {
            try child.withLock { process in
                guard let process else { throw CancellationError() }
                try process.run()
            }
            async let stdout = drain(out)
            async let stderr = drain(err)
            // Ends early when cancelled.
            var status: Int32?
            for await code in exits { status = code }
            let output: Data, errors: Data
            do {
                output = try await stdout
                errors = try await stderr
            } catch where Task.isCancelled {
                throw CancellationError()
            }
            guard let status, !Task.isCancelled else { throw CancellationError() }
            return ProcessOutput(status: status, stdout: output, stderr: errors)
        } onCancel: {
            if let process = child.withLock({ $0.take() }), process.isRunning {
                process.terminate()
                Task {
                    try? await Task.sleep(for: killGrace)
                    if process.isRunning { kill(process.processIdentifier, SIGKILL) }
                }
            }
            try? out.fileHandleForReading.close()
            try? err?.fileHandleForReading.close()
        }
    }

    private static func drain(_ pipe: Pipe?) async throws -> Data {
        guard let pipe else { return Data() }
        var data = Data()
        for try await byte in pipe.fileHandleForReading.bytes { data.append(byte) }
        return data
    }
}
