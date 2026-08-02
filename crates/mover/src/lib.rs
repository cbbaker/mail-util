//! `mover` — the abstraction that actually relocates messages, plus two non-network
//! implementations used before the real movers land:
//!
//! - [`DryRunMover`]: performs nothing; lets `apply --dry-run` exercise the full engine
//!   and journal without touching mail.
//! - [`FakeMover`]: an in-memory mailbox model used by the safety property tests to prove
//!   no-message-loss and crash/resume behavior.
//!
//! The real [`Mover`]s (server-side IMAP `UID MOVE`, and the offline local Maildir mover)
//! implement the same trait in later milestones.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use model::{FolderSpec, MoveAction};

/// What happened to a single message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOutcome {
    /// The message was relocated to its destination.
    Moved,
    /// The message was already at (or gone from) the source — nothing to do. This makes
    /// re-running a partially-applied plan idempotent.
    AlreadyDone,
    /// Dry run: the move was not performed, only simulated.
    Simulated,
}

/// Relocates messages and ensures destination folders exist. Implementations must be
/// safe to re-run: `move_message` on an already-moved message returns [`MoveOutcome::AlreadyDone`].
pub trait Mover {
    /// Ensure a destination folder exists (idempotent).
    fn ensure_folder(&mut self, spec: &FolderSpec) -> Result<()>;
    /// Move one message from its source to its destination.
    fn move_message(&mut self, action: &MoveAction) -> Result<MoveOutcome>;
    /// Reconcile after all moves (e.g. run mbsync, verify UIDVALIDITY). No-op for the
    /// non-network movers.
    fn reconcile(&mut self) -> Result<()>;
}

/// A mover that performs no side effects — used by `apply --dry-run`.
#[derive(Debug, Default)]
pub struct DryRunMover;

impl Mover for DryRunMover {
    fn ensure_folder(&mut self, _spec: &FolderSpec) -> Result<()> {
        Ok(())
    }
    fn move_message(&mut self, _action: &MoveAction) -> Result<MoveOutcome> {
        Ok(MoveOutcome::Simulated)
    }
    fn reconcile(&mut self) -> Result<()> {
        Ok(())
    }
}

/// One message in the [`FakeMover`] model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeMsg {
    pub message_id: Option<String>,
    pub uid: Option<u32>,
}

impl FakeMsg {
    pub fn new(message_id: Option<&str>, uid: Option<u32>) -> FakeMsg {
        FakeMsg {
            message_id: message_id.map(|s| s.to_string()),
            uid,
        }
    }

    /// Whether this message is the one an action refers to. Prefer Message-ID identity;
    /// fall back to UID when the message has no Message-ID.
    fn matches(&self, action: &MoveAction) -> bool {
        match (&self.message_id, &action.message_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.uid.is_some() && self.uid == action.uid,
        }
    }
}

/// An in-memory mailbox: folder dotpath -> messages. Moves remove from the source and
/// append to the destination, so message-count is conserved and each message lands in
/// exactly one folder — the invariants the real movers must also uphold.
#[derive(Debug, Clone)]
pub struct FakeMover {
    pub folders: BTreeMap<String, Vec<FakeMsg>>,
    pub created: Vec<String>,
    /// If set, `move_message` fails once this many moves have succeeded — used to
    /// simulate a crash mid-apply for resume tests.
    pub fail_after: Option<usize>,
    moves_done: usize,
    pub reconciled: bool,
}

impl FakeMover {
    pub fn new(folders: BTreeMap<String, Vec<FakeMsg>>) -> FakeMover {
        FakeMover {
            folders,
            created: Vec::new(),
            fail_after: None,
            moves_done: 0,
            reconciled: false,
        }
    }

    /// Fail after `n` successful moves (to simulate a crash).
    pub fn failing_after(mut self, n: usize) -> FakeMover {
        self.fail_after = Some(n);
        self
    }

    /// The multiset of Message-IDs across the whole mailbox, sorted — for conservation
    /// comparisons before/after an apply.
    pub fn message_id_multiset(&self) -> Vec<Option<String>> {
        let mut ids: Vec<Option<String>> = self
            .folders
            .values()
            .flat_map(|msgs| msgs.iter().map(|m| m.message_id.clone()))
            .collect();
        ids.sort();
        ids
    }

    /// Total messages across all folders.
    pub fn total_messages(&self) -> usize {
        self.folders.values().map(|v| v.len()).sum()
    }
}

impl Mover for FakeMover {
    fn ensure_folder(&mut self, spec: &FolderSpec) -> Result<()> {
        if !self.folders.contains_key(&spec.dotpath) {
            self.folders.insert(spec.dotpath.clone(), Vec::new());
            self.created.push(spec.dotpath.clone());
        }
        Ok(())
    }

    fn move_message(&mut self, action: &MoveAction) -> Result<MoveOutcome> {
        if let Some(fa) = self.fail_after {
            if self.moves_done >= fa {
                bail!("injected failure after {fa} moves");
            }
        }
        // Find the message in its source folder.
        let src = self.folders.entry(action.src_folder.clone()).or_default();
        let Some(pos) = src.iter().position(|m| m.matches(action)) else {
            // Not in source: already moved (idempotent) — leave everything untouched.
            return Ok(MoveOutcome::AlreadyDone);
        };
        let msg = src.remove(pos);
        self.folders
            .entry(action.dst_dotpath.clone())
            .or_default()
            .push(msg);
        self.moves_done += 1;
        Ok(MoveOutcome::Moved)
    }

    fn reconcile(&mut self) -> Result<()> {
        self.reconciled = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(mid: &str, uid: u32, src: &str, dst: &str) -> MoveAction {
        MoveAction {
            message_id: Some(mid.to_string()),
            src_folder: src.to_string(),
            uid: Some(uid),
            src_filename: format!("{uid}.host,U={uid}:2,S"),
            dst_dotpath: dst.to_string(),
            dst_imap: dst.trim_start_matches('.').replace("/.", "."),
            cluster_key: "k".to_string(),
        }
    }

    fn mailbox() -> BTreeMap<String, Vec<FakeMsg>> {
        BTreeMap::from([(
            ".INBOX".to_string(),
            vec![FakeMsg::new(Some("a"), Some(1)), FakeMsg::new(Some("b"), Some(2))],
        )])
    }

    #[test]
    fn move_relocates_and_conserves() {
        let mut fm = FakeMover::new(mailbox());
        let before = fm.message_id_multiset();
        fm.ensure_folder(&FolderSpec {
            dotpath: ".lists/.x".into(),
            imap_name: "lists.x".into(),
            sieve_target: "lists.x".into(),
            exists: false,
        })
        .unwrap();
        assert_eq!(fm.move_message(&action("a", 1, ".INBOX", ".lists/.x")).unwrap(), MoveOutcome::Moved);
        // Conserved and moved.
        assert_eq!(fm.message_id_multiset(), before);
        assert_eq!(fm.folders[".INBOX"].len(), 1);
        assert_eq!(fm.folders[".lists/.x"].len(), 1);
    }

    #[test]
    fn re_move_is_idempotent() {
        let mut fm = FakeMover::new(mailbox());
        let a = action("a", 1, ".INBOX", ".lists/.x");
        assert_eq!(fm.move_message(&a).unwrap(), MoveOutcome::Moved);
        assert_eq!(fm.move_message(&a).unwrap(), MoveOutcome::AlreadyDone);
        assert_eq!(fm.total_messages(), 2);
    }

    #[test]
    fn fail_after_injects_error() {
        let mut fm = FakeMover::new(mailbox()).failing_after(1);
        assert!(fm.move_message(&action("a", 1, ".INBOX", ".x")).is_ok());
        assert!(fm.move_message(&action("b", 2, ".INBOX", ".x")).is_err());
    }
}
