//! `mailcache` — read-only access to an mbsync-managed Maildir cache.
//!
//! Modules:
//! - [`filename`]: parse/emit mbsync native-scheme filenames (`,U=<uid>:2,<flags>`).
//! - [`uidvalidity`]: read per-folder `.uidvalidity` state.
//! - [`header`]: streaming extraction of clustering-relevant headers.
//! - [`scan`]: enumerate folders and stream [`model::Message`] records.

pub mod filename;
pub mod header;
pub mod scan;
pub mod uidvalidity;

pub use filename::MaildirName;
pub use scan::{Account, FolderRef};
pub use uidvalidity::UidValidity;
