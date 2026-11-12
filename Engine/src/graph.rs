//! A Microsoft account's mail over Microsoft Graph, read as the Swift engine's `GraphBackend`
//! reads it (`Sources/MailstreamKit/Backend/GraphBackend.swift`). Every request asks for
//! immutable ids, so a message keeps its id across moves; folder ids are mailbox ids.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use crate::api::{self, Api, ApiError, GraphPage, Priority, Request};
use crate::assembly::{AttachedMessage, MessageAssembly};
use crate::backend::{EnvelopePage, SearchHit};
use crate::model::{Address, DISTANT_PAST, Envelope, Mailbox, MailboxRole, MessageContent, parse_iso8601};
use crate::text;

mod changes;

pub const ROOT: &str = "https://graph.microsoft.com/v1.0/";

const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";
const HTML_BODY: &str = "outlook.body-content-type=\"html\"";
const ENVELOPE_FIELDS: &str =
    "id,conversationId,internetMessageId,subject,from,toRecipients,receivedDateTime,isRead,flag,isDraft,hasAttachments,categories";
const FOLDER_FIELDS: &str = "id,displayName,parentFolderId,childFolderCount,unreadItemCount,totalItemCount";
const ROLE_FOLDERS: [(&str, MailboxRole); 6] = [
    ("inbox", MailboxRole::Inbox),
    ("sentitems", MailboxRole::Sent),
    ("drafts", MailboxRole::Drafts),
    ("deleteditems", MailboxRole::Trash),
    ("junkemail", MailboxRole::Junk),
    ("archive", MailboxRole::Archive),
];
/// Exchange's own folders, left out of the sidebar with everything under them.
const SYSTEM_FOLDERS: [&str; 3] = ["conversationhistory", "outbox", "syncissues"];

pub struct Graph {
    api: Arc<Api>,
    root: Arc<str>,
    /// Well-known folder name → id. Found ids are kept for the backend's life (Graph's folder ids
    /// never change); missing names, which the mailbox may gain, are asked again.
    well_known: Mutex<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct Folder {
    id: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "childFolderCount")]
    child_folder_count: Option<i64>,
    #[serde(rename = "unreadItemCount")]
    unread_item_count: Option<i64>,
    #[serde(rename = "totalItemCount")]
    total_item_count: Option<i64>,
}

#[derive(Clone, Deserialize)]
struct Recipient {
    #[serde(rename = "emailAddress")]
    email_address: Option<EmailAddress>,
}

#[derive(Clone, Deserialize)]
struct EmailAddress {
    name: Option<String>,
    address: Option<String>,
}

impl Recipient {
    fn address(&self) -> Option<Address> {
        let email = self.email_address.as_ref()?;
        let address = email.address.as_deref().filter(|address| !address.is_empty())?;
        Some(Address::new(email.name.clone(), address))
    }
}

fn addresses(recipients: Option<Vec<Recipient>>) -> Vec<Address> {
    recipients.unwrap_or_default().iter().filter_map(Recipient::address).collect()
}

#[derive(Deserialize)]
struct Message {
    id: String,
    #[serde(rename = "conversationId")]
    conversation_id: Option<String>,
    #[serde(rename = "internetMessageId")]
    internet_message_id: Option<String>,
    subject: Option<String>,
    from: Option<Recipient>,
    #[serde(rename = "toRecipients")]
    to_recipients: Option<Vec<Recipient>>,
    #[serde(rename = "receivedDateTime")]
    received_date_time: Option<String>,
    #[serde(rename = "isRead")]
    is_read: Option<bool>,
    flag: Option<Flag>,
    #[serde(rename = "isDraft")]
    is_draft: Option<bool>,
    #[serde(rename = "hasAttachments")]
    has_attachments: Option<bool>,
    categories: Option<Vec<String>>,
    /// Only selected by search.
    #[serde(rename = "parentFolderId")]
    parent_folder_id: Option<String>,
}

#[derive(Deserialize)]
struct Flag {
    #[serde(rename = "flagStatus")]
    flag_status: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Body {
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    content: Option<String>,
}

/// `body` as an HTML body or a plain one, by its content type; absent is an empty HTML one.
fn split(body: Option<&Body>) -> (Option<String>, Option<String>) {
    let content = body.and_then(|body| body.content.clone()).unwrap_or_default();
    if body.and_then(|body| body.content_type.as_deref()).map(str::to_lowercase).as_deref() == Some("text") {
        (None, Some(content))
    } else {
        (Some(content), None)
    }
}

#[derive(Deserialize)]
struct Full {
    /// `#microsoft.graph.eventMessageRequest` and the like for meeting messages.
    #[serde(rename = "@odata.type")]
    kind: Option<String>,
    body: Option<Body>,
    #[serde(rename = "ccRecipients")]
    cc_recipients: Option<Vec<Recipient>>,
    #[serde(rename = "replyTo")]
    reply_to: Option<Vec<Recipient>>,
    #[serde(rename = "internetMessageHeaders")]
    internet_message_headers: Option<Vec<HeaderField>>,
    #[serde(rename = "hasAttachments")]
    has_attachments: Option<bool>,
}

#[derive(Deserialize)]
struct HeaderField {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct AttachmentItem {
    #[serde(rename = "@odata.type")]
    kind: Option<String>,
    id: String,
    name: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    #[serde(rename = "contentId")]
    content_id: Option<String>,
    #[serde(rename = "isInline")]
    is_inline: Option<bool>,
    #[serde(rename = "contentBytes")]
    content_bytes: Option<String>,
}

#[derive(Deserialize)]
struct AttachedItem {
    item: Option<ItemBody>,
}

#[derive(Deserialize)]
struct ItemBody {
    subject: Option<String>,
    from: Option<Recipient>,
    body: Option<Body>,
}

/// An attachment ready for `MessageAssembly`, or an attached message rendered inline.
enum Part {
    File { name: Option<String>, mime: String, content_id: Option<String>, inline: bool, bytes: Vec<u8> },
    Message(AttachedMessage),
}

fn envelope(message: Message) -> Envelope {
    Envelope {
        id: message.id,
        thread: message.conversation_id,
        message_id: message.internet_message_id.as_deref().map(text::normalize_message_id).filter(|id| !id.is_empty()),
        in_reply_to: vec![],
        flags: Envelope::flags(
            message.is_read == Some(true),
            message.flag.and_then(|flag| flag.flag_status).as_deref() == Some("flagged"),
            message.is_draft == Some(true),
        ),
        subject: message.subject.unwrap_or_default(),
        from: message.from.and_then(|from| from.address()).into_iter().collect(),
        to: addresses(message.to_recipients),
        date: message.received_date_time.as_deref().and_then(parse_iso8601),
        size: 0,
        has_attachment: message.has_attachments,
        tags: message.categories.unwrap_or_default().into_iter().collect(),
    }
}

impl Graph {
    pub fn new(api: Arc<Api>, root: Arc<str>) -> Self {
        Self { api, root, well_known: Mutex::new(HashMap::new()) }
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> String {
        api::url(&self.root, path, query)
    }

    /// A Graph request that asks for immutable ids, plus any other `prefer` preference.
    fn request(url: String, prefer: Option<&str>) -> Request {
        let preference = match prefer {
            Some(prefer) => format!("{IMMUTABLE_IDS}, {prefer}"),
            None => IMMUTABLE_IDS.to_string(),
        };
        Request::get(url).header("Prefer", preference)
    }

    async fn decode<T: DeserializeOwned>(&self, url: String, prefer: Option<&str>, priority: Priority) -> Result<T, ApiError> {
        self.api.decode(Self::request(url, prefer), priority, 0).await
    }

    pub async fn identity(&self) -> Result<Address, ApiError> {
        #[derive(Deserialize)]
        struct User {
            #[serde(rename = "displayName")]
            display_name: Option<String>,
            mail: Option<String>,
            #[serde(rename = "userPrincipalName")]
            user_principal_name: Option<String>,
        }
        let user: User =
            self.decode(self.url("me", &[("$select", "displayName,mail,userPrincipalName")]), None, Priority::Interactive).await?;
        let email = [user.mail, user.user_principal_name]
            .into_iter()
            .flatten()
            .find(|email| !email.is_empty())
            .ok_or_else(|| ApiError::malformed("Microsoft Graph returned no address for this account"))?;
        Ok(Address::new(user.display_name, email))
    }

    pub async fn mailboxes(&self, counts: bool) -> Result<Vec<Mailbox>, ApiError> {
        let top_url = self.url("me/mailFolders", &[("$top", "100"), ("$select", FOLDER_FIELDS)]);
        let (known, top) = tokio::join!(self.well_known_ids(), self.pages::<Folder>(top_url, Priority::Interactive));
        let (well_known, top) = (known?, top?);
        let hidden: HashSet<String> = SYSTEM_FOLDERS.iter().filter_map(|name| well_known.get(*name).cloned()).collect();
        let roles: HashMap<String, MailboxRole> =
            ROLE_FOLDERS.iter().filter_map(|(name, role)| Some((well_known.get(*name)?.clone(), *role))).collect();
        let folders = self.expand(top, None, Arc::new(hidden)).await?;
        Ok(folders
            .into_iter()
            .map(|(folder, name)| Mailbox {
                account: self.api.account.clone(),
                role: roles.get(&folder.id).copied().unwrap_or(MailboxRole::Other),
                id: folder.id,
                name,
                unread: if counts { folder.unread_item_count } else { None },
                total: if counts { folder.total_item_count } else { None },
            })
            .collect())
    }

    async fn well_known_ids(&self) -> Result<HashMap<String, String>, ApiError> {
        let mut known = self.well_known.lock().await;
        let wanted: Vec<&str> = ROLE_FOLDERS
            .iter()
            .map(|(name, _)| *name)
            .chain(SYSTEM_FOLDERS)
            .filter(|name| !known.contains_key(*name))
            .collect();
        if wanted.is_empty() {
            return Ok(known.clone());
        }
        #[derive(Deserialize)]
        struct FolderId {
            id: String,
        }
        let lookups = wanted.into_iter().map(|name| async move {
            let url = self.url(&format!("me/mailFolders/{name}"), &[("$select", "id")]);
            match self.decode::<FolderId>(url, None, Priority::Interactive).await {
                Ok(folder) => Ok((name, Some(folder.id))),
                Err(error) if error.status == 404 => Ok((name, None)),
                Err(error) => Err(error),
            }
        });
        for (name, id) in futures::future::try_join_all(lookups).await? {
            if let Some(id) = id {
                known.entry(name.to_string()).or_insert(id);
            }
        }
        Ok(known.clone())
    }

    /// `folders` and all their descendants except `hidden` ones, each with its "Parent/Child" name.
    fn expand<'a>(
        &'a self,
        folders: Vec<Folder>,
        path: Option<String>,
        hidden: Arc<HashSet<String>>,
    ) -> BoxFuture<'a, Result<Vec<(Folder, String)>, ApiError>> {
        async move {
            let mut found = Vec::new();
            let mut children = Vec::new();
            for folder in folders {
                if hidden.contains(&folder.id) {
                    continue;
                }
                let display = folder.display_name.clone().unwrap_or_default();
                let name = path.as_ref().map_or(display.clone(), |path| format!("{path}/{display}"));
                if folder.child_folder_count.unwrap_or(0) > 0 {
                    let url = self.url(
                        &format!("me/mailFolders/{}/childFolders", api::percent_encoded(&folder.id)),
                        &[("$top", "100"), ("$select", FOLDER_FIELDS)],
                    );
                    let (hidden, name) = (hidden.clone(), name.clone());
                    children.push(async move {
                        let nested = self.pages::<Folder>(url, Priority::Interactive).await?;
                        self.expand(nested, Some(name), hidden).await
                    });
                }
                found.push((folder, name));
            }
            for nested in futures::future::try_join_all(children).await? {
                found.extend(nested);
            }
            Ok(found)
        }
        .boxed()
    }

    /// Pages through `first` and every `@odata.nextLink` after it.
    async fn pages<Item: DeserializeOwned>(&self, first: String, priority: Priority) -> Result<Vec<Item>, ApiError> {
        let found = api::pages(
            first,
            usize::MAX,
            |url| async move { self.decode::<GraphPage<Item>>(url, None, priority).await.map(Some) },
        )
        .await?;
        Ok(found.unwrap_or_default())
    }

    pub async fn envelopes(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        page_size: usize,
        priority: Priority,
    ) -> Result<EnvelopePage, ApiError> {
        let url = match cursor {
            Some(cursor) => {
                reqwest::Url::parse(cursor).map_err(|_| ApiError::new(0, "Invalid page cursor"))?;
                cursor.to_string()
            }
            None => self.url(
                &format!("me/mailFolders/{}/messages", api::percent_encoded(mailbox)),
                &[("$top", &page_size.to_string()), ("$orderby", "receivedDateTime desc"), ("$select", ENVELOPE_FIELDS)],
            ),
        };
        let page: GraphPage<Message> = self.decode(url, None, priority).await?;
        Ok(EnvelopePage { envelopes: page.value.into_iter().map(envelope).collect(), next: page.next })
    }

    /// `/me/messages` spans every folder; each hit keeps the folder it is in.
    pub async fn search(&self, query: &str, page_size: usize) -> Result<Vec<SearchHit>, ApiError> {
        let terms = query.replace('"', "");
        let terms = terms.trim();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let select = format!("{ENVELOPE_FIELDS},parentFolderId");
        let url = self.url(
            "me/messages",
            &[("$search", &format!("\"{terms}\"")), ("$top", &page_size.to_string()), ("$select", &select)],
        );
        let page: GraphPage<Message> = self.decode(url, None, Priority::Interactive).await?;
        let mut hits: Vec<SearchHit> = page
            .value
            .into_iter()
            .filter_map(|mut message| {
                let mailbox = message.parent_folder_id.take()?;
                Some(SearchHit { mailbox, envelope: envelope(message) })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.envelope.date.unwrap_or(DISTANT_PAST).total_cmp(&a.envelope.date.unwrap_or(DISTANT_PAST))
        });
        Ok(hits)
    }

    pub async fn content(
        &self,
        id: &str,
        directory: PathBuf,
        url_base: String,
        priority: Priority,
    ) -> Result<MessageContent, ApiError> {
        let message_path = format!("me/messages/{}", api::percent_encoded(id));
        let full: Full = self
            .decode(
                self.url(&message_path, &[("$select", "body,ccRecipients,replyTo,internetMessageHeaders,hasAttachments")]),
                Some(HTML_BODY),
                priority,
            )
            .await?;
        let (mut html, mut text) = split(full.body.as_ref());
        let mut parts: Vec<(usize, Part)> = Vec::new();
        let mentions_cid = html.as_deref().is_some_and(|html| html.to_lowercase().contains("cid:"));
        if full.has_attachments == Some(true) || mentions_cid {
            let items =
                self.pages::<AttachmentItem>(self.url(&format!("{message_path}/attachments"), &[]), priority).await?;
            let fetches = items.into_iter().enumerate().map(|(index, item)| {
                let message_path = &message_path;
                async move { Ok::<_, ApiError>(self.part(item, message_path, priority).await?.map(|part| (index, part))) }
            });
            parts = futures::future::try_join_all(fetches).await?.into_iter().flatten().collect();
            parts.sort_by_key(|(index, _)| *index);
        }
        let mut assembly =
            MessageAssembly::new(directory, url_base).map_err(|error| ApiError::new(0, error.to_string()))?;
        for (index, part) in parts {
            match part {
                Part::File { name, mime, content_id, inline, bytes } => {
                    assembly.store(&bytes, index as i64, name.as_deref(), &mime, content_id.as_deref(), inline);
                }
                Part::Message(attached) => {
                    // A plain message with an HTML one attached is shown as HTML, its own text included.
                    if html.is_none() && attached.has_html {
                        html = Some(text::plain_to_html(text.as_deref().unwrap_or("")));
                    }
                    if let Some(outer) = &mut html {
                        outer.push_str(&attached.html);
                    }
                    if let Some(outer) = &mut text {
                        outer.push_str("\n\n");
                        outer.push_str(&attached.text);
                    }
                }
            }
        }
        let references = text::parse_message_id_list(
            full.internet_message_headers
                .iter()
                .flatten()
                .find(|header| header.name.eq_ignore_ascii_case("References"))
                .map(|header| header.value.as_str()),
        );
        let mut content = assembly.finish(html, text, addresses(full.cc_recipients), addresses(full.reply_to), references);
        // Outlook's meeting requests carry no `.ics`: the event they name, read with the message
        // (without immutable ids, so the id is the one the calendar lists).
        let meeting = full.kind.as_deref().is_some_and(|kind| kind.contains("eventMessage") && !kind.contains("Response"));
        if meeting {
            let url = self.url(&message_path, &[("$expand", "microsoft.graph.eventMessage/event")]);
            let request = Request::get(url).header("Prefer", "outlook.timezone=\"UTC\"");
            let message = self.api.decode::<serde_json::Value>(request, priority, 0).await;
            if let Some(invitation) = message.ok().as_ref().and_then(crate::calendar_api::graph_invitation) {
                content.invitation = Some(invitation);
            }
        }
        Ok(content)
    }

    /// A file's bytes or an attached message's rendering; nil for kinds not shown (cloud links).
    async fn part(&self, item: AttachmentItem, message_path: &str, priority: Priority) -> Result<Option<Part>, ApiError> {
        let path = format!("{message_path}/attachments/{}", api::percent_encoded(&item.id));
        match item.kind.as_deref() {
            Some("#microsoft.graph.fileAttachment") => {
                let bytes = match item.content_bytes.as_deref().and_then(text::base64_url_decode) {
                    Some(bytes) => bytes,
                    None => {
                        self.api.send(Self::request(self.url(&format!("{path}/$value"), &[]), None), priority, None, 0).await?.body
                    }
                };
                let mime = item.content_type.as_deref().and_then(|kind| kind.split(';').next()).unwrap_or("").trim().to_string();
                Ok(Some(Part::File {
                    name: item.name,
                    mime,
                    content_id: item.content_id,
                    inline: item.is_inline.unwrap_or(false),
                    bytes,
                }))
            }
            Some("#microsoft.graph.itemAttachment") => {
                let expanded: AttachedItem = self
                    .decode(self.url(&path, &[("$expand", "microsoft.graph.itemattachment/item")]), Some(HTML_BODY), priority)
                    .await?;
                let item = expanded.item;
                let (html, text) = split(item.as_ref().and_then(|item| item.body.as_ref()));
                let text = text.or_else(|| html.as_deref().map(text::strip_html)).unwrap_or_default();
                let from = item.as_ref().and_then(|item| item.from.as_ref()).and_then(Recipient::address);
                let subject = item.as_ref().and_then(|item| item.subject.clone()).unwrap_or_default();
                Ok(Some(Part::Message(AttachedMessage::new(from.as_ref(), &subject, html.as_deref(), &text))))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_messages_into_envelopes() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "id": "AAk=", "conversationId": "c1", "internetMessageId": "<ID@Host>", "subject": "Hi",
            "from": { "emailAddress": { "name": "Ann", "address": "ann@x.com" } },
            "toRecipients": [{ "emailAddress": { "address": "" } }, { "emailAddress": { "address": "me@x.com" } }],
            "receivedDateTime": "2026-11-01T10:00:00Z", "isRead": false, "flag": { "flagStatus": "flagged" },
            "isDraft": false, "hasAttachments": true,
        }))
        .unwrap();
        let envelope = envelope(message);
        assert_eq!(envelope.message_id.as_deref(), Some("id@host"));
        assert_eq!(envelope.to, vec![Address::new(None, "me@x.com")]);
        assert!(envelope.is_flagged() && !envelope.is_seen());
        assert_eq!(envelope.date.map(crate::model::iso8601).as_deref(), Some("2026-11-01T10:00:00Z"));
        assert_eq!(split(None), (Some(String::new()), None));
    }
}
