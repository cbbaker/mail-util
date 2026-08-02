//! `namemap` — translate a folder among its three names:
//!
//! - **local dotpath**: how the folder appears on disk under the account root, using the
//!   Maildir++ convention where each hierarchy level is a `.`-prefixed directory and
//!   nesting uses the filesystem separator `/` — e.g. `.lists/.elixir`.
//! - **IMAP mailbox name**: how the server names it, with hierarchy levels joined by the
//!   server's *hierarchy separator* — e.g. `lists.elixir` (separator `.`) or `lists/elixir`
//!   (separator `/`).
//! - **sieve `fileinto` target**: identical to the IMAP mailbox name.
//!
//! The IMAP hierarchy separator varies by server, so it is a parameter here rather than a
//! constant. In the full tool it is discovered once via an IMAP `LIST` probe; this crate
//! stays pure and just takes the separator it is given.
//!
//! Note: like Maildir++ itself, this assumes a folder-name component never contains the
//! separator character. Slugs produced by the suggestion engine never do.

/// A folder-name translator bound to one IMAP hierarchy separator.
#[derive(Debug, Clone, Copy)]
pub struct NameMap {
    sep: char,
}

impl NameMap {
    /// Create a translator for the given IMAP hierarchy separator (commonly `.` or `/`).
    pub fn new(sep: char) -> NameMap {
        NameMap { sep }
    }

    pub fn separator(&self) -> char {
        self.sep
    }

    /// Split a local dotpath into its hierarchy components, stripping the leading `.`
    /// of each level. `.lists/.elixir` -> `["lists", "elixir"]`, `.INBOX` -> `["INBOX"]`.
    pub fn dotpath_components(dotpath: &str) -> Vec<&str> {
        dotpath
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|seg| seg.strip_prefix('.').unwrap_or(seg))
            .collect()
    }

    /// Convert a local dotpath to the IMAP mailbox name.
    pub fn dotpath_to_imap(&self, dotpath: &str) -> String {
        Self::dotpath_components(dotpath)
            .join(&self.sep.to_string())
    }

    /// The sieve `fileinto` target for a dotpath — the IMAP mailbox name.
    pub fn dotpath_to_sieve(&self, dotpath: &str) -> String {
        self.dotpath_to_imap(dotpath)
    }

    /// Convert an IMAP mailbox name back to a local dotpath.
    pub fn imap_to_dotpath(&self, imap: &str) -> String {
        imap.split(self.sep)
            .filter(|s| !s.is_empty())
            .map(|c| format!(".{c}"))
            .collect::<Vec<_>>()
            .join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotpath_to_imap_dot_separator() {
        let nm = NameMap::new('.');
        assert_eq!(nm.dotpath_to_imap(".lists/.elixir"), "lists.elixir");
        assert_eq!(nm.dotpath_to_imap(".INBOX"), "INBOX");
        assert_eq!(nm.dotpath_to_imap(".vendors/.rei"), "vendors.rei");
    }

    #[test]
    fn dotpath_to_imap_slash_separator() {
        let nm = NameMap::new('/');
        assert_eq!(nm.dotpath_to_imap(".lists/.elixir"), "lists/elixir");
        assert_eq!(nm.dotpath_to_imap(".INBOX"), "INBOX");
    }

    #[test]
    fn sieve_target_matches_imap() {
        let nm = NameMap::new('.');
        assert_eq!(nm.dotpath_to_sieve(".lists/.elixir"), "lists.elixir");
    }

    #[test]
    fn imap_to_dotpath_roundtrips() {
        for sep in ['.', '/'] {
            let nm = NameMap::new(sep);
            for dotpath in [".INBOX", ".lists/.elixir", ".people/.jane-doe", ".a/.b/.c"] {
                let imap = nm.dotpath_to_imap(dotpath);
                assert_eq!(nm.imap_to_dotpath(&imap), dotpath, "sep={sep} dotpath={dotpath}");
            }
        }
    }

    #[test]
    fn components_strip_leading_dots() {
        assert_eq!(NameMap::dotpath_components(".lists/.elixir"), vec!["lists", "elixir"]);
        assert_eq!(NameMap::dotpath_components(".INBOX"), vec!["INBOX"]);
    }
}
