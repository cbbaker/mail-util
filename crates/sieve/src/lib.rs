//! `sieve` — render sorting rules to a Sieve (RFC 5228) script and merge them into an
//! existing user script without disturbing hand-written rules.
//!
//! Generated rules live in a delimited, tool-managed block. Merging **accumulates**: the
//! existing block's rules are kept and the new ones are added, deduplicated by rule logic
//! (the message-count comment is ignored, so re-deploying an updated plan neither
//! duplicates a rule nor drops previously-deployed ones). Everything outside the markers —
//! hand-written rules — is preserved verbatim. A single `require` for the extensions we
//! use is ensured at the top (Sieve requires all `require`s before any other command).

use std::collections::HashMap;

use model::{Cluster, Signal, SieveRule, SieveTest};

/// Markers delimiting the tool-managed section of a user's sieve script.
pub const BEGIN_MARKER: &str = "# BEGIN mail-util (managed — edits here are overwritten)";
pub const END_MARKER: &str = "# END mail-util";

/// Build a sieve rule for a cluster, filing matching mail into `sieve_target`.
pub fn rule_for_cluster(cluster: &Cluster, sieve_target: &str) -> SieveRule {
    let test = match cluster.signal {
        Signal::ListId => SieveTest::HeaderContains {
            header: "List-Id".to_string(),
            value: cluster.key.clone(),
        },
        Signal::SenderDomain => SieveTest::AddressDomain {
            header: "From".to_string(),
            domain: cluster.key.clone(),
        },
        Signal::PersonAddress => SieveTest::AddressIs {
            header: "From".to_string(),
            address: cluster.key.clone(),
        },
    };
    SieveRule {
        test,
        fileinto: sieve_target.to_string(),
        stop: true,
        comment: Some(format!("{} ({} messages)", cluster.key, cluster.count)),
    }
}

/// Escape a string for inclusion in a Sieve double-quoted string (RFC 5228 §2.4.2:
/// backslash and double-quote are the only characters needing escaping).
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

/// Render a single test to its Sieve expression.
pub fn render_test(test: &SieveTest) -> String {
    match test {
        SieveTest::HeaderContains { header, value } => {
            format!("header :contains {} {}", quote(header), quote(value))
        }
        SieveTest::AddressDomain { header, domain } => {
            format!("address :domain :is {} {}", quote(header), quote(domain))
        }
        SieveTest::AddressIs { header, address } => {
            format!("address :all :is {} {}", quote(header), quote(address))
        }
    }
}

/// Render one rule as an `if` block.
pub fn render_rule(rule: &SieveRule) -> String {
    let mut s = String::new();
    if let Some(c) = &rule.comment {
        s.push_str(&format!("# {c}\n"));
    }
    s.push_str(&format!("if {} {{\n", render_test(&rule.test)));
    s.push_str(&format!("    fileinto {};\n", quote(&rule.fileinto)));
    if rule.stop {
        s.push_str("    stop;\n");
    }
    s.push('}');
    s
}

/// The Sieve extensions required by a set of rules. `fileinto` is always needed; the
/// `header`/`address` tests are part of the base spec and need no `require`.
pub fn required_extensions(_rules: &[SieveRule]) -> Vec<&'static str> {
    vec!["fileinto"]
}

/// Render the tool-managed block (just the rules, wrapped in markers) for `rules`.
pub fn render_managed_block(rules: &[SieveRule]) -> String {
    let mut s = String::new();
    s.push_str(BEGIN_MARKER);
    s.push('\n');
    for (i, rule) in rules.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str(&render_rule(rule));
        s.push('\n');
    }
    s.push_str(END_MARKER);
    s
}

/// Render a complete standalone script: a `require`, then the managed block.
pub fn render_script(rules: &[SieveRule]) -> String {
    let exts = required_extensions(rules);
    let require = format!(
        "require [{}];",
        exts.iter().map(|e| quote(e)).collect::<Vec<_>>().join(", ")
    );
    format!("{require}\n\n{}\n", render_managed_block(rules))
}

/// True if the script already declares the `fileinto` extension in some `require`.
fn declares_fileinto(script: &str) -> bool {
    script
        .lines()
        .filter(|l| l.trim_start().starts_with("require"))
        .any(|l| l.contains("fileinto"))
}

/// Merge generated `rules` into `existing`, **accumulating** into the tool-managed block
/// (keeping any rules already there) and preserving all hand-written content. Rules are
/// deduplicated by their logic (comment ignored). Ensures a `require ["fileinto"];`.
pub fn merge(existing: &str, rules: &[SieveRule]) -> String {
    // Collect the managed block's rule chunks, keyed by logic-signature, in order.
    let mut order: Vec<String> = Vec::new();
    let mut by_sig: HashMap<String, String> = HashMap::new();
    let mut add = |chunk: String| {
        let sig = rule_signature(&chunk);
        if !by_sig.contains_key(&sig) {
            order.push(sig.clone());
        }
        by_sig.insert(sig, chunk); // a later, matching rule refreshes the comment
    };
    if let Some(body) = extract_block_body(existing) {
        for chunk in split_rules(&body) {
            add(chunk);
        }
    }
    for rule in rules {
        add(render_rule(rule));
    }
    let chunks: Vec<String> = order.iter().map(|s| by_sig[s].clone()).collect();
    let block = render_block_from_chunks(&chunks);

    // Splice the rebuilt block over the old one, or append it, preserving everything else.
    let merged = match (existing.find(BEGIN_MARKER), existing.find(END_MARKER)) {
        (Some(b), Some(e)) if e > b => {
            let end = e + END_MARKER.len();
            format!("{}{}{}", &existing[..b], block, &existing[end..])
        }
        _ => {
            let mut s = existing.trim_end().to_string();
            if !s.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(&block);
            s.push('\n');
            s
        }
    };

    if declares_fileinto(&merged) {
        merged
    } else {
        format!("require [\"fileinto\"];\n\n{}", merged.trim_start())
    }
}

/// The rules body between the managed markers, trimmed, if a block is present.
fn extract_block_body(script: &str) -> Option<String> {
    let start = script.find(BEGIN_MARKER)? + BEGIN_MARKER.len();
    let end = script[start..].find(END_MARKER)? + start;
    Some(script[start..end].trim().to_string())
}

/// Split a managed-block body into individual rule chunks (rules are separated by a
/// blank line, and never contain one internally).
fn split_rules(body: &str) -> Vec<String> {
    body.split("\n\n")
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// A rule's identity, ignoring `#` comment lines (so a changed message count doesn't
/// look like a different rule). Whitespace is normalized.
fn rule_signature(chunk: &str) -> String {
    chunk
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_block_from_chunks(chunks: &[String]) -> String {
    let mut s = String::new();
    s.push_str(BEGIN_MARKER);
    s.push('\n');
    for (i, c) in chunks.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str(c.trim());
        s.push('\n');
    }
    s.push_str(END_MARKER);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Confidence, Destination};

    fn cluster(signal: Signal, key: &str) -> Cluster {
        Cluster {
            key: key.to_string(),
            signal,
            destination: Destination::New { dotpath: ".x".into() },
            count: 12,
            score: 1.0,
            confidence: Confidence::High,
            sample_senders: vec![],
            sample_subjects: vec![],
            uids: vec![],
        }
    }

    #[test]
    fn list_id_rule() {
        let r = rule_for_cluster(&cluster(Signal::ListId, "elixir.groups.io"), "lists.elixir");
        let text = render_rule(&r);
        assert!(text.contains(r#"header :contains "List-Id" "elixir.groups.io""#));
        assert!(text.contains(r#"fileinto "lists.elixir";"#));
        assert!(text.contains("stop;"));
    }

    #[test]
    fn domain_rule() {
        let r = rule_for_cluster(&cluster(Signal::SenderDomain, "github.com"), "vendors.github");
        assert!(render_rule(&r).contains(r#"address :domain :is "From" "github.com""#));
    }

    #[test]
    fn person_rule() {
        let r = rule_for_cluster(&cluster(Signal::PersonAddress, "jane@gmail.com"), "people.jane");
        assert!(render_rule(&r).contains(r#"address :all :is "From" "jane@gmail.com""#));
    }

    #[test]
    fn quote_escapes() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn script_has_require() {
        let r = rule_for_cluster(&cluster(Signal::ListId, "l"), "L");
        let s = render_script(&[r]);
        assert!(s.starts_with(r#"require ["fileinto"];"#));
        assert!(s.contains(BEGIN_MARKER));
        assert!(s.contains(END_MARKER));
    }

    #[test]
    fn merge_into_empty_adds_require_and_block() {
        let r = rule_for_cluster(&cluster(Signal::ListId, "l"), "L");
        let out = merge("", &[r]);
        assert!(out.contains(r#"require ["fileinto"];"#));
        assert!(out.contains(BEGIN_MARKER));
    }

    #[test]
    fn merge_accumulates_and_preserves_handwritten() {
        let r1 = rule_for_cluster(&cluster(Signal::ListId, "first"), "First");
        let existing = merge("require [\"fileinto\"];\n\n# my own rule\nif true { keep; }\n", &[r1]);
        assert!(existing.contains("# my own rule"));

        // Deploy a *different* rule later: the hand rule and the FIRST managed rule both
        // stay, and the new one is added.
        let r2 = rule_for_cluster(&cluster(Signal::ListId, "second"), "Second");
        let out = merge(&existing, &[r2]);
        assert!(out.contains("# my own rule"), "hand-written rule preserved");
        assert!(out.contains("\"first\""), "earlier managed rule kept: {out}");
        assert!(out.contains("\"second\""), "new managed rule added");
        // Still exactly one managed block, one require.
        assert_eq!(out.matches(BEGIN_MARKER).count(), 1);
        assert_eq!(out.matches("require").count(), 1);
    }

    #[test]
    fn merge_dedups_by_logic_ignoring_comment() {
        // Same rule logic, but the comment (message count) differs between deploys.
        let mut a = rule_for_cluster(&cluster(Signal::ListId, "l"), "L");
        a.comment = Some("l (4 messages)".into());
        let mut b = rule_for_cluster(&cluster(Signal::ListId, "l"), "L");
        b.comment = Some("l (9 messages)".into());

        let first = merge("", &[a]);
        let second = merge(&first, &[b]);
        // Not duplicated: only one `if` block for this rule.
        assert_eq!(second.matches("if header").count(), 1, "{second}");
        // The comment was refreshed to the latest count.
        assert!(second.contains("l (9 messages)"));
        assert!(!second.contains("l (4 messages)"));
    }

    #[test]
    fn merge_is_idempotent() {
        let r = rule_for_cluster(&cluster(Signal::ListId, "l"), "L");
        let once = merge("", &[r.clone()]);
        let twice = merge(&once, &[r]);
        assert_eq!(once, twice);
    }
}
