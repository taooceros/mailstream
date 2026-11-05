import AppKit

/// The Mailstream executable's entry: the `mailstream` CLI when the arguments name a command,
/// else the AppKit app.
public enum MailstreamApp {
    public static func main() -> Never {
        let arguments = Array(CommandLine.arguments.dropFirst())
        if CLI.wants(arguments) { exit(CLI.run(arguments)) }

        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        app.setActivationPolicy(.regular)
        app.run()
        exit(0)
    }
}
