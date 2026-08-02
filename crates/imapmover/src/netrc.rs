//! Minimal `.netrc` parsing — enough to fetch the login/password for a machine, the same
//! source mbsync's `PassCmd` reads from. We never store or log the password.

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetrcAuth {
    pub login: String,
    pub password: String,
}

/// Find the `login`/`password` for `machine` in netrc `contents`. Tokens are
/// whitespace-separated per the netrc format; a `machine`/`default` keyword ends the
/// previous entry.
pub fn find_machine(contents: &str, machine: &str) -> Option<NetrcAuth> {
    let tokens: Vec<&str> = contents.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let is_match = (tokens[i] == "machine" && tokens.get(i + 1) == Some(&machine))
            || tokens[i] == "default";
        if is_match {
            let mut j = if tokens[i] == "default" { i + 1 } else { i + 2 };
            let (mut login, mut password) = (None, None);
            while j < tokens.len() && tokens[j] != "machine" && tokens[j] != "default" {
                match tokens[j] {
                    "login" | "user" => {
                        login = tokens.get(j + 1).map(|s| s.to_string());
                        j += 2;
                    }
                    "password" => {
                        password = tokens.get(j + 1).map(|s| s.to_string());
                        j += 2;
                    }
                    "account" | "port" => j += 2,
                    "macdef" => break,
                    _ => j += 1,
                }
            }
            if let (Some(login), Some(password)) = (login, password) {
                return Some(NetrcAuth { login, password });
            }
        }
        i += 1;
    }
    None
}

/// Read `~/.netrc` and return the auth for `machine`.
pub fn read_default(machine: &str) -> Result<NetrcAuth> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = std::path::Path::new(&home).join(".netrc");
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    find_machine(&contents, machine)
        .with_context(|| format!("no netrc entry for machine {machine}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_machine_entry() {
        let nc = "machine imap.example.com login me@example.com password s3cret\n";
        let a = find_machine(nc, "imap.example.com").unwrap();
        assert_eq!(a.login, "me@example.com");
        assert_eq!(a.password, "s3cret");
    }

    #[test]
    fn picks_the_right_machine_among_several() {
        let nc = "machine a.com login au password ap\n\
                  machine b.com login bu password bp\n";
        assert_eq!(find_machine(nc, "b.com").unwrap().login, "bu");
        assert!(find_machine(nc, "c.com").is_none());
    }

    #[test]
    fn multiline_and_extra_tokens() {
        let nc = "machine a.com\n  login au\n  password ap\n  account foo\n";
        let a = find_machine(nc, "a.com").unwrap();
        assert_eq!((a.login.as_str(), a.password.as_str()), ("au", "ap"));
    }
}
