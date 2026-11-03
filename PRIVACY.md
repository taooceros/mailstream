# Privacy Policy

Mailstream is a personal, open-source mail client for macOS that runs on your Mac.

- **Sign-in:** you sign in to Google or Microsoft in your browser; the provider returns an authorization to Mailstream on your Mac. Refresh tokens are stored in the macOS Keychain and are sent only to the provider that issued them.
- **Mail** is fetched directly from Google (Gmail API) or Microsoft (Microsoft Graph) to your Mac, and messages you write are sent the same way. Mail content is not sent to any other server.
- **Push notifications:** to learn about new mail without polling, Mailstream asks Gmail and Microsoft Graph to notify a relay (a Cloudflare Worker deployed by whoever builds the app) and keeps one connection to it. A notification carries only identifiers: the account's address and a Gmail history id, or a Graph subscription id and the kind of change. It carries no subjects, senders or message content. The relay forwards it to the connected app and stores nothing.
- **Contacts:** with your permission, Mailstream reads the photos and email addresses of your Google contacts (People API, read-only) and Outlook contacts (Microsoft Graph, read-only) so it can show sender avatars. It does not modify your contacts.
- **Google Chat:** with your permission, Mailstream reads your Google Chat spaces, their members, messages and your read state, and sends the messages you write and marks spaces read, through the Google Chat API, directly between Google and your Mac. It does not delete or edit messages or change spaces.
- **Avatars:** to find logos and avatars, Mailstream can look up the sender's domain (BIMI DNS record and the site's icon) and request Gravatar using a hash of the sender's address.
- **AI tagging (optional):** when enabled, a conversation's sender, subject and preview are sent to the model provider you configured in the `omp` CLI.
- **Storage:** everything Mailstream fetches is cached only on your Mac.
- **No sharing:** Mailstream has no analytics or tracking, and shares no data with third parties beyond the requests described above.

Questions: open an issue at https://github.com/taooceros/mailstream.
