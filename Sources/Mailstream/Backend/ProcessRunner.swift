import Foundation
import Synchronization

nonisolated struct ProcessOutput: Sendable {
    let status: Int32
    let stdout: Data
    let stderr: Data
}

/// Runs a child process without parking a thread on it: both pipes are drained as async byte
/// streams (so neither fills up and stalls the child) and exit arrives through the termination
/// handler. Returns once the child has exited and both pipes hit EOF; cancelling the calling
/// task terminates the child.
nonisolated enum ProcessRunner {
    @concurrent
    static func run(
        executable: URL,
        arguments: [String],
        environment: [String: String],
        currentDirectory: URL? = nil
    ) async throws -> ProcessOutput {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        process.environment = environment
        if let currentDirectory { process.currentDirectoryURL = currentDirectory }
        let out = Pipe(), err = Pipe()
        process.standardOutput = out
        process.standardError = err
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
            async let stdout = drain(out.fileHandleForReading)
            async let stderr = drain(err.fileHandleForReading)
            var status: Int32?
            for await code in exits { status = code }
            let output = try await (stdout, stderr)
            guard let status, !Task.isCancelled else { throw CancellationError() }
            return ProcessOutput(status: status, stdout: output.0, stderr: output.1)
        } onCancel: {
            child.withLock { process in
                if let process, process.isRunning { process.terminate() }
                process = nil
            }
        }
    }

    private static func drain(_ file: FileHandle) async throws -> Data {
        var data = Data()
        for try await byte in file.bytes { data.append(byte) }
        return data
    }
}
