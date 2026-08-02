//! Parsing and construction of mbsync *native-scheme* Maildir filenames.
//!
//! A native-scheme filename looks like:
//!
//! ```text
//! 1726376999.528878_10.framework,U=10:2,S
//! └──────── unique base ────────┘└ U ┘└flags┘
//! ```
//!
//! The `,U=<uid>` segment is how mbsync binds the local file to a server IMAP UID.
//! Duplicating it into another folder is exactly what corrupts sync (duplicate UID →
//! UIDVALIDITY reset). So when we *create* a file for a moved message we must emit a
//! fresh unique base and **omit** the `,U=` segment (see [`MaildirName::new_local`]).
//!
//! The Maildir info section is `:2,<flags>` where flags are single letters kept in
//! ASCII order (e.g. `S` seen, `R` replied, `F` flagged, `P` passed, `D` draft, `T` trashed).

/// A parsed Maildir filename, split into its three logical parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaildirName {
    /// The unique base (everything before `,U=` or the info separator `:`).
    pub base: String,
    /// The mbsync native UID, if the `,U=<n>` segment was present.
    pub uid: Option<u32>,
    /// The info flags after `:2,` (e.g. "S", "PS"). Empty if no/blank info section.
    pub flags: String,
}

impl MaildirName {
    /// Parse a filename as written on disk. This never fails: anything unrecognized is
    /// treated as a bare base with no UID and no flags, so odd files are still tracked
    /// (and therefore counted for message-conservation) rather than silently dropped.
    pub fn parse(name: &str) -> MaildirName {
        // Split off the info section at the FIRST ':' — the unique/UID part may not
        // contain a colon, but flags never do, so the first colon is unambiguous.
        let (left, info) = match name.split_once(':') {
            Some((l, i)) => (l, Some(i)),
            None => (name, None),
        };

        // The UID segment is the trailing ",U=<digits>" of the left part.
        let (base, uid) = match left.rfind(",U=") {
            Some(pos) => {
                let digits = &left[pos + 3..];
                match digits.parse::<u32>() {
                    Ok(u) => (left[..pos].to_string(), Some(u)),
                    // ",U=" present but not followed by a clean number: keep it in base.
                    Err(_) => (left.to_string(), None),
                }
            }
            None => (left.to_string(), None),
        };

        // Info section is "2,<flags>"; tolerate a missing/short version.
        let flags = match info {
            Some(i) => i.strip_prefix("2,").unwrap_or(i).to_string(),
            None => String::new(),
        };

        MaildirName { base, uid, flags }
    }

    /// Build a *fresh local* filename for a moved message: a brand-new unique base,
    /// **no** `,U=` segment (so mbsync assigns a new server UID on push), preserving the
    /// original flags. `seq` and `pid`/`host` come from the caller to keep this pure/testable.
    pub fn new_local(secs: u64, micros: u32, seq: u64, host: &str, flags: &str) -> MaildirName {
        MaildirName {
            base: format!("{secs}.{micros}_{seq}.{host}"),
            uid: None,
            flags: flags.to_string(),
        }
    }

    /// Render back to an on-disk filename.
    pub fn to_filename(&self) -> String {
        let mut s = self.base.clone();
        if let Some(uid) = self.uid {
            s.push_str(&format!(",U={uid}"));
        }
        // Always emit the info section so the file lands in a "cur"-style named form;
        // an empty-flags message is still `:2,`.
        s.push_str(":2,");
        s.push_str(&self.flags);
        s
    }

    /// True if this file carries an mbsync UID (i.e. it is server-synced, not a fresh local).
    pub fn has_uid(&self) -> bool {
        self.uid.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_native_seen() {
        let n = MaildirName::parse("1726376999.528878_10.framework,U=10:2,S");
        assert_eq!(n.base, "1726376999.528878_10.framework");
        assert_eq!(n.uid, Some(10));
        assert_eq!(n.flags, "S");
    }

    #[test]
    fn parse_multi_flags() {
        let n = MaildirName::parse("1726376999.528878_17.framework,U=17:2,PS");
        assert_eq!(n.uid, Some(17));
        assert_eq!(n.flags, "PS");
    }

    #[test]
    fn parse_no_uid_no_flags() {
        let n = MaildirName::parse("1700000000.12345_1.host");
        assert_eq!(n.base, "1700000000.12345_1.host");
        assert_eq!(n.uid, None);
        assert_eq!(n.flags, "");
    }

    #[test]
    fn roundtrip_preserves_bytes() {
        let orig = "1726376999.528878_20.framework,U=20:2,S";
        assert_eq!(MaildirName::parse(orig).to_filename(), orig);
    }

    #[test]
    fn new_local_has_no_uid_but_keeps_flags() {
        // The critical anti-corruption property: a moved message's new file must NOT
        // carry a ,U= segment, or mbsync will treat it as a duplicate server UID.
        let n = MaildirName::new_local(1730000000, 42, 7, "host", "S");
        assert_eq!(n.uid, None);
        assert!(n.has_uid() == false);
        let rendered = n.to_filename();
        assert!(!rendered.contains(",U="), "fresh local file must omit ,U=: {rendered}");
        assert!(rendered.ends_with(":2,S"));
        assert_eq!(rendered, "1730000000.42_7.host:2,S");
    }

    #[test]
    fn malformed_uid_segment_kept_in_base() {
        let n = MaildirName::parse("weird,U=notanumber:2,S");
        assert_eq!(n.base, "weird,U=notanumber");
        assert_eq!(n.uid, None);
        assert_eq!(n.flags, "S");
    }
}
