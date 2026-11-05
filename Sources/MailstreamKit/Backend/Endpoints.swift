import Foundation

/// The provider API roots requests are built from, so the backends, push registrations and
/// contact photos name each host once.
nonisolated enum Endpoints {
    static let gmail = URL(string: "https://gmail.googleapis.com/gmail/v1/users/me/")!
    /// Gmail's media upload root, which takes messages up to 35 MB.
    static let gmailUpload = URL(string: "https://gmail.googleapis.com/upload/gmail/v1/users/me/")!
    /// Gmail's batch endpoint; its sub-requests name paths under `gmail`.
    static let gmailBatch = URL(string: "https://gmail.googleapis.com/batch/gmail/v1")!
    static let graph = URL(string: "https://graph.microsoft.com/v1.0/")!
    static let chat = URL(string: "https://chat.googleapis.com/v1/")!
    static let people = URL(string: "https://people.googleapis.com/v1/")!
}

/// OAuth scopes: what sign-in asks for, and what features look for in an account's grant.
nonisolated enum OAuthScope {
    static let gmail = "https://mail.google.com/"
    static let googleContacts = "https://www.googleapis.com/auth/contacts.readonly"
    static let googleOtherContacts = "https://www.googleapis.com/auth/contacts.other.readonly"
    static let googleDirectory = "https://www.googleapis.com/auth/directory.readonly"
    static let chatSpaces = "https://www.googleapis.com/auth/chat.spaces.readonly"
    /// Starting direct messages and group chats (`spaces.setup`).
    static let chatSpacesCreate = "https://www.googleapis.com/auth/chat.spaces.create"
    /// The one that makes a Google account a chat account.
    static let chatMessages = "https://www.googleapis.com/auth/chat.messages.readonly"
    static let chatCreate = "https://www.googleapis.com/auth/chat.messages.create"
    static let chatMemberships = "https://www.googleapis.com/auth/chat.memberships.readonly"
    static let chatReadState = "https://www.googleapis.com/auth/chat.users.readstate"
    static let graphUser = "https://graph.microsoft.com/User.Read"
    static let graphMail = "https://graph.microsoft.com/Mail.ReadWrite"
    static let graphSend = "https://graph.microsoft.com/Mail.Send"
    static let graphContacts = "https://graph.microsoft.com/Contacts.Read"
}
