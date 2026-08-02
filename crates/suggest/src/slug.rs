//! Turning keys (list-ids, domains, addresses) into folder slugs that match the user's
//! existing naming style: short, lowercase, hyphenated (`la-ruby`, `dead-snakes`).

/// Domains where the domain itself is not a useful sorting key, so we cluster by the
/// full sender address (person) instead.
pub const FREEMAIL: &[&str] = &[
    "gmail.com", "googlemail.com", "yahoo.com", "ymail.com", "hotmail.com",
    "outlook.com", "live.com", "msn.com", "aol.com", "icloud.com", "me.com",
    "mac.com", "proton.me", "protonmail.com", "gmx.com", "fastmail.com",
];

pub fn is_freemail(domain: &str) -> bool {
    FREEMAIL.contains(&domain)
}

/// Lowercase, replace runs of non-alphanumeric with single hyphens, trim hyphens.
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Slug for a List-Id value, e.g. `elixir-lang.groups.io` -> `elixir-lang`,
/// `parents1.labschool.ucla.edu` -> `parents1`. We take the first dot-label since that
/// is the list's own name in the observed `.lists/.<name>` taxonomy.
pub fn list_slug(list_id: &str) -> String {
    let first = list_id.split('.').next().unwrap_or(list_id);
    slugify(first)
}

/// True if a slug is unusable as a human folder name: empty, generic (`www`), all
/// digits, or a long hex blob (Mailchimp/ESP campaign ids like
/// `9d03786bc12364dc5eca9fb32`). Callers fall back to a better signal (display name /
/// domain) when this holds.
pub fn is_opaque_slug(slug: &str) -> bool {
    let bare: String = slug.chars().filter(|c| *c != '-').collect();
    if bare.is_empty() || slug == "www" {
        return true;
    }
    let all_digits = bare.chars().all(|c| c.is_ascii_digit());
    let hex_blob = bare.len() >= 8 && bare.chars().all(|c| c.is_ascii_hexdigit());
    all_digits || hex_blob
}

/// Slug for a sender domain, e.g. `github.com` -> `github`, `news.ycombinator.com` ->
/// take the registrable-ish label just left of the public suffix (approximated as the
/// label before the final one or two components).
pub fn domain_slug(domain: &str) -> String {
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    let name = match labels.as_slice() {
        [] => domain,
        [single] => single,
        // For two-part public suffixes (co.uk, com.au) take the third-from-last.
        [.., a, b, c] if is_two_level_suffix(b, c) => a,
        [.., a, _] => a,
    };
    slugify(name)
}

fn is_two_level_suffix(second: &str, last: &str) -> bool {
    matches!(
        (second, last),
        ("co", "uk") | ("org", "uk") | ("ac", "uk") | ("co", "nz") | ("com", "au") | ("co", "jp")
    )
}

/// Slug for a person address: prefer the display name if given, else the local part.
pub fn person_slug(addr: &str, name: Option<&str>) -> String {
    if let Some(n) = name {
        let s = slugify(n);
        if !s.is_empty() {
            return s;
        }
    }
    let local = addr.split('@').next().unwrap_or(addr);
    slugify(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("La Ruby!"), "la-ruby");
        assert_eq!(slugify("dead_snakes"), "dead-snakes");
        assert_eq!(slugify("  spaced  "), "spaced");
    }

    #[test]
    fn list_slug_takes_first_label() {
        assert_eq!(list_slug("elixir-lang.groups.io"), "elixir-lang");
        assert_eq!(list_slug("parents1.labschool.ucla.edu"), "parents1");
    }

    #[test]
    fn domain_slug_registrable() {
        assert_eq!(domain_slug("github.com"), "github");
        assert_eq!(domain_slug("news.ycombinator.com"), "ycombinator");
        assert_eq!(domain_slug("shop.example.co.uk"), "example");
    }

    #[test]
    fn detects_opaque_slugs() {
        assert!(is_opaque_slug("9d03786bc12364dc5eca9fb32"));
        assert!(is_opaque_slug("100025145"));
        assert!(is_opaque_slug("www"));
        assert!(is_opaque_slug(""));
        assert!(!is_opaque_slug("danieldrezner"));
        assert!(!is_opaque_slug("elixir-lang"));
        assert!(!is_opaque_slug("parents1")); // has letters, not a blob
    }

    #[test]
    fn person_slug_prefers_name() {
        assert_eq!(person_slug("jane.doe@gmail.com", Some("Jane Doe")), "jane-doe");
        assert_eq!(person_slug("jdoe@gmail.com", None), "jdoe");
    }
}
