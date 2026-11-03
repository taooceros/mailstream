import Foundation

/// Build-time configuration injected into Info.plist by scripts/bundle.sh (never committed).
nonisolated struct AppConfig: Sendable {
    let googleClientID: String?, googleClientSecret: String?
    let microsoftClientID: String?, microsoftClientSecret: String?
    let pushURL: URL?, pushToken: String?, graphClientState: String?, gmailTopic: String?

    static let current = AppConfig(info: Bundle.main.infoDictionary ?? [:])

    /// Missing keys and empty strings read as nil.
    private init(info: [String: Any]) {
        func value(_ key: String) -> String? {
            guard let text = (info[key] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !text.isEmpty else { return nil }
            return text
        }
        googleClientID = value("MSGoogleClientID")
        googleClientSecret = value("MSGoogleClientSecret")
        microsoftClientID = value("MSMicrosoftClientID")
        microsoftClientSecret = value("MSMicrosoftClientSecret")
        pushURL = value("MSPushURL").flatMap(URL.init(string:))
        pushToken = value("MSPushToken")
        graphClientState = value("MSGraphClientState")
        gmailTopic = value("MSGmailTopic")
    }
}
