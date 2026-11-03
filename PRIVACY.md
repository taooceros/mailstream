# Privacy Policy

Mailstream is a personal, open-source mail client for macOS that runs entirely on your Mac.

- **Mail** is fetched directly from your mail provider (Gmail, Outlook, IMAP) to your Mac. It is not sent to any other server.
- **Google contacts:** with your permission, Mailstream reads the photos and email addresses of your Google contacts (People API, read-only) so it can show sender avatars. It does not modify your contacts.
- **Google Chat:** with your permission, Mailstream reads your Google Chat spaces, their members, messages and your read state, and sends the messages you write and marks spaces read, through the Google Chat API, directly between Google and your Mac. It does not delete or edit messages or change spaces.
- **Avatars:** to find logos and avatars, Mailstream can look up the sender's domain (BIMI DNS record and the site's icon) and request Gravatar using a hash of the sender's address.
- **Storage:** everything Mailstream fetches is cached only on your Mac. OAuth tokens are stored in the macOS Keychain.
- **No sharing:** Mailstream has no servers, analytics or tracking, and shares no data with third parties beyond the requests described above.

Questions: open an issue at https://github.com/taooceros/mailstream.
