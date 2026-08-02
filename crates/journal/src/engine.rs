//! The apply engine: drive a [`Mover`] over a [`Plan`]'s actions, journaling every step
//! so an interrupted run can be resumed idempotently.
//!
//! Safety ordering per action: emit (and fsync) `ActionBegin` **before** attempting the
//! move, then `ActionMoved` + `ActionVerified` after it succeeds. A mover error is
//! recorded (`ActionFailed` + `Fatal`) and stops the run fail-closed; the partial journal
//! is enough to resume, and the mover's own idempotency (`AlreadyDone`) prevents dupes.

use std::collections::HashSet;

use anyhow::Result;
use model::Plan;
use mover::{MoveOutcome, Mover};

use crate::record::{JournalRecord, RecordSink};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub folders_created: usize,
    pub moved: usize,
    pub skipped: usize,
    pub simulated: usize,
    pub failed: usize,
    /// Set when the run stopped early due to a mover error.
    pub fatal: Option<String>,
}

impl ApplyReport {
    pub fn ok(&self) -> bool {
        self.fatal.is_none() && self.failed == 0
    }
}

/// Apply `plan` using `mover`, emitting journal records to `sink`. Actions whose index is
/// in `already_done` (from replaying a prior journal) are skipped, making resume safe.
pub fn apply_plan(
    plan: &Plan,
    mover: &mut dyn Mover,
    sink: &mut dyn RecordSink,
    already_done: &HashSet<usize>,
) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();

    sink.emit(&JournalRecord::PlanLoaded {
        plan_id: plan.plan_id.clone(),
        actions: plan.actions.len(),
    })?;

    for spec in &plan.folders_to_create {
        mover.ensure_folder(spec)?;
        report.folders_created += 1;
        sink.emit(&JournalRecord::FolderEnsured {
            dotpath: spec.dotpath.clone(),
        })?;
    }

    for (index, action) in plan.actions.iter().enumerate() {
        if already_done.contains(&index) {
            report.skipped += 1;
            sink.emit(&JournalRecord::ActionSkipped {
                index,
                reason: "already done (resume)".to_string(),
            })?;
            continue;
        }

        // Announce intent and make it durable *before* mutating anything.
        sink.emit(&JournalRecord::ActionBegin {
            index,
            message_id: action.message_id.clone(),
            src_folder: action.src_folder.clone(),
            uid: action.uid,
            dst: action.dst_dotpath.clone(),
        })?;

        match mover.move_message(action) {
            Ok(MoveOutcome::Moved) => {
                report.moved += 1;
                sink.emit(&JournalRecord::ActionMoved {
                    index,
                    outcome: "moved".to_string(),
                })?;
                sink.emit(&JournalRecord::ActionVerified { index })?;
            }
            Ok(MoveOutcome::Simulated) => {
                report.simulated += 1;
                sink.emit(&JournalRecord::ActionMoved {
                    index,
                    outcome: "simulated".to_string(),
                })?;
            }
            Ok(MoveOutcome::AlreadyDone) => {
                report.skipped += 1;
                sink.emit(&JournalRecord::ActionSkipped {
                    index,
                    reason: "already at destination".to_string(),
                })?;
            }
            Err(e) => {
                report.failed += 1;
                let error = e.to_string();
                sink.emit(&JournalRecord::ActionFailed {
                    index,
                    error: error.clone(),
                })?;
                sink.emit(&JournalRecord::Fatal { error: error.clone() })?;
                report.fatal = Some(error);
                return Ok(report);
            }
        }
    }

    mover.reconcile()?;
    sink.emit(&JournalRecord::Reconciled)?;
    sink.emit(&JournalRecord::Done {
        moved: report.moved,
        skipped: report.skipped,
        simulated: report.simulated,
        failed: report.failed,
    })?;
    Ok(report)
}
