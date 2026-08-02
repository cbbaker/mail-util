use std::collections::BTreeMap;

use model::{FolderSpec, MoveAction};
use mover::{MoveOutcome, Mover};

use crate::{FakeImapOps, ImapMover};

fn folderspec(dotpath: &str) -> FolderSpec {
    FolderSpec {
        dotpath: dotpath.to_string(),
        imap_name: dotpath.trim_start_matches('.').replace("/.", "."),
        sieve_target: String::new(),
        exists: false,
    }
}

fn action(mid: &str, uid: Option<u32>, src: &str, dst: &str) -> MoveAction {
    MoveAction {
        message_id: Some(mid.to_string()),
        src_folder: src.to_string(),
        uid,
        src_filename: "f".to_string(),
        dst_dotpath: dst.to_string(),
        dst_imap: dst.trim_start_matches('.').replace("/.", "."),
        cluster_key: "k".to_string(),
    }
}

fn server(supports_move: bool) -> FakeImapOps {
    FakeImapOps::new(BTreeMap::from([("INBOX".to_string(), vec![1, 2, 3])]), supports_move)
}

#[test]
fn moves_via_uid_move_when_supported() {
    let mut m = ImapMover::new(server(true), '.');
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    let out = m.move_message(&action("a", Some(1), ".INBOX", ".lists/.x")).unwrap();
    assert_eq!(out, MoveOutcome::Moved);

    let ops = &m.ops;
    assert!(!ops.uids("INBOX").contains(&1), "uid gone from source");
    assert_eq!(ops.uids("lists.x").len(), 1, "one message in destination");
    assert!(ops.op_log.iter().any(|l| l == "UID MOVE 1 lists.x"));
    assert!(ops.created.contains(&"lists.x".to_string()));
}

#[test]
fn falls_back_to_copy_delete_expunge() {
    let mut m = ImapMover::new(server(false), '.');
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    m.move_message(&action("a", Some(2), ".INBOX", ".lists/.x")).unwrap();

    let ops = &m.ops;
    assert!(!ops.uids("INBOX").contains(&2));
    assert_eq!(ops.uids("lists.x").len(), 1);
    let log = ops.op_log.join("|");
    assert!(log.contains("UID COPY 2 lists.x"));
    assert!(log.contains("STORE 2 +FLAGS \\Deleted"));
    assert!(log.contains("UID EXPUNGE 2"));
}

#[test]
fn re_move_is_idempotent() {
    let mut m = ImapMover::new(server(true), '.');
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    let a = action("a", Some(1), ".INBOX", ".lists/.x");
    assert_eq!(m.move_message(&a).unwrap(), MoveOutcome::Moved);
    // Second time: uid is gone from source, so it's a no-op, not an error.
    assert_eq!(m.move_message(&a).unwrap(), MoveOutcome::AlreadyDone);
}

#[test]
fn uid_less_message_is_refused() {
    let mut m = ImapMover::new(server(true), '.');
    let err = m.move_message(&action("a", None, ".INBOX", ".lists/.x")).unwrap_err();
    assert!(err.to_string().contains("no IMAP UID"));
}

#[test]
fn reconciler_receives_touched_folders() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let touched: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = touched.clone();
    let mut m = ImapMover::new(server(true), '.').with_reconciler(move |t| {
        *sink.borrow_mut() = t.iter().cloned().collect();
        Ok(())
    });
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    m.move_message(&action("a", Some(1), ".INBOX", ".lists/.x")).unwrap();
    m.reconcile().unwrap();

    let t = touched.borrow();
    assert!(t.contains(&".INBOX".to_string()));
    assert!(t.contains(&".lists/.x".to_string()));
}
