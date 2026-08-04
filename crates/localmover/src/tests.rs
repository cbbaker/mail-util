use model::{FolderSpec, MoveAction};
use mover::{MoveOutcome, Mover};

use crate::LocalMover;

fn folderspec(dotpath: &str) -> FolderSpec {
    FolderSpec {
        dotpath: dotpath.to_string(),
        imap_name: String::new(),
        sieve_target: String::new(),
        exists: false,
    }
}

fn action(filename: &str, uid: u32, dst: &str) -> MoveAction {
    MoveAction {
        message_id: Some(format!("mid{uid}")),
        src_folder: ".INBOX".to_string(),
        uid: Some(uid),
        src_filename: filename.to_string(),
        dst_dotpath: dst.to_string(),
        dst_imap: String::new(),
        cluster_key: "k".to_string(),
    }
}

fn cur_files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir.join("cur"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn moves_locally_with_fresh_uidless_name() {
    let mc = mockcache::MockCache::new();
    let fname = mc.message(".INBOX").uid(10).from("a@b.com").flags("PS").write();
    let src_path = mc.root().join(".INBOX/cur").join(&fname);
    let original = std::fs::read(&src_path).unwrap();

    let mut m = LocalMover::new(mc.root());
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    let out = m.move_message(&action(&fname, 10, ".lists/.x")).unwrap();
    assert_eq!(out, MoveOutcome::Moved);

    // Source gone.
    assert!(!src_path.exists(), "source file should be removed");

    // Destination has exactly one file, with NO ,U= and the flags preserved.
    let dst = cur_files(&mc.root().join(".lists/.x"));
    assert_eq!(dst.len(), 1, "one file in destination: {dst:?}");
    let name = &dst[0];
    assert!(!name.contains(",U="), "moved file must not carry a ,U= UID: {name}");
    assert!(name.ends_with(":2,PS"), "flags preserved: {name}");

    // Bytes are byte-identical to the original.
    let moved = std::fs::read(mc.root().join(".lists/.x/cur").join(name)).unwrap();
    assert_eq!(moved, original, "message bytes must be preserved exactly");
}

#[test]
fn re_move_is_idempotent() {
    let mc = mockcache::MockCache::new();
    let fname = mc.message(".INBOX").uid(11).from("a@b.com").write();
    let mut m = LocalMover::new(mc.root());
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    let a = action(&fname, 11, ".lists/.x");
    assert_eq!(m.move_message(&a).unwrap(), MoveOutcome::Moved);
    // Second time the source is gone: no-op, not an error, no duplicate.
    assert_eq!(m.move_message(&a).unwrap(), MoveOutcome::AlreadyDone);
    assert_eq!(cur_files(&mc.root().join(".lists/.x")).len(), 1);
}

#[test]
fn conserves_messages_across_a_move() {
    let mc = mockcache::MockCache::new();
    let f1 = mc.message(".INBOX").uid(1).write();
    let f2 = mc.message(".INBOX").uid(2).write();
    let _keep = mc.message(".INBOX").uid(3).write();

    let count = |p: &std::path::Path| cur_files(p).len();
    let before = count(&mc.root().join(".INBOX"));
    assert_eq!(before, 3);

    let mut m = LocalMover::new(mc.root());
    m.ensure_folder(&folderspec(".lists/.x")).unwrap();
    m.move_message(&action(&f1, 1, ".lists/.x")).unwrap();
    m.move_message(&action(&f2, 2, ".lists/.x")).unwrap();

    // Total conserved: 1 left in inbox, 2 in destination.
    assert_eq!(count(&mc.root().join(".INBOX")), 1);
    assert_eq!(count(&mc.root().join(".lists/.x")), 2);
}

#[test]
fn ensure_folder_creates_maildir_dirs() {
    let mc = mockcache::MockCache::new();
    let mut m = LocalMover::new(mc.root());
    m.ensure_folder(&folderspec(".vendors/.new-one")).unwrap();
    for sub in ["cur", "new", "tmp"] {
        assert!(mc.root().join(".vendors/.new-one").join(sub).is_dir(), "missing {sub}");
    }
}
