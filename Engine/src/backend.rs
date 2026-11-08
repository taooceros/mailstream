//! One signed-in account's mail server: Gmail or Microsoft Graph, as the Swift engine's
//! `MailBackend` (`Sources/MailstreamKit/Backend/MailBackend.swift`) reads them. Mailbox and
//! message ids are the provider's own and are what listings and mail.db store.
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::{ApiError, Priority};
use crate::gmail::{Gmail, GmailEndpoints};
use crate::graph::Graph;
use crate::model::{Address, Envelope, Mailbox, MessageContent};

/// Where the providers' REST APIs live; tests point them at a local server.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub gmail: GmailEndpoints,
    pub graph: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self { gmail: GmailEndpoints::default(), graph: crate::graph::ROOT.into() }
    }
}

/// One page of a mailbox, newest first.
#[derive(Clone, Debug)]
pub struct EnvelopePage {
    pub envelopes: Vec<Envelope>,
    /// The cursor of the next older page; nil at the end.
    pub next: Option<String>,
}

/// A search result: the message and the mailbox it lists under.
#[derive(Clone, Debug)]
pub struct SearchHit {
    pub mailbox: String,
    pub envelope: Envelope,
}

pub enum Backend {
    Gmail(Gmail),
    Graph(Graph),
}

impl Backend {
    /// The account's address and the name it sends as.
    pub async fn identity(&self) -> Result<Address, ApiError> {
        match self {
            Self::Gmail(gmail) => gmail.identity().await,
            Self::Graph(graph) => graph.identity().await,
        }
    }

    /// Every mailbox, roles resolved; `unread` and `total` only with `counts`.
    pub async fn mailboxes(&self, counts: bool) -> Result<Vec<Mailbox>, ApiError> {
        match self {
            Self::Gmail(gmail) => gmail.mailboxes(counts).await,
            Self::Graph(graph) => graph.mailboxes(counts).await,
        }
    }

    /// Newest first; `cursor` is nil for the first page, else the previous page's `next`.
    pub async fn envelopes(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        page_size: usize,
        priority: Priority,
    ) -> Result<EnvelopePage, ApiError> {
        match self {
            Self::Gmail(gmail) => gmail.envelopes(mailbox, cursor, page_size, priority).await,
            Self::Graph(graph) => graph.envelopes(mailbox, cursor, page_size, priority).await,
        }
    }

    /// Messages anywhere but spam and trash matching `query` in the provider's own syntax.
    pub async fn search(&self, query: &str, page_size: usize) -> Result<Vec<SearchHit>, ApiError> {
        match self {
            Self::Gmail(gmail) => gmail.search(query, page_size).await,
            Self::Graph(graph) => graph.search(query, page_size).await,
        }
    }

    /// The full message; parts it shows inline or offers as attachments are written into
    /// `directory`, and its HTML references them under `url_base`. Never marks it read.
    pub async fn content(
        &self,
        id: &str,
        directory: PathBuf,
        url_base: String,
        priority: Priority,
    ) -> Result<MessageContent, ApiError> {
        match self {
            Self::Gmail(gmail) => gmail.content(id, directory, url_base, priority).await,
            Self::Graph(graph) => graph.content(id, directory, url_base, priority).await,
        }
    }
}

/// The account's authorized requests: Gmail's carry its per-user quota budget, Graph's stay
/// within Outlook's four requests at a time.
pub fn api(account: &str, provider: crate::Provider, tokens: Arc<crate::tokens::Tokens>) -> crate::api::Api {
    match provider {
        crate::Provider::Google => crate::api::Api::gmail(account, tokens),
        crate::Provider::Microsoft => crate::api::Api::graph(account, tokens),
    }
}

/// A backend for `api`'s account on `provider`.
pub fn make(api: Arc<crate::api::Api>, provider: crate::Provider, endpoints: &Endpoints, cache_root: &std::path::Path) -> Backend {
    match provider {
        crate::Provider::Google => Backend::Gmail(Gmail::new(api, Arc::new(endpoints.gmail.clone()), cache_root)),
        crate::Provider::Microsoft => Backend::Graph(Graph::new(api, endpoints.graph.as_str().into())),
    }
}
