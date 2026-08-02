//! Walking a mbsync-managed Maildir account: enumerating folders and streaming the
//! header-level [`Message`] records the suggestion engine consumes.

use std::io::Read;
use std::path::{Path, PathBuf};

use model::Message;
use walkdir::WalkDir;

use crate::filename::MaildirName;
use crate::header;

/// Read at most this many bytes when hunting for the header/body boundary. Headers
/// (even with heavy DKIM/ARC) virtually never exceed this; a message with a larger
/// header block still parses from the truncated prefix.
const HEADER_READ_CAP: usize = 256 * 1024;

/// A folder within an account, identified by its dotpath (filesystem path relative to
/// the account root, e.g. ".INBOX" or ".lists/.elixir").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRef {
    pub dotpath: String,
    pub dir: PathBuf,
}

/// The root of one mbsync account (e.g. `~/Maildir/<account>`).
#[derive(Debug, Clone)]
pub struct Account {
    pub root: PathBuf,
}

impl Account {
    pub fn new(root: impl Into<PathBuf>) -> Account {
        Account { root: root.into() }
    }

    /// Enumerate every Maildir folder under the account root. A directory is a folder
    /// iff it contains a `cur/` subdirectory (the Maildir marker). Results are sorted
    /// by dotpath for deterministic output.
    pub fn folders(&self) -> Vec<FolderRef> {
        let mut out = Vec::new();
        for entry in WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }
            if entry.file_name() == "cur" || entry.file_name() == "new" || entry.file_name() == "tmp" {
                continue; // skip the Maildir sub-parts themselves
            }
            if !entry.path().join("cur").is_dir() {
                continue;
            }
            if let Some(dotpath) = self.dotpath_of(entry.path()) {
                out.push(FolderRef {
                    dotpath,
                    dir: entry.path().to_path_buf(),
                });
            }
        }
        out.sort_by(|a, b| a.dotpath.cmp(&b.dotpath));
        out
    }

    /// Compute the dotpath of a folder directory relative to the account root.
    /// Returns `None` for the root itself.
    fn dotpath_of(&self, dir: &Path) -> Option<String> {
        let rel = dir.strip_prefix(&self.root).ok()?;
        let s = rel.to_string_lossy();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    /// Find a folder by dotpath.
    pub fn folder(&self, dotpath: &str) -> FolderRef {
        FolderRef {
            dotpath: dotpath.to_string(),
            dir: self.root.join(dotpath),
        }
    }
}

impl FolderRef {
    /// Number of messages in this folder (files in `cur/` + `new/`).
    pub fn message_count(&self) -> usize {
        ["cur", "new"]
            .iter()
            .map(|sub| count_files(&self.dir.join(sub)))
            .sum()
    }

    /// Stream the messages in this folder as header-level [`Message`] records.
    /// IO errors on individual files are skipped (logged by the caller if desired) so a
    /// single unreadable file never aborts a full-cache scan.
    pub fn messages(&self) -> Vec<Message> {
        let mut out = Vec::new();
        for (sub, is_new) in [("cur", false), ("new", true)] {
            let subdir = self.dir.join(sub);
            let rd = match std::fs::read_dir(&subdir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for de in rd.filter_map(|e| e.ok()) {
                if !de.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let filename = de.file_name().to_string_lossy().to_string();
                if filename.starts_with('.') {
                    continue; // skip dotfiles like .uidvalidity should never be here, but be safe
                }
                let parsed = MaildirName::parse(&filename);
                let raw = match read_header_prefix(&de.path()) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let (headers, message_id) = header::parse(&raw);
                out.push(Message {
                    folder: self.dotpath.clone(),
                    uid: parsed.uid,
                    filename,
                    is_new,
                    message_id,
                    headers,
                });
            }
        }
        // Deterministic ordering by (uid, filename) for stable output.
        out.sort_by(|a, b| (a.uid, &a.filename).cmp(&(b.uid, &b.filename)));
        out
    }
}

fn count_files(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .count(),
        Err(_) => 0,
    }
}

/// Read up to the header/body boundary (capped) so we never load large bodies.
fn read_header_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let n = f.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        // Stop once we have the blank line separating headers from body.
        if header::header_block(&buf).len() < buf.len() {
            break;
        }
        if buf.len() >= HEADER_READ_CAP {
            break;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_msg(dir: &Path, sub: &str, name: &str, body: &str) {
        let d = dir.join(sub);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(name), body).unwrap();
    }

    #[test]
    fn scans_folder_messages_and_uids() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let inbox = root.join(".INBOX");
        fs::create_dir_all(inbox.join("tmp")).unwrap();
        write_msg(
            &inbox,
            "cur",
            "1.host,U=10:2,S",
            "From: a@b.com\nList-Id: <x.list>\n\nbody",
        );
        write_msg(&inbox, "new", "2.host,U=11:2,", "From: c@d.com\n\nbody");

        let acct = Account::new(root);
        let folders = acct.folders();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].dotpath, ".INBOX");
        assert_eq!(folders[0].message_count(), 2);

        let msgs = folders[0].messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].uid, Some(10));
        assert_eq!(msgs[0].headers.list_id.as_deref(), Some("x.list"));
        assert_eq!(msgs[1].uid, Some(11));
        assert!(msgs[1].is_new);
    }

    #[test]
    fn nested_folder_dotpath() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let elixir = root.join(".lists").join(".elixir");
        for s in ["cur", "new", "tmp"] {
            fs::create_dir_all(elixir.join(s)).unwrap();
        }
        // .lists itself is also a folder container with cur/
        fs::create_dir_all(root.join(".lists").join("cur")).unwrap();

        let acct = Account::new(root);
        let dotpaths: Vec<_> = acct.folders().into_iter().map(|f| f.dotpath).collect();
        assert!(dotpaths.contains(&".lists".to_string()));
        assert!(dotpaths.contains(&".lists/.elixir".to_string()));
    }
}
