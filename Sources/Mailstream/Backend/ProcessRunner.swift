import Foundation

struct ProcessOutput: Sendable {
    let status: Int32
    let stdout: Data
    let stderr: Data
}

/// Runs a child process without parking any thread on it: both pipes are drained by readability
/// handlers (so neither fills up and stalls the child) and exit arrives through the termination
/// handler; the caller resumes once the process has exited and both pipes hit EOF. Task
/// cancellation terminates the child.
enum ProcessRunner {
    static func run(
        executable: URL,
        arguments: [String],
        environment: [String: String],
        stdin: Data? = nil,
        currentDirectory: URL? = nil
    ) async throws -> ProcessOutput {
        let handle = ProcessHandle()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<ProcessOutput, Error>) in
                let process = Process()
                process.executableURL = executable
                process.arguments = arguments
                process.environment = environment
                if let currentDirectory { process.currentDirectoryURL = currentDirectory }
                let out = Pipe(), err = Pipe(), input = Pipe()
                process.standardOutput = out
                process.standardError = err
                process.standardInput = stdin == nil ? FileHandle.nullDevice : input
                let run = ProcessRun(handle: handle, continuation: continuation)
                out.fileHandleForReading.readabilityHandler = { file in run.received(file.availableData, stdout: true, from: file) }
                err.fileHandleForReading.readabilityHandler = { file in run.received(file.availableData, stdout: false, from: file) }
                process.terminationHandler = { process in run.exited(process.terminationStatus) }
                do {
                    try handle.start(process)
                } catch {
                    out.fileHandleForReading.readabilityHandler = nil
                    err.fileHandleForReading.readabilityHandler = nil
                    run.failed(error)
                    return
                }
                if let stdin {
                    let writer = input.fileHandleForWriting
                    DispatchQueue.global(qos: .userInitiated).async {
                        try? writer.write(contentsOf: stdin)
                        try? writer.close()
                    }
                }
            }
        } onCancel: {
            handle.cancel()
        }
    }
}

/// Collects one child's output and resumes its caller exactly once.
private final class ProcessRun: @unchecked Sendable {
    private let lock = NSLock()
    private let handle: ProcessHandle
    private var continuation: CheckedContinuation<ProcessOutput, Error>?
    private var stdout = Data()
    private var stderr = Data()
    private var openPipes = 2
    private var status: Int32?

    init(handle: ProcessHandle, continuation: CheckedContinuation<ProcessOutput, Error>) {
        self.handle = handle
        self.continuation = continuation
    }

    func received(_ data: Data, stdout isStdout: Bool, from file: FileHandle) {
        lock.withLock {
            if data.isEmpty {
                file.readabilityHandler = nil
                openPipes -= 1
            } else if isStdout {
                stdout.append(data)
            } else {
                stderr.append(data)
            }
        }
        finishIfDone()
    }

    func exited(_ status: Int32) {
        lock.withLock { self.status = status }
        finishIfDone()
    }

    func failed(_ error: Error) {
        let continuation = lock.withLock { () -> CheckedContinuation<ProcessOutput, Error>? in
            defer { self.continuation = nil }
            return self.continuation
        }
        continuation?.resume(throwing: error)
    }

    private func finishIfDone() {
        let done = lock.withLock { () -> (CheckedContinuation<ProcessOutput, Error>, ProcessOutput)? in
            guard openPipes == 0, let status, let continuation else { return nil }
            self.continuation = nil
            return (continuation, ProcessOutput(status: status, stdout: stdout, stderr: stderr))
        }
        guard let (continuation, output) = done else { return }
        if handle.wasCancelled {
            continuation.resume(throwing: CancellationError())
        } else {
            continuation.resume(returning: output)
        }
    }
}

/// Lock-protected process reference shared between the runner and the cancellation handler.
private final class ProcessHandle: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?
    private var cancelled = false

    var wasCancelled: Bool { lock.withLock { cancelled } }

    func start(_ process: Process) throws {
        try lock.withLock {
            if cancelled { throw CancellationError() }
            try process.run()
            self.process = process
        }
    }

    func cancel() {
        lock.withLock {
            cancelled = true
            if let process, process.isRunning { process.terminate() }
        }
    }
}

/// FIFO counting semaphore for bounding concurrent child processes.
actor AsyncSemaphore {
    private var available: Int
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(_ count: Int) { available = count }

    func acquire() async {
        if available > 0 {
            available -= 1
            return
        }
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        if waiters.isEmpty {
            available += 1
        } else {
            waiters.removeFirst().resume()
        }
    }

    func withPermit<T: Sendable>(_ body: @Sendable () async throws -> T) async throws -> T {
        await acquire()
        do {
            let value = try await body()
            release()
            return value
        } catch {
            release()
            throw error
        }
    }
}
