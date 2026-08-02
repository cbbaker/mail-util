//! Shared data types for mail-util, serialized across the CLI <-> Emacs JSON boundary.
//!
//! These types are intentionally IO-free so every other crate can depend on them
//! without pulling in filesystem or network code.

use serde::{Deserialize, Serialize};

/// A message's stable identity plus the header fields the suggestion engine needs.
///
/// We deliberately never carry the body here — the scanner streams headers only so it
/// can walk 100k+ messages cheaply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Maildir++ dotpath of the folder the message currently lives in, e.g. ".INBOX".
    pub folder: String,
    /// mbsync native-scheme UID embedded in the filename (`,U=<uid>`), if present.
    pub uid: Option<u32>,
    /// The on-disk filename (within the folder's `cur/` or `new/`).
    pub filename: String,
    /// Whether the file is in `new/` (true) or `cur/` (false).
    pub is_new: bool,
    /// RFC5322 Message-ID with angle brackets stripped, lowercased. `None` if absent.
    pub message_id: Option<String>,
    /// Envelope-ish header fields used for clustering.
    pub headers: MessageHeaders,
}

/// The subset of headers we extract for clustering. All values are trimmed; addresses
/// are lowercased. Missing headers are `None` / empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHeaders {
    /// `List-Id` / `List-ID`, normalized: angle brackets stripped, lowercased.
    pub list_id: Option<String>,
    /// Bare `From` address (`local@domain`), lowercased.
    pub from_addr: Option<String>,
    /// `From` display name, if any.
    pub from_name: Option<String>,
    /// Recipient addresses from To/Cc/Delivered-To, lowercased, deduplicated.
    pub recipients: Vec<String>,
    /// `Subject`, raw-decoded.
    pub subject: Option<String>,
    /// Whether a `List-Unsubscribe` / `List-Post` header was present (list corroboration).
    pub is_list_mail: bool,
}

impl MessageHeaders {
    /// Registrable-ish domain of the sender: everything after the last `@`, lowercased.
    /// (A full public-suffix reduction is a later refinement; the raw domain is the key.)
    pub fn from_domain(&self) -> Option<&str> {
        self.from_addr.as_deref().and_then(|a| a.rsplit('@').next())
    }
}

/// How a cluster maps to a destination folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    /// A folder that already exists in the cache (reuse existing taxonomy).
    Existing { dotpath: String },
    /// A new folder to be created, described by its proposed slug/dotpath.
    New { dotpath: String },
}

impl Destination {
    pub fn dotpath(&self) -> &str {
        match self {
            Destination::Existing { dotpath } | Destination::New { dotpath } => dotpath,
        }
    }
}

/// The signal that produced a cluster, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    ListId,
    SenderDomain,
    PersonAddress,
}

impl Signal {
    /// Base weight used in scoring; List-Id is the most reliable sorting signal.
    pub fn strength(self) -> f64 {
        match self {
            Signal::ListId => 1.0,
            Signal::PersonAddress => 0.7,
            Signal::SenderDomain => 0.6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// A proposed grouping of inbox messages that should be filed together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cluster {
    /// The matched key (the List-Id value, sender domain, or person address).
    pub key: String,
    pub signal: Signal,
    pub destination: Destination,
    /// Number of inbox messages in this cluster.
    pub count: usize,
    pub score: f64,
    pub confidence: Confidence,
    /// A few representative senders (deduplicated), for the review UI.
    pub sample_senders: Vec<String>,
    /// A few representative subjects, for the review UI.
    pub sample_subjects: Vec<String>,
    /// UIDs of the messages in this cluster (source folder is always the scanned inbox).
    pub uids: Vec<u32>,
}

/// Tunable knobs for the suggestion engine and the "conservation universe".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Minimum messages for a cluster to be surfaced.
    pub min_count: usize,
    /// Minimum score for a cluster to be surfaced.
    pub min_score: f64,
    /// Folders excluded from source scanning and from message-conservation math
    /// (deleting mail there is legitimate, so it must not count as "lost").
    pub excluded_folders: Vec<String>,
    /// Domains treated as work mail (routed toward `.work` rather than `.vendors`).
    pub work_domains: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            min_count: 5,
            min_score: 0.0,
            // Generic Maildir++ special-folder names (case variants included). Deleting
            // mail in these is legitimate, so they are excluded both as sort sources and
            // from message-conservation math. Override in config for other layouts.
            excluded_folders: [
                ".Trash", ".trash", ".Junk", ".junk", ".Spam", ".spam",
                ".Deleted Messages", ".Drafts", ".drafts",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            work_domains: Vec::new(),
        }
    }
}
