import AppKit
import SwiftUI

let arguments = Array(CommandLine.arguments.dropFirst())
if CLI.wants(arguments) { exit(CLI.run(arguments)) }

MailstreamApp.main()
