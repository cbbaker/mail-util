//! Streaming header extraction: pull just the clustering-relevant fields out of a
//! message file without loading or decoding the body.
//!
//! For a 100k-message cache we read only the header block (up to the first blank line)
//! and hand that to `mail-parser`, so per-message cost stays small.

use std::sync::OnceLock;

use mail_parser::{HeaderValue, MessageParser};
use model::MessageHeaders;

/// mail-parser's `default()`/`new()` parser registers *no* headers (everything falls to
/// raw), so typed accessors like `from()` return nothing. We register exactly the fields
/// we cluster on: the minimal RFC set (From/To/Cc/Subject/…), Message-Id, and the list
/// headers as raw. HeaderName matching is case-insensitive, so `List-Id` also matches
/// `List-ID`.
fn parser() -> &'static MessageParser {
    static P: OnceLock<MessageParser> = OnceLock::new();
    P.get_or_init(|| {
        MessageParser::new()
            .with_minimal_headers()
            .with_message_ids()
            .header_raw("List-Id")
            .header_raw("List-Unsubscribe")
            .header_raw("List-Post")
            .header_raw("Delivered-To")
    })
}

/// Extract the header block (bytes up to and including the first blank line) from a
/// raw RFC5322 message. Handles both `\n\n` and `\r\n\r\n` separators. If no blank
/// line is found the whole input is returned (a header-only file).
pub fn header_block(raw: &[u8]) -> &[u8] {
    // Search for a blank line: \n\n or \n\r\n.
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\n' {
            if i + 1 < raw.len() && raw[i + 1] == b'\n' {
                return &raw[..i + 2];
            }
            if i + 2 < raw.len() && raw[i + 1] == b'\r' && raw[i + 2] == b'\n' {
                return &raw[..i + 3];
            }
        }
        i += 1;
    }
    raw
}

/// Normalize a `List-Id` header value to its bare identifier: the text inside the last
/// `<...>` if present (RFC 2919), otherwise the whole trimmed value; lowercased.
fn normalize_list_id(raw: &str) -> String {
    let inner = match (raw.rfind('<'), raw.rfind('>')) {
        (Some(a), Some(b)) if a < b => &raw[a + 1..b],
        _ => raw.trim(),
    };
    inner.trim().to_lowercase()
}

/// Parse the header-relevant fields from raw message bytes. Returns `(headers, message_id)`.
pub fn parse(raw: &[u8]) -> (MessageHeaders, Option<String>) {
    let block = header_block(raw);
    let parsed = match parser().parse(block) {
        Some(m) => m,
        None => return (MessageHeaders::default(), None),
    };

    let mut h = MessageHeaders::default();

    // List-Id (registered as raw; case-insensitive lookup covers List-ID).
    if let Some(t) = parsed.header_raw("List-Id") {
        h.list_id = Some(normalize_list_id(t));
    }

    // Sender address + display name.
    if let Some(addr) = parsed.from().and_then(|a| a.first()) {
        h.from_addr = addr.address().map(|s| s.trim().to_lowercase());
        h.from_name = addr.name().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }

    // Recipients from To / Cc / Delivered-To.
    let mut recips: Vec<String> = Vec::new();
    for group in [parsed.to(), parsed.cc()] {
        if let Some(list) = group {
            for a in list.iter() {
                if let Some(addr) = a.address() {
                    recips.push(addr.trim().to_lowercase());
                }
            }
        }
    }
    if let Some(v) = parsed.header("Delivered-To") {
        if let Some(t) = header_as_text(v) {
            recips.push(t.trim().to_lowercase());
        }
    }
    recips.sort();
    recips.dedup();
    h.recipients = recips;

    h.subject = parsed.subject().map(|s| s.trim().to_string());

    h.is_list_mail = parsed.header("List-Unsubscribe").is_some()
        || parsed.header("List-Post").is_some()
        || h.list_id.is_some();

    // Message-ID: strip angle brackets, lowercase.
    let message_id = parsed
        .message_id()
        .map(|m| m.trim().trim_matches(|c| c == '<' || c == '>').to_lowercase())
        .filter(|s| !s.is_empty());

    (h, message_id)
}

/// Coerce an arbitrary header value to a single text string when possible.
fn header_as_text(v: &HeaderValue) -> Option<String> {
    match v {
        HeaderValue::Text(t) => Some(t.to_string()),
        HeaderValue::TextList(list) => list.first().map(|t| t.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_block_splits_on_blank_line() {
        let raw = b"From: a@b.com\nSubject: hi\n\nbody here\nmore body";
        let block = header_block(raw);
        assert!(block.ends_with(b"\n\n"));
        assert!(!std::str::from_utf8(block).unwrap().contains("body here"));
    }

    #[test]
    fn normalize_list_id_extracts_angle_brackets() {
        assert_eq!(
            normalize_list_id("Parents list <Parents1.labschool.UCLA.edu>"),
            "parents1.labschool.ucla.edu"
        );
        assert_eq!(normalize_list_id("elixir.foo"), "elixir.foo");
    }

    #[test]
    fn parse_extracts_core_fields() {
        let raw = b"From: Jane Doe <Jane@Example.COM>\r\n\
                    To: me@here.org, you@there.org\r\n\
                    Subject: Weekly digest\r\n\
                    List-ID: The List <the-list.example.com>\r\n\
                    List-Unsubscribe: <mailto:x>\r\n\
                    Message-ID: <ABC123@example.com>\r\n\
                    \r\n\
                    body";
        let (h, mid) = parse(raw);
        assert_eq!(h.from_addr.as_deref(), Some("jane@example.com"));
        assert_eq!(h.from_name.as_deref(), Some("Jane Doe"));
        assert_eq!(h.list_id.as_deref(), Some("the-list.example.com"));
        assert_eq!(h.from_domain(), Some("example.com"));
        assert!(h.is_list_mail);
        assert_eq!(h.subject.as_deref(), Some("Weekly digest"));
        assert!(h.recipients.contains(&"me@here.org".to_string()));
        assert_eq!(mid.as_deref(), Some("abc123@example.com"));
    }

    #[test]
    fn parse_no_list_headers() {
        let raw = b"From: bob@work.example\nSubject: lunch?\n\nhi";
        let (h, _) = parse(raw);
        assert_eq!(h.list_id, None);
        assert!(!h.is_list_mail);
        assert_eq!(h.from_domain(), Some("work.example"));
    }
}
