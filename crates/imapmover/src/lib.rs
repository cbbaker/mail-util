//! `imapmover` — the server-side [`mover::Mover`]: relocate messages by talking IMAP to
//! the server (the source of truth), then let `mbsync` bring the local cache in line.
//!
//! This is the **default, safest** mover: because moves happen on the server via
//! `UID MOVE` (RFC 6851; or a `UID COPY` + `\Deleted` + `UID EXPUNGE` fallback per
//! RFC 4315), we never fabricate local Maildir files and therefore never trip mbsync's
//! native-scheme duplicate-UID corruption.
//!
//! The low-level protocol is abstracted behind [`ImapOps`] so the move *logic* is tested
//! hermetically against [`FakeImapOps`] — no live server required. A real backend using
//! the `imap` crate implements the same trait.

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use model::FolderSpec;
use mover::{MoveOutcome, Mover};
use namemap::NameMap;

/// The minimal set of IMAP operations the mover needs. All mailbox names are full IMAP
/// names (already mapped from dotpaths via [`NameMap`]).
pub trait ImapOps {
    /// Whether the server advertised the `MOVE` capability (RFC 6851).
    fn has_move(&self) -> bool;
    /// Create a mailbox. Must treat "already exists" as success (idempotent).
    fn create_mailbox(&mut self, name: &str) -> Result<()>;
    /// Select a mailbox for subsequent UID operations.
    fn select(&mut self, mailbox: &str) -> Result<()>;
    /// Whether `uid` exists in the currently-selected mailbox.
    fn uid_exists(&mut self, uid: u32) -> Result<bool>;
    /// `UID MOVE uid dst` from the selected mailbox (RFC 6851).
    fn uid_move(&mut self, uid: u32, dst: &str) -> Result<()>;
    /// `UID COPY uid dst` from the selected mailbox.
    fn uid_copy(&mut self, uid: u32, dst: &str) -> Result<()>;
    /// `UID STORE uid +FLAGS (\Deleted)` in the selected mailbox.
    fn uid_store_deleted(&mut self, uid: u32) -> Result<()>;
    /// `UID EXPUNGE uid` in the selected mailbox (RFC 4315 — only the given uid).
    fn uid_expunge(&mut self, uid: u32) -> Result<()>;
}

type Reconciler = Box<dyn FnMut(&BTreeSet<String>) -> Result<()>>;

/// The server-side mover. Generic over [`ImapOps`] so tests inject a fake server.
pub struct ImapMover<O: ImapOps> {
    ops: O,
    nm: NameMap,
    /// Dotpaths of every folder touched (sources + destinations), for reconcile.
    touched: BTreeSet<String>,
    /// Optional callback run at `reconcile` time with the touched dotpaths — the CLI
    /// wires this to run `mbsync` and assert UIDVALIDITY stability. `None` in tests.
    on_reconcile: Option<Reconciler>,
}

impl<O: ImapOps> ImapMover<O> {
    /// Create a mover using `separator` (the probed IMAP hierarchy delimiter) for
    /// dotpath↔IMAP name mapping.
    pub fn new(ops: O, separator: char) -> ImapMover<O> {
        ImapMover {
            ops,
            nm: NameMap::new(separator),
            touched: BTreeSet::new(),
            on_reconcile: None,
        }
    }

    /// Attach a reconcile callback (run once after all moves, with the touched dotpaths).
    pub fn with_reconciler(
        mut self,
        f: impl FnMut(&BTreeSet<String>) -> Result<()> + 'static,
    ) -> ImapMover<O> {
        self.on_reconcile = Some(Box::new(f));
        self
    }

    /// Folders touched so far (for external inspection / reconcile).
    pub fn touched(&self) -> &BTreeSet<String> {
        &self.touched
    }
}

impl<O: ImapOps> Mover for ImapMover<O> {
    fn ensure_folder(&mut self, spec: &FolderSpec) -> Result<()> {
        let imap = self.nm.dotpath_to_imap(&spec.dotpath);
        self.ops.create_mailbox(&imap)?;
        self.touched.insert(spec.dotpath.clone());
        Ok(())
    }

    fn move_message(&mut self, action: &model::MoveAction) -> Result<MoveOutcome> {
        // A server-side move is keyed on the IMAP UID; a message not yet assigned one
        // (e.g. still in local `new/`) cannot be moved — stop rather than guess.
        let Some(uid) = action.uid else {
            bail!(
                "message {:?} in {} has no IMAP UID yet; run mbsync first, then re-apply",
                action.message_id,
                action.src_folder
            );
        };
        let src = self.nm.dotpath_to_imap(&action.src_folder);
        let dst = self.nm.dotpath_to_imap(&action.dst_dotpath);

        self.ops.select(&src)?;
        if !self.ops.uid_exists(uid)? {
            // Already gone from the source: a prior run moved it. Idempotent no-op.
            return Ok(MoveOutcome::AlreadyDone);
        }

        if self.ops.has_move() {
            self.ops.uid_move(uid, &dst)?;
        } else {
            // Fallback: copy, mark deleted, then expunge only this UID.
            self.ops.uid_copy(uid, &dst)?;
            self.ops.uid_store_deleted(uid)?;
            self.ops.uid_expunge(uid)?;
        }

        self.touched.insert(action.src_folder.clone());
        self.touched.insert(action.dst_dotpath.clone());
        Ok(MoveOutcome::Moved)
    }

    fn reconcile(&mut self) -> Result<()> {
        if let Some(f) = &mut self.on_reconcile {
            let touched = self.touched.clone();
            f(&touched)?;
        }
        Ok(())
    }
}

mod fake;
pub use fake::FakeImapOps;

pub mod netrc;

#[cfg(feature = "real-imap")]
mod real;
#[cfg(feature = "real-imap")]
pub use real::RealImapOps;

#[cfg(test)]
mod tests;
