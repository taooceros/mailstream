//! Changing and sending Gmail mail, as the Swift engine's `GmailBackend` does: flags and moves are
//! label changes (`batchModify`, `trash`/`untrash`), deletion is `batchDelete`, and sending goes
//! through the media upload endpoint, which takes messages up to 35 MB with their thread. Each
//! change is applied to the envelope cache ahead of the history reporting it. AI tags synced
//! (`tagsync`) are user labels found by name in any case, made when missing, never deleted.
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{ALL_MAIL, EnvelopeCache, Gmail, GmailEndpoints, Label};
use crate::api::{self, ApiError, Priority, Request};
use crate::backend::{Flag, SentCopy};
use crate::mime::{self, OutgoingMessage};

/// Labels Gmail keeps to itself: a move never takes them off.
const FIXED: [&str; 3] = [ALL_MAIL, "SENT", "DRAFT"];

impl GmailEndpoints {
    /// Gmail's media upload root, beside the REST root: `…/upload/gmail/v1/users/me/`.
    fn upload_url(&self, path: &str, query: &[(&str, &str)]) -> String {
        api::url(&self.root.replacen("/gmail/v1/", "/upload/gmail/v1/", 1), path, query)
    }
}

impl Gmail {
    pub async fn set_flag(&self, flag: Flag, on: bool, ids: &[String]) -> Result<(), ApiError> {
        let label = if flag == Flag::Seen { "UNREAD" } else { "STARRED" };
        // Seen is the absence of UNREAD.
        let add = (flag == Flag::Seen) != on;
        let (added, removed): (&[&str], &[&str]) = if add { (&[label], &[]) } else { (&[], &[label]) };
        self.modify(ids, added, removed).await
    }

    /// Moving to All Mail archives; to or from the trash goes through `trash`/`untrash`.
    pub async fn move_to(&self, ids: &[String], from: &str, to: &str) -> Result<(), ApiError> {
        if from == to || ids.is_empty() {
            return Ok(());
        }
        if to == "TRASH" {
            self.each(ids, "trash").await?;
            self.cache.relabel(ids, &["TRASH"], &[]).await;
            return Ok(());
        }
        if from == "TRASH" {
            // Untrash restores the labels the message had; then file it where asked, unless that
            // is a label Gmail keeps to itself (Sent, Drafts, All Mail), which untrash restored.
            self.each(ids, "untrash").await?;
            self.cache.relabel(ids, &[], &["TRASH"]).await;
            if !FIXED.contains(&to) {
                let removed: &[&str] = if to == "SPAM" { &["INBOX"] } else { &[] };
                self.modify(ids, &[to], removed).await?;
            }
            return Ok(());
        }
        // Leaving STARRED or IMPORTANT unstars or unmarks: the message leaves that mailbox.
        let mut removed: BTreeSet<&str> = BTreeSet::new();
        if !FIXED.contains(&from) {
            removed.insert(from);
        }
        if to == "SPAM" {
            removed.insert("INBOX");
        }
        let added: &[&str] = if to == ALL_MAIL { &[] } else { &[to] };
        self.modify(ids, added, &removed.into_iter().collect::<Vec<_>>()).await
    }

    /// Deletes for good; only called for messages already in the trash.
    pub async fn delete(&self, ids: &[String]) -> Result<(), ApiError> {
        for chunk in ids.chunks(1000) {
            let body = serde_json::to_vec(&serde_json::json!({ "ids": chunk })).expect("ids encode");
            let request = Request::post(self.endpoints.url("messages/batchDelete", &[]), "application/json", body);
            self.api.send(request, Priority::Interactive, Some(true), 50).await?;
        }
        self.cache.forget(ids).await;
        Ok(())
    }

    /// Sends and files the copy in Sent, in the replied-to message's thread; answers the copy's id
    /// and thread. Never repeated.
    pub async fn send(&self, message: &OutgoingMessage) -> Result<SentCopy, ApiError> {
        #[derive(Serialize)]
        struct Metadata<'a> {
            #[serde(rename = "threadId", skip_serializing_if = "Option::is_none")]
            thread_id: Option<&'a str>,
        }
        let raw = mime::render(message).map_err(|message| ApiError::new(0, message))?;
        let boundary = mime::boundary("ms_upload_", true);
        let mut body = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n").into_bytes();
        body.extend(serde_json::to_vec(&Metadata { thread_id: message.thread.as_deref() }).expect("metadata encodes"));
        body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: message/rfc822\r\n\r\n").as_bytes());
        body.extend(raw);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = self.endpoints.upload_url("messages/send", &[("uploadType", "multipart")]);
        let request = Request::post(url, &format!("multipart/related; boundary={boundary}"), body);
        let response = self.api.send(request, Priority::Interactive, Some(false), 100).await?;
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Answer {
            id: Option<String>,
            thread_id: Option<String>,
        }
        // Sent is sent: an answer that doesn't read only leaves the copy unnamed.
        let answer: Answer = serde_json::from_slice(&response.body).unwrap_or_default();
        Ok(SentCopy { id: answer.id, thread: answer.thread_id })
    }

    /// Puts the user labels named `add` on the messages and takes those named `remove` off,
    /// matching names in any case. A label `add` names that the mailbox lacks is made, shown in
    /// Gmail's label list and on its messages, with the labels it nests under (`Mailstream` for
    /// `Mailstream/Finance`: Gmail nests a label only under one that exists); no label is ever
    /// deleted.
    pub async fn set_tags(&self, ids: &[String], add: &[String], remove: &[String]) -> Result<(), ApiError> {
        fn find(labels: &[Label], name: &str) -> Option<String> {
            let name = name.to_lowercase();
            labels
                .iter()
                .find(|label| label.kind.as_deref() == Some("user") && label.name.to_lowercase() == name)
                .map(|label| label.id.clone())
        }
        let mut labels = self.cache.labels(Priority::Interactive).await?;
        let mut made = false;
        let mut add_ids: Vec<String> = Vec::new();
        for name in add {
            if find(&labels, name).is_none() {
                let parents = name.match_indices('/').map(|(end, _)| &name[..end]);
                for missing in parents.filter(|parent| find(&labels, parent).is_none()).chain([name.as_str()]) {
                    self.create_label(missing).await?;
                }
                labels = self.cache.labels(Priority::Interactive).await?;
                made = true;
            }
            let id = find(&labels, name)
                .ok_or_else(|| ApiError::new(0, format!("Gmail did not make the label \"{name}\"")))?;
            add_ids.push(id);
        }
        let remove_ids: Vec<String> =
            remove.iter().filter_map(|name| find(&labels, name)).filter(|id| !add_ids.contains(id)).collect();
        let add: Vec<&str> = add_ids.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove_ids.iter().map(String::as_str).collect();
        self.modify(ids, &add, &remove).await?;
        if made {
            // The new labels' names, for the envelopes that now carry them.
            self.cache.labels(Priority::Interactive).await?;
        }
        Ok(())
    }

    /// Makes a user label; one another client made meanwhile (409) is fine.
    async fn create_label(&self, name: &str) -> Result<(), ApiError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "name": name, "labelListVisibility": "labelShow", "messageListVisibility": "show",
        }))
        .expect("label encodes");
        let request = Request::post(self.endpoints.url("labels", &[]), "application/json", body);
        match self.api.send(request, Priority::Interactive, Some(true), 5).await {
            Ok(_) => Ok(()),
            Err(error) if error.status == 409 => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn modify(&self, ids: &[String], add: &[&str], remove: &[&str]) -> Result<(), ApiError> {
        if ids.is_empty() || (add.is_empty() && remove.is_empty()) {
            return Ok(());
        }
        for chunk in ids.chunks(1000) {
            let body = serde_json::to_vec(&serde_json::json!({ "ids": chunk, "addLabelIds": add, "removeLabelIds": remove }))
                .expect("labels encode");
            let request = Request::post(self.endpoints.url("messages/batchModify", &[]), "application/json", body);
            self.api.send(request, Priority::Interactive, Some(true), 50).await?;
        }
        self.cache.relabel(ids, add, remove).await;
        Ok(())
    }

    /// `messages/{id}/<action>` for every id, in parallel.
    async fn each(&self, ids: &[String], action: &str) -> Result<(), ApiError> {
        let requests = ids.iter().map(|id| {
            let url = self.endpoints.url(&format!("messages/{}/{action}", api::percent_encoded(id)), &[]);
            let request = Request { method: reqwest::Method::POST, url, headers: Vec::new(), body: None };
            self.api.send(request, Priority::Interactive, Some(true), 5)
        });
        futures::future::try_join_all(requests).await?;
        Ok(())
    }
}

impl EnvelopeCache {
    /// Our own label change, ahead of the history reporting it.
    async fn relabel(self: &Arc<Self>, ids: &[String], add: &[&str], remove: &[&str]) {
        let mut state = self.state.lock().await;
        self.load(&mut state);
        for id in ids {
            if let Some(entry) = state.entries.get_mut(id) {
                entry.labels.extend(add.iter().map(|label| label.to_string()));
                for label in remove {
                    entry.labels.remove(*label);
                }
            }
        }
        state.advance(Some(ids.iter().cloned().collect::<HashSet<_>>()));
    }

    async fn forget(self: &Arc<Self>, ids: &[String]) {
        let mut state = self.state.lock().await;
        self.load(&mut state);
        for id in ids {
            state.entries.remove(id);
        }
        state.advance(Some(ids.iter().cloned().collect::<HashSet<_>>()));
    }
}
