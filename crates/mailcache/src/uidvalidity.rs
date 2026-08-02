//! Parsing of mbsync's per-folder `.uidvalidity` file (native scheme).
//!
//! Format is two lines of ASCII decimal:
//!
//! ```text
//! 1726376998   <- UIDVALIDITY
//! 30068        <- highest UID seen
//! ```
//!
//! We only ever read this. The critical safety property elsewhere is that line 1
//! (`validity`) must be **unchanged** after a move — a change means mbsync forced a
//! resync, which is the corruption we are designed to avoid.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidValidity {
    pub validity: u64,
    pub max_uid: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum UidValidityError {
    #[error("uidvalidity file missing the validity line")]
    MissingValidity,
    #[error("could not parse uidvalidity number: {0}")]
    Parse(String),
}

impl UidValidity {
    /// Parse the textual contents of a `.uidvalidity` file. A missing max-UID line is
    /// tolerated (treated as 0) since some folders are freshly created.
    pub fn parse(contents: &str) -> Result<UidValidity, UidValidityError> {
        let mut lines = contents.lines();
        let validity = lines
            .next()
            .ok_or(UidValidityError::MissingValidity)?
            .trim();
        let validity: u64 = validity
            .parse()
            .map_err(|_| UidValidityError::Parse(validity.to_string()))?;
        let max_uid = match lines.next() {
            Some(l) if !l.trim().is_empty() => l
                .trim()
                .parse()
                .map_err(|_| UidValidityError::Parse(l.trim().to_string()))?,
            _ => 0,
        };
        Ok(UidValidity { validity, max_uid })
    }

    /// Read and parse the `.uidvalidity` inside a folder directory. Returns `Ok(None)`
    /// if the file does not exist (folder not yet synced).
    pub fn read_from_folder(folder_dir: &Path) -> anyhow::Result<Option<UidValidity>> {
        let path = folder_dir.join(".uidvalidity");
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(UidValidity::parse(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_two_lines() {
        let uv = UidValidity::parse("1726376998\n30068\n").unwrap();
        assert_eq!(uv.validity, 1726376998);
        assert_eq!(uv.max_uid, 30068);
    }

    #[test]
    fn parse_missing_maxuid_defaults_zero() {
        let uv = UidValidity::parse("1726376998\n").unwrap();
        assert_eq!(uv.validity, 1726376998);
        assert_eq!(uv.max_uid, 0);
    }

    #[test]
    fn parse_empty_is_error() {
        assert!(UidValidity::parse("").is_err());
    }
}
