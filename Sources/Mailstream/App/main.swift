import AppKit

let arguments = Array(CommandLine.arguments.dropFirst())
if CLI.wants(arguments) { exit(CLI.run(arguments)) }

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
