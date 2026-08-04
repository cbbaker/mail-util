//! `localmover` — the **offline** [`mover::Mover`]: relocate messages by manipulating the
//! local Maildir directly, then let `mbsync` propagate the change to the server.
//!
//! This is the opt-in alternative to the server-side IMAP mover; it works with no network
//! but is the sharp edge of the whole tool, because it must not trip mbsync's native-scheme
//! duplicate-UID corruption. The rules that keep it safe:
//!
//! 1. The destination file gets a **fresh, `,U=`-less** Maildir name (preserving the
//!    `:2,<flags>`). mbsync then treats it as a brand-new local message and assigns it a
//!    new server UID on push — a copied `,U=` would be a duplicate UID and force a
//!    UIDVALIDITY reset.
//! 2. Ordering is **write → fsync → verify bytes → delete source**, never delete first, so
//!    a crash at any point leaves the message in at least one place. Re-running is
//!    idempotent (a source that is already gone yields [`MoveOutcome::AlreadyDone`]).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use mailcache::MaildirName;
use model::{FolderSpec, MoveAction};
use mover::{MoveOutcome, Mover};

type Reconciler = Box<dyn FnMut(&BTreeSet<String>) -> Result<()>>;

pub struct LocalMover {
    root: PathBuf,
    host: String,
    seq: AtomicU64,
    touched: BTreeSet<String>,
    on_reconcile: Option<Reconciler>,
}

impl LocalMover {
    pub fn new(root: impl Into<PathBuf>) -> LocalMover {
        LocalMover {
            root: root.into(),
            host: format!("mailutil-{}", std::process::id()),
            seq: AtomicU64::new(0),
            touched: BTreeSet::new(),
            on_reconcile: None,
        }
    }

    /// Attach a reconcile callback (run after all moves with the touched dotpaths); the
    /// CLI wires this to run `mbsync` and assert UIDVALIDITY stability.
    pub fn with_reconciler(
        mut self,
        f: impl FnMut(&BTreeSet<String>) -> Result<()> + 'static,
    ) -> LocalMover {
        self.on_reconcile = Some(Box::new(f));
        self
    }

    pub fn touched(&self) -> &BTreeSet<String> {
        &self.touched
    }

    fn folder_dir(&self, dotpath: &str) -> PathBuf {
        self.root.join(dotpath)
    }

    /// Locate the source message file in the folder's `cur/` or `new/`.
    fn find_source(&self, folder: &str, filename: &str) -> Option<PathBuf> {
        for sub in ["cur", "new"] {
            let p = self.folder_dir(folder).join(sub).join(filename);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    /// A fresh Maildir-unique name for a moved message, deliberately without `,U=`.
    fn fresh_name(&self, flags: &str) -> String {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        MaildirName::new_local(now.as_secs(), now.subsec_micros(), seq, &self.host, flags)
            .to_filename()
    }

    fn ensure_maildir(dir: &std::path::Path) -> Result<()> {
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(dir.join(sub))
                .with_context(|| format!("creating {}", dir.join(sub).display()))?;
        }
        Ok(())
    }
}

impl Mover for LocalMover {
    fn ensure_folder(&mut self, spec: &FolderSpec) -> Result<()> {
        Self::ensure_maildir(&self.folder_dir(&spec.dotpath))?;
        self.touched.insert(spec.dotpath.clone());
        Ok(())
    }

    fn move_message(&mut self, action: &MoveAction) -> Result<MoveOutcome> {
        let Some(src_path) = self.find_source(&action.src_folder, &action.src_filename) else {
            // Already gone from the source — a prior run moved it. Idempotent no-op.
            return Ok(MoveOutcome::AlreadyDone);
        };
        let bytes = std::fs::read(&src_path)
            .with_context(|| format!("reading {}", src_path.display()))?;

        // Fresh, ,U=-less destination name preserving the source flags.
        let flags = MaildirName::parse(&action.src_filename).flags;
        let name = self.fresh_name(&flags);
        let dst_dir = self.folder_dir(&action.dst_dotpath);
        Self::ensure_maildir(&dst_dir)?;
        let tmp_path = dst_dir.join("tmp").join(&name);
        let cur_path = dst_dir.join("cur").join(&name);

        // Write to tmp, fsync, then atomically rename into cur (Maildir delivery).
        {
            let mut f = std::fs::File::create(&tmp_path)
                .with_context(|| format!("creating {}", tmp_path.display()))?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, &cur_path)
            .with_context(|| format!("renaming into {}", cur_path.display()))?;
        if let Ok(d) = std::fs::File::open(dst_dir.join("cur")) {
            let _ = d.sync_all(); // durably persist the rename
        }

        // Byte-verify the destination BEFORE removing the source.
        let written = std::fs::read(&cur_path)
            .with_context(|| format!("re-reading {}", cur_path.display()))?;
        if written != bytes {
            bail!("verification failed: destination bytes differ for {}", cur_path.display());
        }

        std::fs::remove_file(&src_path)
            .with_context(|| format!("removing source {}", src_path.display()))?;

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

#[cfg(test)]
mod tests;
