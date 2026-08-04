//! An in-memory IMAP server for hermetic tests of the move logic.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

use crate::ImapOps;

/// A fake IMAP server: mailbox name -> set of present UIDs, plus per-mailbox `\Deleted`
/// marks. Records an operation log for assertions.
#[derive(Debug, Clone)]
pub struct FakeImapOps {
    pub mailboxes: BTreeMap<String, BTreeSet<u32>>,
    pub deleted: BTreeMap<String, BTreeSet<u32>>,
    pub supports_move: bool,
    pub delimiter: char,
    pub created: Vec<String>,
    pub op_log: Vec<String>,
    selected: Option<String>,
    next_uid: u32,
}

impl FakeImapOps {
    /// Build a server from an initial `mailbox -> uids` map.
    pub fn new(mailboxes: BTreeMap<String, Vec<u32>>, supports_move: bool) -> FakeImapOps {
        let max = mailboxes.values().flatten().copied().max().unwrap_or(0);
        FakeImapOps {
            mailboxes: mailboxes
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect(),
            deleted: BTreeMap::new(),
            supports_move,
            delimiter: '.',
            created: Vec::new(),
            op_log: Vec::new(),
            selected: None,
            next_uid: max + 1000,
        }
    }

    fn sel(&self) -> Result<String> {
        self.selected
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no mailbox selected"))
    }

    /// UIDs currently in a mailbox — for assertions.
    pub fn uids(&self, mailbox: &str) -> Vec<u32> {
        self.mailboxes
            .get(mailbox)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
}

impl ImapOps for FakeImapOps {
    fn has_move(&self) -> bool {
        self.supports_move
    }

    fn create_mailbox(&mut self, name: &str) -> Result<()> {
        self.op_log.push(format!("CREATE {name}"));
        self.mailboxes.entry(name.to_string()).or_default();
        self.created.push(name.to_string());
        Ok(())
    }

    fn select(&mut self, mailbox: &str) -> Result<()> {
        if !self.mailboxes.contains_key(mailbox) {
            bail!("SELECT of nonexistent mailbox {mailbox}");
        }
        self.op_log.push(format!("SELECT {mailbox}"));
        self.selected = Some(mailbox.to_string());
        Ok(())
    }

    fn uid_exists(&mut self, uid: u32) -> Result<bool> {
        let sel = self.sel()?;
        Ok(self.mailboxes.get(&sel).is_some_and(|s| s.contains(&uid)))
    }

    fn uid_move(&mut self, uid: u32, dst: &str) -> Result<()> {
        let sel = self.sel()?;
        self.op_log.push(format!("UID MOVE {uid} {dst}"));
        let removed = self.mailboxes.get_mut(&sel).is_some_and(|s| s.remove(&uid));
        if !removed {
            bail!("UID MOVE of absent uid {uid} from {sel}");
        }
        // The server assigns a fresh UID in the destination.
        let new = self.next_uid;
        self.next_uid += 1;
        self.mailboxes.entry(dst.to_string()).or_default().insert(new);
        Ok(())
    }

    fn uid_copy(&mut self, uid: u32, dst: &str) -> Result<()> {
        let sel = self.sel()?;
        self.op_log.push(format!("UID COPY {uid} {dst}"));
        if !self.mailboxes.get(&sel).is_some_and(|s| s.contains(&uid)) {
            bail!("UID COPY of absent uid {uid} from {sel}");
        }
        let new = self.next_uid;
        self.next_uid += 1;
        self.mailboxes.entry(dst.to_string()).or_default().insert(new);
        Ok(())
    }

    fn uid_store_deleted(&mut self, uid: u32) -> Result<()> {
        let sel = self.sel()?;
        self.op_log.push(format!("STORE {uid} +FLAGS \\Deleted"));
        self.deleted.entry(sel).or_default().insert(uid);
        Ok(())
    }

    fn uid_expunge(&mut self, uid: u32) -> Result<()> {
        let sel = self.sel()?;
        self.op_log.push(format!("UID EXPUNGE {uid}"));
        let is_deleted = self.deleted.get(&sel).is_some_and(|s| s.contains(&uid));
        if !is_deleted {
            bail!("UID EXPUNGE of uid {uid} not marked \\Deleted");
        }
        self.mailboxes.get_mut(&sel).map(|s| s.remove(&uid));
        self.deleted.get_mut(&sel).map(|s| s.remove(&uid));
        Ok(())
    }
}
