//! Real ManageSieve (RFC 5804) backend over STARTTLS + SASL PLAIN. Compiled only with the
//! `real-sieve` feature.
//!
//! Only the subset the deployer needs is implemented: greeting, STARTTLS, AUTHENTICATE,
//! LISTSCRIPTS, GETSCRIPT, PUTSCRIPT, SETACTIVE.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use native_tls::{TlsConnector, TlsStream};

use crate::SieveOps;

pub struct RealSieveOps {
    reader: BufReader<TlsStream<TcpStream>>,
}

impl RealSieveOps {
    /// Connect to `host:port`, STARTTLS, and authenticate as `user` via SASL PLAIN.
    pub fn connect(host: &str, port: u16, user: &str, pass: &str) -> Result<RealSieveOps> {
        let tcp = TcpStream::connect((host, port))
            .with_context(|| format!("connecting to {host}:{port}"))?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;

        // Plaintext phase: read greeting capabilities, request STARTTLS.
        let mut plain = BufReader::new(tcp);
        read_status(&mut plain).context("reading server greeting")?;
        write_line(plain.get_mut(), "STARTTLS")?;
        read_status(&mut plain).context("STARTTLS")?;
        if !plain.buffer().is_empty() {
            bail!("server sent unexpected data before the TLS handshake");
        }

        // Upgrade to TLS, then read the post-TLS capabilities and authenticate.
        let tcp = plain.into_inner();
        let connector = TlsConnector::new().context("building TLS connector")?;
        let tls = connector
            .connect(host, tcp)
            .with_context(|| format!("TLS handshake with {host}"))?;
        let mut reader = BufReader::new(tls);
        read_status(&mut reader).context("post-TLS capabilities")?;

        let token = base64_encode(format!("\0{user}\0{pass}").as_bytes());
        write_line(reader.get_mut(), &format!("AUTHENTICATE \"PLAIN\" \"{token}\""))?;
        read_status(&mut reader).context("authentication")?;

        Ok(RealSieveOps { reader })
    }

    pub fn logout(&mut self) -> Result<()> {
        write_line(self.reader.get_mut(), "LOGOUT")?;
        let _ = read_status(&mut self.reader);
        Ok(())
    }
}

impl SieveOps for RealSieveOps {
    fn active_script(&mut self) -> Result<Option<String>> {
        write_line(self.reader.get_mut(), "LISTSCRIPTS")?;
        let lines = read_status(&mut self.reader).context("LISTSCRIPTS")?;
        for line in lines {
            // Format: "name" [ACTIVE]
            if line.to_ascii_uppercase().contains("ACTIVE") {
                if let Some(name) = unquote_first(&line) {
                    return Ok(Some(name));
                }
            }
        }
        Ok(None)
    }

    fn get_script(&mut self, name: &str) -> Result<Option<String>> {
        write_line(self.reader.get_mut(), &format!("GETSCRIPT {}", quote(name)))?;
        let first = read_line(&mut self.reader)?;
        let trimmed = first.trim();
        if let Some(size) = parse_literal(trimmed) {
            let mut buf = vec![0u8; size];
            self.reader.read_exact(&mut buf).context("reading script body")?;
            // Consume the trailing lines up to OK.
            read_status(&mut self.reader).context("GETSCRIPT trailer")?;
            let content = String::from_utf8(buf).context("script is not UTF-8")?;
            Ok(Some(content))
        } else if trimmed.to_ascii_uppercase().starts_with("NO") {
            Ok(None) // no such script
        } else if trimmed.to_ascii_uppercase().starts_with("OK") {
            Ok(Some(String::new())) // empty script
        } else {
            bail!("unexpected GETSCRIPT response: {trimmed}");
        }
    }

    fn put_script(&mut self, name: &str, content: &str) -> Result<()> {
        let bytes = content.as_bytes();
        // Non-synchronizing literal ({n+}) so we can stream the body immediately.
        let header = format!("PUTSCRIPT {} {{{}+}}\r\n", quote(name), bytes.len());
        let w = self.reader.get_mut();
        w.write_all(header.as_bytes())?;
        w.write_all(bytes)?;
        w.write_all(b"\r\n")?;
        w.flush()?;
        read_status(&mut self.reader).context("PUTSCRIPT (server rejected the script?)")?;
        Ok(())
    }

    fn set_active(&mut self, name: &str) -> Result<()> {
        write_line(self.reader.get_mut(), &format!("SETACTIVE {}", quote(name)))?;
        read_status(&mut self.reader).context("SETACTIVE")?;
        Ok(())
    }
}

// ---- protocol helpers ----

fn write_line<W: Write>(w: &mut W, line: &str) -> Result<()> {
    w.write_all(line.as_bytes())?;
    w.write_all(b"\r\n")?;
    w.flush()?;
    Ok(())
}

fn read_line<R: BufRead>(r: &mut R) -> Result<String> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf)?;
    if n == 0 {
        bail!("connection closed by server");
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Read response lines until a final OK/NO/BYE. Returns the lines before it; errors on
/// NO/BYE with the server's message.
fn read_status<R: BufRead>(r: &mut R) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    loop {
        let line = read_line(r)?;
        let upper = line.trim_start().to_ascii_uppercase();
        if upper.starts_with("OK") {
            return Ok(lines);
        }
        if upper.starts_with("NO") || upper.starts_with("BYE") {
            return Err(anyhow!("server said: {}", line.trim()));
        }
        lines.push(line);
    }
}

/// If `s` is a literal marker like `{123}` or `{123+}`, return the size.
fn parse_literal(s: &str) -> Option<usize> {
    let inner = s.strip_prefix('{')?.strip_suffix('}')?;
    inner.trim_end_matches('+').parse().ok()
}

/// Extract the first double-quoted token from a line.
fn unquote_first(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    Some(line[start..end].to_string())
}

/// Quote a name for the protocol (escape backslash and double-quote).
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Standard base64 (for SASL PLAIN); avoids pulling in a dependency.
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod real_tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // SASL PLAIN token shape.
        assert_eq!(base64_encode(b"\0user\0pass"), "AHVzZXIAcGFzcw==");
    }

    #[test]
    fn parse_literal_forms() {
        assert_eq!(parse_literal("{123}"), Some(123));
        assert_eq!(parse_literal("{123+}"), Some(123));
        assert_eq!(parse_literal("OK"), None);
    }

    #[test]
    fn unquote_first_name() {
        assert_eq!(unquote_first(r#""roundcube" ACTIVE"#).as_deref(), Some("roundcube"));
    }
}
