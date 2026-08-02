//! `mockcache` — a test-only builder that writes a **real** temp Maildir tree matching
//! mbsync's native scheme, so the rest of the code runs unmodified against it.
//!
//! Files are named `<secs>.<seq>_<uid>.mock,U=<uid>:2,<flags>` (matching the observed
//! `1726376999.528878_10.framework,U=10:2,S` form), each folder gets `cur/new/tmp` and a
//! `.uidvalidity`, and nested folders (`.lists/.elixir`) are real nested directories.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Fixed epoch used for all synthetic filenames (kept constant so tests are deterministic).
const MOCK_SECS: u64 = 1_700_000_000;

pub struct MockCache {
    _dir: TempDir,
    root: PathBuf,
    seq: Cell<u64>,
}

impl MockCache {
    /// Create a new temp cache whose account root is `<tmp>/maildir`.
    pub fn new() -> MockCache {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = dir.path().join("maildir");
        std::fs::create_dir_all(&root).expect("create account root");
        MockCache {
            _dir: dir,
            root,
            seq: Cell::new(0),
        }
    }

    /// Account root directory (pass to `mailcache::Account::new`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Ensure a folder exists (cur/new/tmp + a default `.uidvalidity`). Chainable.
    pub fn folder(&self, dotpath: &str) -> &MockCache {
        let dir = self.root.join(dotpath);
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).expect("create maildir subdir");
        }
        let uv = dir.join(".uidvalidity");
        if !uv.exists() {
            std::fs::write(&uv, format!("{MOCK_SECS}\n0\n")).expect("write uidvalidity");
        }
        self
    }

    /// Overwrite a folder's `.uidvalidity` (to test stability checks).
    pub fn set_uidvalidity(&self, dotpath: &str, validity: u64, max_uid: u32) {
        let uv = self.root.join(dotpath).join(".uidvalidity");
        std::fs::write(&uv, format!("{validity}\n{max_uid}\n")).expect("write uidvalidity");
    }

    /// Start building a message in `dotpath` (folder is auto-created).
    pub fn message<'a>(&'a self, dotpath: &str) -> MsgBuilder<'a> {
        self.folder(dotpath);
        MsgBuilder {
            cache: self,
            folder: dotpath.to_string(),
            uid: None,
            from: "someone@example.com".to_string(),
            from_name: None,
            list_id: None,
            subject: "test message".to_string(),
            message_id: None,
            extra_headers: Vec::new(),
            flags: "S".to_string(),
            is_new: false,
        }
    }

    fn next_seq(&self) -> u64 {
        let s = self.seq.get() + 1;
        self.seq.set(s);
        s
    }
}

impl Default for MockCache {
    fn default() -> Self {
        MockCache::new()
    }
}

pub struct MsgBuilder<'a> {
    cache: &'a MockCache,
    folder: String,
    uid: Option<u32>,
    from: String,
    from_name: Option<String>,
    list_id: Option<String>,
    subject: String,
    message_id: Option<String>,
    extra_headers: Vec<(String, String)>,
    flags: String,
    is_new: bool,
}

impl<'a> MsgBuilder<'a> {
    /// Set the mbsync native UID (embedded as `,U=<uid>`). Omit to simulate a fresh local file.
    pub fn uid(mut self, uid: u32) -> Self {
        self.uid = Some(uid);
        self
    }
    pub fn from(mut self, addr: &str) -> Self {
        self.from = addr.to_string();
        self
    }
    pub fn from_name(mut self, name: &str) -> Self {
        self.from_name = Some(name.to_string());
        self
    }
    pub fn list_id(mut self, id: &str) -> Self {
        self.list_id = Some(id.to_string());
        self
    }
    pub fn subject(mut self, s: &str) -> Self {
        self.subject = s.to_string();
        self
    }
    pub fn message_id(mut self, id: &str) -> Self {
        self.message_id = Some(id.to_string());
        self
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.extra_headers.push((name.to_string(), value.to_string()));
        self
    }
    pub fn flags(mut self, flags: &str) -> Self {
        self.flags = flags.to_string();
        self
    }
    pub fn in_new(mut self) -> Self {
        self.is_new = true;
        self
    }

    /// Write the message file; returns its filename.
    pub fn write(self) -> String {
        let seq = self.cache.next_seq();
        let mid = self
            .message_id
            .clone()
            .unwrap_or_else(|| format!("mock-{seq}@example.com"));

        let mut raw = String::new();
        if let Some(n) = &self.from_name {
            raw.push_str(&format!("From: {n} <{}>\r\n", self.from));
        } else {
            raw.push_str(&format!("From: {}\r\n", self.from));
        }
        raw.push_str(&format!("Subject: {}\r\n", self.subject));
        raw.push_str(&format!("Message-ID: <{mid}>\r\n"));
        if let Some(lid) = &self.list_id {
            raw.push_str(&format!("List-Id: <{lid}>\r\n"));
            raw.push_str("List-Unsubscribe: <mailto:unsub@example.com>\r\n");
        }
        for (k, v) in &self.extra_headers {
            raw.push_str(&format!("{k}: {v}\r\n"));
        }
        raw.push_str("\r\n");
        raw.push_str("This is a mock message body.\r\n");

        let filename = match self.uid {
            Some(uid) => format!("{MOCK_SECS}.{seq}_{uid}.mock,U={uid}:2,{}", self.flags),
            None => format!("{MOCK_SECS}.{seq}_{seq}.mock:2,{}", self.flags),
        };
        let sub = if self.is_new { "new" } else { "cur" };
        let path = self.cache.root.join(&self.folder).join(sub).join(&filename);
        std::fs::write(&path, raw).expect("write mock message");
        filename
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_native_filenames() {
        let mc = MockCache::new();
        let fname = mc
            .message(".INBOX")
            .uid(10)
            .from("a@b.com")
            .list_id("x.list")
            .flags("S")
            .write();
        assert!(fname.contains(",U=10:2,S"), "got {fname}");
        assert!(mc.root().join(".INBOX/cur").join(&fname).exists());
        assert!(mc.root().join(".INBOX/.uidvalidity").exists());
    }

    #[test]
    fn fresh_local_message_has_no_uid_segment() {
        let mc = MockCache::new();
        let fname = mc.message(".INBOX").from("a@b.com").write();
        assert!(!fname.contains(",U="), "got {fname}");
    }
}
