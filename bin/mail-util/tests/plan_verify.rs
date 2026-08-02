//! End-to-end tests for the `plan` and `verify` subcommands, driving the compiled
//! binary against a mock Maildir. Everything stays read-only.

use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_mail-util");

/// Run the binary and return parsed-JSON stdout (asserting success).
fn run_json(root: &std::path::Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("spawn mail-util");
    assert!(
        out.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("parse stdout JSON")
}

fn build_mock() -> mockcache::MockCache {
    let mc = mockcache::MockCache::new();
    // A mailing list large enough to surface.
    for uid in 1..=4u32 {
        mc.message(".INBOX")
            .uid(uid)
            .from("bounce@elixir-lang.groups.io")
            .list_id("elixir.groups.io")
            .subject(&format!("topic {uid}"))
            .write();
    }
    // Noise below threshold.
    mc.message(".INBOX").uid(99).from("random@nowhere.example").write();
    mc
}

#[test]
fn plan_builds_folders_actions_and_sieve() {
    let mc = build_mock();
    let plan = run_json(mc.root(), &["plan", "--min-count", "3"]);

    assert_eq!(plan["mover"], "imap");
    assert_eq!(plan["separator"], ".");

    let folders = plan["folders_to_create"].as_array().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0]["dotpath"], ".lists/.elixir");
    assert_eq!(folders[0]["imap_name"], "lists.elixir");
    assert_eq!(folders[0]["sieve_target"], "lists.elixir");
    assert_eq!(folders[0]["exists"], false);

    let actions = plan["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 4, "one action per list message");
    for a in actions {
        assert_eq!(a["dst_dotpath"], ".lists/.elixir");
        assert_eq!(a["dst_imap"], "lists.elixir");
        assert_eq!(a["src_folder"], ".INBOX");
        assert!(a["uid"].is_number());
    }

    assert!(plan["sieve_text"]
        .as_str()
        .unwrap()
        .contains(r#"fileinto "lists.elixir";"#));
    assert!(plan["sieve_text"].as_str().unwrap().contains("List-Id"));

    let pc = &plan["precheck"];
    assert_eq!(pc["actions"], 4);
    assert_eq!(pc["actions_unresolved"], 0);
    assert_eq!(pc["total_messages"], 5);
}

#[test]
fn verify_confirms_a_fresh_plan() {
    let mc = build_mock();
    let plan = run_json(mc.root(), &["plan", "--min-count", "3"]);

    let plan_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(plan_file.path(), serde_json::to_string(&plan).unwrap()).unwrap();

    let verify = run_json(
        mc.root(),
        &["verify", "--plan", plan_file.path().to_str().unwrap()],
    );
    assert_eq!(verify["actions"], 4);
    assert_eq!(verify["resolved"], 4);
    assert_eq!(verify["unresolved"], 0);
    assert_eq!(verify["actions_into_excluded"], 0);
    assert_eq!(verify["ok"], true);
}

#[test]
fn approved_filter_restricts_to_selected_clusters() {
    let mc = mockcache::MockCache::new();
    for uid in 1..=4u32 {
        mc.message(".INBOX").uid(uid).from("b@l.com").list_id("elixir.groups.io").write();
    }
    for uid in 10..=14u32 {
        mc.message(".INBOX").uid(uid).from(&format!("n{uid}@github.com")).write();
    }

    // Approve only the github (sender-domain) cluster.
    let approved = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        approved.path(),
        r#"{"approved":[{"key":"github.com"}]}"#,
    )
    .unwrap();

    let plan = run_json(
        mc.root(),
        &["plan", "--min-count", "3", "--approved", approved.path().to_str().unwrap()],
    );
    let folders = plan["folders_to_create"].as_array().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0]["dotpath"], ".vendors/.github");
    // Only the 5 github messages become actions; the elixir cluster is excluded.
    assert_eq!(plan["actions"].as_array().unwrap().len(), 5);
}
