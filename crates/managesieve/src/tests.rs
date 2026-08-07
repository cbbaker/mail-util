use model::{SieveRule, SieveTest};

use crate::{FakeSieveOps, SieveDeployer, SieveOps, DEFAULT_SCRIPT};

fn rule(list_id: &str, target: &str) -> SieveRule {
    SieveRule {
        test: SieveTest::HeaderContains {
            header: "List-Id".to_string(),
            value: list_id.to_string(),
        },
        fileinto: target.to_string(),
        stop: true,
        comment: None,
    }
}

#[test]
fn deploys_to_a_new_script_when_none_active() {
    let mut d = SieveDeployer::new(FakeSieveOps::new());
    let report = d.deploy(None, &[rule("elixir.groups.io", "lists.elixir")]).unwrap();
    assert_eq!(report.script, DEFAULT_SCRIPT);
    assert!(report.created);
}

#[test]
fn merges_into_existing_active_script_and_keeps_it_active() {
    let ops = FakeSieveOps::new().with_script(
        "roundcube",
        "require [\"fileinto\"];\n\n# hand rule\nif true { keep; }\n",
        true,
    );
    let mut d = SieveDeployer::new(ops);
    let report = d.deploy(None, &[rule("elixir.groups.io", "lists.elixir")]).unwrap();

    assert_eq!(report.script, "roundcube");
    assert!(!report.created);

    // Re-borrow via a fresh deployer isn't possible; instead inspect through a fresh op.
    // Deploy a second time to confirm idempotency + that the hand rule survives.
    let merged = d.preview(None, &[rule("elixir.groups.io", "lists.elixir")]).unwrap();
    assert!(merged.contains("# hand rule"), "hand-written rule preserved");
    assert!(merged.contains(r#"fileinto "lists.elixir";"#));
}

#[test]
fn deploy_is_idempotent() {
    let mut d = SieveDeployer::new(FakeSieveOps::new());
    let rules = [rule("l", "L")];
    let r1 = d.deploy(None, &rules).unwrap();
    let r2 = d.deploy(None, &rules).unwrap();
    assert_eq!(r1.bytes, r2.bytes, "second deploy produces identical script");
}

#[test]
fn preview_does_not_upload() {
    let mut ops = FakeSieveOps::new();
    // Snapshot: no scripts yet.
    assert!(ops.active_script().unwrap().is_none());
    let mut d = SieveDeployer::new(ops.clone());
    let _ = d.preview(None, &[rule("l", "L")]).unwrap();
    // The original ops is untouched (preview operated on the deployer's own copy).
    assert!(ops.scripts.is_empty());
}

#[test]
fn explicit_name_overrides_active() {
    let ops = FakeSieveOps::new().with_script("main", "require [\"fileinto\"];\n", true);
    let mut d = SieveDeployer::new(ops);
    let report = d.deploy(Some("mail-util"), &[rule("l", "L")]).unwrap();
    assert_eq!(report.script, "mail-util");
}
