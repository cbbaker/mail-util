//! The append-only journal: one JSON record per line, each fsync'd before the mutation
//! it announces, so an interrupted `apply` can be replayed and resumed without loss.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One journal event. `index` refers to the position of a [`model::MoveAction`] in the
/// plan's `actions` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JournalRecord {
    PlanLoaded { plan_id: String, actions: usize },
    FolderEnsured { dotpath: String },
    /// Written and fsync'd *before* the move is attempted.
    ActionBegin {
        index: usize,
        message_id: Option<String>,
        src_folder: String,
        uid: Option<u32>,
        dst: String,
    },
    ActionMoved { index: usize, outcome: String },
    ActionVerified { index: usize },
    ActionSkipped { index: usize, reason: String },
    ActionFailed { index: usize, error: String },
    Reconciled,
    Done {
        moved: usize,
        skipped: usize,
        simulated: usize,
        failed: usize,
    },
    Fatal { error: String },
}

/// A place journal records are written. The engine emits to a sink; the CLI tees a
/// durable file and an NDJSON stdout stream.
pub trait RecordSink {
    fn emit(&mut self, rec: &JournalRecord) -> Result<()>;
}

/// A durable append-only journal file. Each record is flushed and `fsync`'d.
pub struct JournalFile {
    file: File,
}

impl JournalFile {
    /// Open (creating if needed) a journal file for appending.
    pub fn append(path: &Path) -> Result<JournalFile> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening journal {}", path.display()))?;
        Ok(JournalFile { file })
    }
}

impl RecordSink for JournalFile {
    fn emit(&mut self, rec: &JournalRecord) -> Result<()> {
        let line = serde_json::to_string(rec)?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        // Durability: the record must hit disk before the mutation it precedes.
        self.file.sync_data()?;
        Ok(())
    }
}

/// Collects records in memory — for tests.
#[derive(Debug, Default)]
pub struct VecSink(pub Vec<JournalRecord>);

impl RecordSink for VecSink {
    fn emit(&mut self, rec: &JournalRecord) -> Result<()> {
        self.0.push(rec.clone());
        Ok(())
    }
}

/// Fans a record out to several sinks (e.g. durable file + NDJSON stdout).
#[derive(Default)]
pub struct Tee {
    pub sinks: Vec<Box<dyn RecordSink>>,
}

impl Tee {
    pub fn new(sinks: Vec<Box<dyn RecordSink>>) -> Tee {
        Tee { sinks }
    }
}

impl RecordSink for Tee {
    fn emit(&mut self, rec: &JournalRecord) -> Result<()> {
        for s in &mut self.sinks {
            s.emit(rec)?;
        }
        Ok(())
    }
}

/// Read and parse a journal file into its records (skipping blank lines).
pub fn read_journal(path: &Path) -> Result<Vec<JournalRecord>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading journal {}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parsing journal line: {l}")))
        .collect()
}

/// Indices of actions that completed safely (reached `ActionVerified` or were skipped),
/// so a resumed apply can leave them alone.
pub fn completed_actions(records: &[JournalRecord]) -> std::collections::HashSet<usize> {
    records
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ActionVerified { index }
            | JournalRecord::ActionSkipped { index, .. } => Some(*index),
            _ => None,
        })
        .collect()
}
