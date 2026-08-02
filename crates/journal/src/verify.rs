//! No-message-lost invariants, checked in tests and (later) around a real apply.
//!
//! These operate on the [`mover::FakeMover`]'s in-memory mailbox, which the real movers
//! must mirror: a move relocates a message, never duplicates or drops it.

use std::collections::HashMap;

use mover::FakeMover;

/// Message-ID conservation: the multiset of Message-IDs is identical before and after.
/// `before` is a snapshot from [`FakeMover::message_id_multiset`].
pub fn message_ids_conserved(before: &[Option<String>], after: &FakeMover) -> bool {
    before == after.message_id_multiset().as_slice()
}

/// Exactly-once placement: every Message-ID present appears in exactly one folder.
/// (Messages without a Message-ID are ignored here; they are covered by count/conservation.)
pub fn each_message_once(fm: &FakeMover) -> bool {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for msgs in fm.folders.values() {
        for m in msgs {
            if let Some(id) = &m.message_id {
                *counts.entry(id.as_str()).or_insert(0) += 1;
            }
        }
    }
    counts.values().all(|&c| c == 1)
}

/// Convenience: run the two structural invariants together.
pub fn no_loss(before: &[Option<String>], after: &FakeMover) -> bool {
    message_ids_conserved(before, after) && each_message_once(after)
}
