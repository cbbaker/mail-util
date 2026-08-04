//! Integration tests for `apply`: the dry-run NDJSON stream and the guards that keep
//! real mail untouched until the real movers land.

use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_mail-util");

fn build_mock() -> mockcache::MockCache {
    let mc = mockcache::MockCache::new();
    for uid in 1..=4u32 {
        mc.message(".INBOX")
            .uid(uid)
            .from("bounce@elixir-lang.groups.io")
            .list_id("elixir.groups.io")
            .write();
    }
    mc
}

/// Produce a plan file for the mock and return its path (kept alive by the returned temp).
fn plan_file(mc: &mockcache::MockCache) -> tempfile::NamedTempFile {
    let out = Command::new(BIN)
        .args(["--root", mc.root().to_str().unwrap(), "plan", "--min-count", "3"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), &out.stdout).unwrap();
    f
}

#[test]
fn dry_run_streams_ndjson_and_moves_nothing() {
    let mc = build_mock();
    let plan = plan_file(&mc);

    let out = Command::new(BIN)
        .args([
            "--root", mc.root().to_str().unwrap(),
            "apply", "--plan", plan.path().to_str().unwrap(), "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    // Every stdout line is a JSON journal record.
    let records: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect();

    assert_eq!(records.first().unwrap()["event"], "plan_loaded");
    assert_eq!(records.last().unwrap()["event"], "done");
    let simulated = records
        .iter()
        .filter(|r| r["event"] == "action_moved" && r["outcome"] == "simulated")
        .count();
    assert_eq!(simulated, 4, "all four actions simulated");
    let done = records.iter().find(|r| r["event"] == "done").unwrap();
    assert_eq!(done["simulated"], 4);
    assert_eq!(done["moved"], 0);

    // Nothing actually moved: the inbox still has its messages.
    assert_eq!(mc.root().join(".INBOX/cur").read_dir().unwrap().count(), 4);
    // No .lists/.elixir folder was created.
    assert!(!mc.root().join(".lists/.elixir").exists());
}

fn cur_names(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir.join("cur"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn local_apply_moves_files_and_strips_uid() {
    let mc = build_mock(); // 4 elixir list messages in .INBOX
    let root = mc.root().to_str().unwrap();

    // Plan targeting the offline local mover.
    let out = Command::new(BIN)
        .args(["--root", root, "plan", "--min-count", "3", "--mover", "local"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let plan = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(plan.path(), &out.stdout).unwrap();

    // Real apply — no IMAP host, no mbsync channel (reconcile just checks UIDVALIDITY).
    let out = Command::new(BIN)
        .args(["--root", root, "apply", "--plan", plan.path().to_str().unwrap(), "--yes"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let records: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let done = records.iter().find(|r| r["event"] == "done").unwrap();
    assert_eq!(done["moved"], 4);

    // The four messages left the inbox and landed in the new folder…
    assert_eq!(cur_names(&mc.root().join(".INBOX")).len(), 0);
    let dst = cur_names(&mc.root().join(".lists/.elixir"));
    assert_eq!(dst.len(), 4);
    // …each with a fresh, ,U=-less filename (the anti-corruption invariant).
    for name in &dst {
        assert!(!name.contains(",U="), "moved file must not carry a ,U= UID: {name}");
    }
}

#[test]
fn refuses_real_apply_without_yes() {
    let mc = build_mock();
    let plan = plan_file(&mc);
    let out = Command::new(BIN)
        .args([
            "--root", mc.root().to_str().unwrap(),
            "apply", "--plan", plan.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
}

#[test]
fn real_apply_requires_imap_host() {
    // With --yes but no server to talk to, a real IMAP apply must refuse rather than
    // guess — it never touches mail without an explicit target.
    let mc = build_mock();
    let plan = plan_file(&mc);
    let out = Command::new(BIN)
        .args([
            "--root", mc.root().to_str().unwrap(),
            "apply", "--plan", plan.path().to_str().unwrap(), "--yes",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--imap-host"));
}
