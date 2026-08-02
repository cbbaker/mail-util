//! `journal` — the crash-safe apply engine and its no-loss invariants.
//!
//! - [`record`]: the append-only journal (records, sinks, replay).
//! - [`engine`]: [`engine::apply_plan`] drives a [`mover::Mover`] over a plan, journaling
//!   each step so an interrupted run resumes idempotently.
//! - [`verify`]: the message-conservation / exactly-once invariants.

pub mod engine;
pub mod record;
pub mod verify;

pub use engine::{apply_plan, ApplyReport};
pub use record::{completed_actions, read_journal, JournalFile, JournalRecord, RecordSink, Tee, VecSink};

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use model::{FolderSpec, MoveAction, MoverKind, Plan, Precheck};
    use mover::{DryRunMover, FakeMover, FakeMsg};

    use super::*;

    fn folderspec(dotpath: &str) -> FolderSpec {
        FolderSpec {
            dotpath: dotpath.to_string(),
            imap_name: dotpath.trim_start_matches('.').replace("/.", "."),
            sieve_target: dotpath.trim_start_matches('.').replace("/.", "."),
            exists: false,
        }
    }

    fn action(mid: &str, uid: u32, dst: &str) -> MoveAction {
        MoveAction {
            message_id: Some(mid.to_string()),
            src_folder: ".INBOX".to_string(),
            uid: Some(uid),
            src_filename: format!("{uid}.host,U={uid}:2,S"),
            dst_dotpath: dst.to_string(),
            dst_imap: dst.trim_start_matches('.').replace("/.", "."),
            cluster_key: "k".to_string(),
        }
    }

    fn plan(actions: Vec<MoveAction>, folders: Vec<FolderSpec>) -> Plan {
        Plan {
            plan_id: "plan-test".to_string(),
            account_root: "/tmp/x".to_string(),
            inbox: ".INBOX".to_string(),
            mover: MoverKind::Imap,
            separator: '.',
            folders_to_create: folders,
            actions,
            sieve_rules: vec![],
            sieve_text: String::new(),
            precheck: Precheck {
                universe_folders: 0,
                total_messages: 0,
                distinct_message_ids: 0,
                actions: 0,
                actions_missing_message_id: 0,
                actions_unresolved: 0,
            },
        }
    }

    fn inbox(ids: &[(&str, u32)]) -> BTreeMap<String, Vec<FakeMsg>> {
        BTreeMap::from([(
            ".INBOX".to_string(),
            ids.iter().map(|(m, u)| FakeMsg::new(Some(m), Some(*u))).collect(),
        )])
    }

    #[test]
    fn happy_path_moves_and_conserves() {
        let p = plan(
            vec![action("a", 1, ".lists/.x"), action("b", 2, ".lists/.x")],
            vec![folderspec(".lists/.x")],
        );
        let mut fm = FakeMover::new(inbox(&[("a", 1), ("b", 2), ("c", 3)]));
        let before = fm.message_id_multiset();
        let mut sink = VecSink::default();

        let report = apply_plan(&p, &mut fm, &mut sink, &HashSet::new()).unwrap();
        assert!(report.ok());
        assert_eq!(report.moved, 2);
        assert_eq!(report.folders_created, 1);

        assert!(verify::no_loss(&before, &fm));
        assert_eq!(fm.folders[".lists/.x"].len(), 2);
        assert_eq!(fm.folders[".INBOX"].len(), 1);
        assert!(fm.reconciled);

        // Journal ends with Reconciled + Done and records each action's lifecycle.
        assert!(matches!(sink.0.last(), Some(JournalRecord::Done { moved: 2, .. })));
        assert!(sink.0.iter().any(|r| matches!(r, JournalRecord::ActionVerified { index: 0 })));
    }

    #[test]
    fn dry_run_simulates_without_moving() {
        let p = plan(vec![action("a", 1, ".x"), action("b", 2, ".x")], vec![folderspec(".x")]);
        let mut mover = DryRunMover;
        let mut sink = VecSink::default();
        let report = apply_plan(&p, &mut mover, &mut sink, &HashSet::new()).unwrap();
        assert_eq!(report.simulated, 2);
        assert_eq!(report.moved, 0);
        assert!(report.ok());
        assert!(sink
            .0
            .iter()
            .any(|r| matches!(r, JournalRecord::ActionMoved { outcome, .. } if outcome == "simulated")));
    }

    #[test]
    fn crash_then_resume_loses_nothing() {
        let p = plan(
            vec![
                action("a", 1, ".x"),
                action("b", 2, ".x"),
                action("c", 3, ".x"),
                action("d", 4, ".x"),
            ],
            vec![folderspec(".x")],
        );
        let mut fm = FakeMover::new(inbox(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)])).failing_after(2);
        let before = fm.message_id_multiset();

        // First run crashes after 2 moves.
        let mut sink1 = VecSink::default();
        let r1 = apply_plan(&p, &mut fm, &mut sink1, &HashSet::new()).unwrap();
        assert!(!r1.ok());
        assert_eq!(r1.moved, 2);
        assert!(r1.fatal.is_some());

        let done = completed_actions(&sink1.0);
        assert_eq!(done, HashSet::from([0, 1]));

        // Resume: no more failures, skip the done ones.
        fm.fail_after = None;
        let mut sink2 = VecSink::default();
        let r2 = apply_plan(&p, &mut fm, &mut sink2, &done).unwrap();
        assert!(r2.ok());
        assert_eq!(r2.skipped, 2);
        assert_eq!(r2.moved, 2);

        // Nothing lost, nothing duplicated, everything filed.
        assert!(verify::no_loss(&before, &fm));
        assert_eq!(fm.folders[".x"].len(), 4);
        assert_eq!(fm.folders[".INBOX"].len(), 0);
    }

    #[test]
    fn rerun_without_resume_set_is_still_idempotent() {
        // Even if the resume set is lost, re-running the whole plan must not duplicate:
        // the mover reports AlreadyDone for messages no longer in the source.
        let p = plan(vec![action("a", 1, ".x"), action("b", 2, ".x")], vec![folderspec(".x")]);
        let mut fm = FakeMover::new(inbox(&[("a", 1), ("b", 2)]));
        let before = fm.message_id_multiset();

        apply_plan(&p, &mut fm, &mut VecSink::default(), &HashSet::new()).unwrap();
        let r2 = apply_plan(&p, &mut fm, &mut VecSink::default(), &HashSet::new()).unwrap();

        assert_eq!(r2.skipped, 2, "second run finds everything already done");
        assert_eq!(r2.moved, 0);
        assert!(verify::no_loss(&before, &fm));
        assert!(verify::each_message_once(&fm));
        assert_eq!(fm.total_messages(), 2);
    }
}
