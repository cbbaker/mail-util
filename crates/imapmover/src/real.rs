//! The real IMAP backend, on top of the `imap` crate over IMAPS. Compiled only with the
//! `real-imap` feature.

use std::net::TcpStream;

use anyhow::{anyhow, Context, Result};
use imap::Session;
use native_tls::{TlsConnector, TlsStream};

use crate::ImapOps;

/// A live authenticated IMAP session implementing [`ImapOps`].
pub struct RealImapOps {
    session: Session<TlsStream<TcpStream>>,
    has_move: bool,
    delimiter: char,
}

impl RealImapOps {
    /// Connect over IMAPS, log in, and probe the server's MOVE capability and hierarchy
    /// separator.
    pub fn connect(host: &str, port: u16, user: &str, pass: &str) -> Result<RealImapOps> {
        let tls = TlsConnector::builder().build().context("building TLS connector")?;
        let client = imap::connect((host, port), host, &tls)
            .with_context(|| format!("connecting to {host}:{port}"))?;
        let mut session = client
            .login(user, pass)
            .map_err(|(e, _)| anyhow!("IMAP login failed: {e}"))?;
        let has_move = session
            .capabilities()
            .context("CAPABILITY")?
            .has_str("MOVE");
        let delimiter = Self::detect_delimiter(&mut session)?;
        Ok(RealImapOps {
            session,
            has_move,
            delimiter,
        })
    }

    /// The probed IMAP hierarchy separator.
    pub fn delimiter(&self) -> char {
        self.delimiter
    }

    /// Discover the hierarchy separator via a top-level `LIST "" "%"`.
    fn detect_delimiter(session: &mut Session<TlsStream<TcpStream>>) -> Result<char> {
        let names = session.list(Some(""), Some("%")).context("LIST")?;
        for n in names.iter() {
            if let Some(d) = n.delimiter().and_then(|s| s.chars().next()) {
                return Ok(d);
            }
        }
        Ok('.')
    }

    /// Cleanly log out.
    pub fn logout(&mut self) -> Result<()> {
        self.session.logout().context("LOGOUT")?;
        Ok(())
    }
}

impl ImapOps for RealImapOps {
    fn has_move(&self) -> bool {
        self.has_move
    }

    fn create_mailbox(&mut self, name: &str) -> Result<()> {
        match self.session.create(name) {
            Ok(()) => Ok(()),
            // Treat "already exists" as success so ensure_folder is idempotent.
            Err(e) if e.to_string().to_lowercase().contains("exist") => Ok(()),
            Err(e) => Err(anyhow!("CREATE {name}: {e}")),
        }
    }

    fn select(&mut self, mailbox: &str) -> Result<()> {
        self.session.select(mailbox).with_context(|| format!("SELECT {mailbox}"))?;
        Ok(())
    }

    fn uid_exists(&mut self, uid: u32) -> Result<bool> {
        let set = self
            .session
            .uid_search(format!("UID {uid}"))
            .context("UID SEARCH")?;
        Ok(set.contains(&uid))
    }

    fn uid_move(&mut self, uid: u32, dst: &str) -> Result<()> {
        self.session
            .uid_mv(uid.to_string(), dst)
            .with_context(|| format!("UID MOVE {uid} -> {dst}"))?;
        Ok(())
    }

    fn uid_copy(&mut self, uid: u32, dst: &str) -> Result<()> {
        self.session
            .uid_copy(uid.to_string(), dst)
            .with_context(|| format!("UID COPY {uid} -> {dst}"))?;
        Ok(())
    }

    fn uid_store_deleted(&mut self, uid: u32) -> Result<()> {
        self.session
            .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
            .with_context(|| format!("UID STORE \\Deleted {uid}"))?;
        Ok(())
    }

    fn uid_expunge(&mut self, uid: u32) -> Result<()> {
        self.session
            .uid_expunge(uid.to_string())
            .with_context(|| format!("UID EXPUNGE {uid}"))?;
        Ok(())
    }
}
