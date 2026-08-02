//! `suggest` — deterministic clustering of inbox messages into proposed sorting folders.
//!
//! The core is pure and reproducible: same messages in, same clusters out. An optional
//! LLM labeling pass (later milestone) may only rename slugs, never re-group messages.

pub mod slug;

use std::collections::BTreeMap;

use model::{Cluster, Confidence, Config, Destination, Message, Signal};

/// A folder that already exists in the cache, for taxonomy reuse. `leaf` is the final
/// path component with leading dots stripped (e.g. ".lists/.elixir" -> "elixir").
#[derive(Debug, Clone)]
pub struct ExistingFolder {
    pub dotpath: String,
    pub leaf: String,
}

impl ExistingFolder {
    pub fn new(dotpath: &str) -> ExistingFolder {
        let leaf = dotpath
            .rsplit('/')
            .next()
            .unwrap_or(dotpath)
            .trim_start_matches('.')
            .to_string();
        ExistingFolder {
            dotpath: dotpath.to_string(),
            leaf,
        }
    }
}

/// Internal accumulator for one cluster before scoring.
#[derive(Default)]
struct Bucket {
    count: usize,
    senders: Vec<String>,
    subjects: Vec<String>,
    uids: Vec<u32>,
    // For slug generation of person buckets.
    from_name: Option<String>,
}

/// Choose the clustering key for a message: List-Id > sender-domain > person-address.
/// Returns `(signal, key)` or `None` if the message has no usable signal.
///
/// Public so the `plan` command can assign each message (including uid-less ones) to the
/// cluster that motivates its move, using exactly the engine's own clustering rule.
pub fn classify(m: &Message) -> Option<(Signal, String)> {
    if let Some(lid) = &m.headers.list_id {
        return Some((Signal::ListId, lid.clone()));
    }
    let domain = m.headers.from_domain()?;
    if slug::is_freemail(domain) {
        // Personal mail: the domain says nothing, cluster by the individual.
        let addr = m.headers.from_addr.as_deref()?;
        return Some((Signal::PersonAddress, addr.to_string()));
    }
    Some((Signal::SenderDomain, domain.to_string()))
}

/// Produce ranked cluster suggestions for a set of inbox messages.
pub fn suggest(messages: &[Message], existing: &[ExistingFolder], config: &Config) -> Vec<Cluster> {
    // Group into buckets keyed by (signal, key).
    let mut buckets: BTreeMap<(Signal, String), Bucket> = BTreeMap::new();
    for m in messages {
        let Some((signal, key)) = classify(m) else {
            continue;
        };
        let b = buckets.entry((signal, key.clone())).or_default();
        b.count += 1;
        if let Some(uid) = m.uid {
            b.uids.push(uid);
        }
        if let Some(f) = &m.headers.from_addr {
            if b.senders.len() < 3 && !b.senders.contains(f) {
                b.senders.push(f.clone());
            }
        }
        if b.from_name.is_none() {
            b.from_name = m.headers.from_name.clone();
        }
        if let Some(s) = &m.headers.subject {
            if b.subjects.len() < 3 && !b.subjects.contains(s) {
                b.subjects.push(s.clone());
            }
        }
    }

    let mut clusters: Vec<Cluster> = Vec::new();
    for ((signal, key), b) in buckets {
        if b.count < config.min_count {
            continue;
        }
        let (destination, reused) = choose_destination(signal, &key, &b, existing, config);
        let score = score(b.count, signal, reused);
        if score < config.min_score {
            continue;
        }
        clusters.push(Cluster {
            key,
            signal,
            destination,
            count: b.count,
            score,
            confidence: confidence(signal, b.count),
            sample_senders: b.senders,
            sample_subjects: b.subjects,
            uids: b.uids,
        });
    }

    // Rank: score desc, then count desc, then key for determinism.
    clusters.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
            .then(a.key.cmp(&b.key))
    });
    clusters
}

/// Decide the destination folder and whether it reuses an existing one.
fn choose_destination(
    signal: Signal,
    key: &str,
    b: &Bucket,
    existing: &[ExistingFolder],
    config: &Config,
) -> (Destination, bool) {
    let slug = match signal {
        Signal::ListId => {
            // Many ESPs (Mailchimp, SparkPost) use opaque list-ids; when the derived
            // slug is a hash/number, prefer the sender's display name, then its domain,
            // so the proposed folder is human-readable.
            let s = slug::list_slug(key);
            if slug::is_opaque_slug(&s) {
                let from_name = b
                    .from_name
                    .as_deref()
                    .map(|n| slug::person_slug("", Some(n)))
                    .filter(|s| !slug::is_opaque_slug(s));
                let from_domain = b
                    .senders
                    .first()
                    .and_then(|a| a.rsplit('@').next())
                    .map(slug::domain_slug)
                    .filter(|d| !slug::is_opaque_slug(d));
                from_name.or(from_domain).unwrap_or(s)
            } else {
                s
            }
        }
        Signal::SenderDomain => slug::domain_slug(key),
        Signal::PersonAddress => slug::person_slug(key, b.from_name.as_deref()),
    };

    // Taxonomy reuse: if an existing folder's leaf equals the slug, file into it.
    if let Some(f) = existing.iter().find(|f| f.leaf == slug) {
        return (Destination::Existing { dotpath: f.dotpath.clone() }, true);
    }

    let parent = match signal {
        Signal::ListId => ".lists",
        Signal::PersonAddress => ".people",
        Signal::SenderDomain => {
            if config.work_domains.iter().any(|d| d == key) {
                return (Destination::New { dotpath: ".work".to_string() }, false);
            }
            ".vendors"
        }
    };
    (
        Destination::New {
            dotpath: format!("{parent}/.{slug}"),
        },
        false,
    )
}

fn score(count: usize, signal: Signal, reused: bool) -> f64 {
    let volume = ((count as f64) + 1.0).ln();
    let reuse_bonus = if reused { 0.5 } else { 0.0 };
    volume + signal.strength() * 2.0 + reuse_bonus
}

fn confidence(signal: Signal, count: usize) -> Confidence {
    match signal {
        Signal::ListId => Confidence::High,
        Signal::SenderDomain if count >= 20 => Confidence::High,
        Signal::SenderDomain => Confidence::Medium,
        Signal::PersonAddress if count >= 20 => Confidence::Medium,
        Signal::PersonAddress => Confidence::Low,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::MessageHeaders;

    fn msg(uid: u32, from: &str, list_id: Option<&str>, subject: &str) -> Message {
        let mut h = MessageHeaders::default();
        h.from_addr = Some(from.to_string());
        h.list_id = list_id.map(|s| s.to_string());
        h.is_list_mail = list_id.is_some();
        h.subject = Some(subject.to_string());
        Message {
            folder: ".INBOX".to_string(),
            uid: Some(uid),
            filename: format!("f{uid}"),
            is_new: false,
            message_id: Some(format!("mid{uid}")),
            headers: h,
        }
    }

    fn cfg() -> Config {
        Config { min_count: 3, ..Config::default() }
    }

    #[test]
    fn clusters_by_list_id() {
        let msgs: Vec<_> = (0..5)
            .map(|i| msg(i, "bounce@lists.example.com", Some("elixir.groups.io"), "hi"))
            .collect();
        let clusters = suggest(&msgs, &[], &cfg());
        assert_eq!(clusters.len(), 1);
        let c = &clusters[0];
        assert_eq!(c.signal, Signal::ListId);
        assert_eq!(c.count, 5);
        assert_eq!(c.confidence, Confidence::High);
        assert_eq!(c.destination, Destination::New { dotpath: ".lists/.elixir".into() });
        assert_eq!(c.uids.len(), 5);
    }

    #[test]
    fn clusters_by_domain() {
        let msgs: Vec<_> = (0..4)
            .map(|i| msg(i, &format!("no-reply{i}@github.com"), None, "PR"))
            .collect();
        let clusters = suggest(&msgs, &[], &cfg());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].signal, Signal::SenderDomain);
        assert_eq!(clusters[0].destination, Destination::New { dotpath: ".vendors/.github".into() });
    }

    #[test]
    fn freemail_clusters_by_person() {
        let msgs: Vec<_> = (0..3)
            .map(|i| msg(i, "jane.doe@gmail.com", None, "hi"))
            .collect();
        let clusters = suggest(&msgs, &[], &cfg());
        assert_eq!(clusters[0].signal, Signal::PersonAddress);
        assert_eq!(clusters[0].destination, Destination::New { dotpath: ".people/.jane-doe".into() });
    }

    #[test]
    fn reuses_existing_folder() {
        let msgs: Vec<_> = (0..5)
            .map(|i| msg(i, "b@l.com", Some("elixir.groups.io"), "hi"))
            .collect();
        let existing = vec![ExistingFolder::new(".lists/.elixir")];
        let clusters = suggest(&msgs, &existing, &cfg());
        assert_eq!(
            clusters[0].destination,
            Destination::Existing { dotpath: ".lists/.elixir".into() }
        );
    }

    #[test]
    fn below_min_count_dropped() {
        let msgs = vec![msg(1, "a@github.com", None, "x")];
        assert!(suggest(&msgs, &[], &cfg()).is_empty());
    }

    #[test]
    fn deterministic_ordering() {
        let mut msgs = Vec::new();
        for i in 0..10 {
            msgs.push(msg(i, "a@github.com", None, "x"));
        }
        for i in 10..14 {
            msgs.push(msg(i, "b@l.com", Some("list.example.com"), "y"));
        }
        let a = suggest(&msgs, &[], &cfg());
        let b = suggest(&msgs, &[], &cfg());
        assert_eq!(a, b);
        // List-Id (strength 1.0) should outrank domain despite lower count.
        assert_eq!(a[0].signal, Signal::ListId);
    }
}
