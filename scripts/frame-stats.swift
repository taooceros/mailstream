// Summarizes frame logs the front ends write in bench mode (scripts/bench.sh), the same way for
// every front end. A log is "# key=value …" header lines, then one line per frame:
// "<start ms>\t<work ms>", where start is when the frame began for its vsync and work is the
// longest single-thread stage of the frame (see AGENTS.md, "Performance: 120 fps").
// Usage: swift scripts/frame-stats.swift [--hz 120] LOG…
import Foundation

var hz = 120.0
var paths: [String] = []
var args = CommandLine.arguments.dropFirst()
while let arg = args.popFirst() {
    if arg == "--hz", let value = args.popFirst().flatMap(Double.init) {
        hz = value
    } else {
        paths.append(arg)
    }
}
guard !paths.isEmpty else {
    FileHandle.standardError.write(Data("usage: swift scripts/frame-stats.swift [--hz 120] LOG…\n".utf8))
    exit(2)
}
let period = 1000 / hz

/// Nearest-rank percentile of sorted values.
func percentile(_ sorted: [Double], _ p: Double) -> Double {
    sorted[min(sorted.count - 1, max(0, Int((p / 100 * Double(sorted.count)).rounded(.up)) - 1))]
}

func ms(_ value: Double) -> String { String(format: "%.1f", value) }

var failed = false
for path in paths {
    guard let text = try? String(contentsOfFile: path, encoding: .utf8) else {
        print("\(path): unreadable")
        failed = true
        continue
    }
    var header: [String] = []
    var starts: [Double] = []
    var work: [Double] = []
    for line in text.split(separator: "\n") {
        if line.hasPrefix("#") {
            header.append(line.dropFirst().trimmingCharacters(in: .whitespaces))
            continue
        }
        let fields = line.split(separator: "\t").compactMap { Double($0) }
        guard fields.count == 2 else { continue }
        starts.append(fields[0])
        work.append(fields[1])
    }
    guard starts.count > 100 else {
        print("\(path): only \(starts.count) frames")
        failed = true
        continue
    }
    let intervals = zip(starts.dropFirst(), starts).map { $0 - $1 }.sorted()
    // Each interval spans a whole number of vsyncs; every one past the first is a frame not shown.
    let vsyncs = intervals.map { max(1, ($0 / period).rounded()) }
    let dropped = vsyncs.reduce(0) { $0 + $1 - 1 }
    let droppedShare = dropped / vsyncs.reduce(0, +) * 100
    let fps = Double(intervals.count) / (starts.last! - starts.first!) * 1000
    let p99 = percentile(intervals, 99)
    let sortedWork = work.sorted()
    let pass = droppedShare <= 1 && p99 <= period * 1.5
    failed = failed || !pass
    print(
        """
        \(path) [\(header.joined(separator: " "))]
          \(starts.count) frames, \(String(format: "%.1f", fps)) fps, dropped \(Int(dropped)) vsyncs \
        (\(String(format: "%.2f", droppedShare))%)
          interval p50 \(ms(percentile(intervals, 50))) p95 \(ms(percentile(intervals, 95))) \
        p99 \(ms(p99)) max \(ms(intervals.last!)) ms
          work p50 \(ms(percentile(sortedWork, 50))) p95 \(ms(percentile(sortedWork, 95))) \
        p99 \(ms(percentile(sortedWork, 99))) max \(ms(sortedWork.last!)) ms, \
        \(work.filter { $0 > period }.count) frames over \(ms(period)) ms
          \(pass ? "PASS" : "FAIL") at \(Int(hz)) Hz (needs dropped ≤ 1% and interval p99 ≤ \(ms(period * 1.5)) ms)
        """)
}
exit(failed ? 1 : 0)
