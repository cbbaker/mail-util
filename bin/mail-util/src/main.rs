//! `mail-util` CLI. Read-only subcommands (`scan`, `suggest`) emit JSON on stdout so
//! Emacs (and humans via `jq`) can drive the tool; human-readable logs go to stderr.
//!
//! Mutating subcommands (plan/apply/…) arrive in later milestones.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mailcache::Account;
use model::Config;
use serde::Serialize;
use suggest::ExistingFolder;

#[derive(Parser)]
#[command(name = "mail-util", version, about = "Suggest and apply mail-sorting folders/sieve rules, locally.")]
struct Cli {
    /// Account root: the Maildir directory whose subdirectories are the folders
    /// (`.INBOX`, `.lists/…`, …) — i.e. the mbsync Slave path for the account.
    /// May also be supplied via the MAILUTIL_ROOT environment variable.
    #[arg(long, global = true, env = "MAILUTIL_ROOT")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Enumerate folders and message counts (read-only).
    Scan {
        /// Restrict to a single folder dotpath (e.g. .INBOX); otherwise summarize all.
        #[arg(long)]
        folder: Option<String>,
    },
    /// Cluster the inbox and print ranked folder/rule suggestions (read-only).
    Suggest {
        /// Inbox folder to analyze, as a path relative to the account root.
        /// Defaults to the Maildir++ convention `.INBOX`.
        #[arg(long, default_value = ".INBOX")]
        inbox: String,
        /// Minimum messages for a cluster to be surfaced.
        #[arg(long)]
        min_count: Option<usize>,
    },
}

fn account_root(cli: &Cli) -> Result<PathBuf> {
    let root = cli.root.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no account root given: pass --root <maildir-dir> or set MAILUTIL_ROOT \
             (the Maildir directory whose subdirectories are the folders)"
        )
    })?;
    if !root.is_dir() {
        anyhow::bail!("account root is not a directory: {}", root.display());
    }
    Ok(root)
}

#[derive(Serialize)]
struct FolderSummary {
    dotpath: String,
    messages: usize,
}

#[derive(Serialize)]
struct ScanOutput {
    root: String,
    folder_count: usize,
    total_messages: usize,
    folders: Vec<FolderSummary>,
}

#[derive(Serialize)]
struct SuggestOutput {
    root: String,
    inbox: String,
    inbox_messages: usize,
    clustered_messages: usize,
    clusters: Vec<model::Cluster>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = account_root(&cli)?;
    let account = Account::new(&root);

    match &cli.command {
        Command::Scan { folder } => {
            let folders = account.folders();
            let selected: Vec<_> = match folder {
                Some(f) => folders.into_iter().filter(|fr| &fr.dotpath == f).collect(),
                None => folders,
            };
            let summaries: Vec<FolderSummary> = selected
                .iter()
                .map(|f| FolderSummary {
                    dotpath: f.dotpath.clone(),
                    messages: f.message_count(),
                })
                .collect();
            let out = ScanOutput {
                root: root.display().to_string(),
                folder_count: summaries.len(),
                total_messages: summaries.iter().map(|s| s.messages).sum(),
                folders: summaries,
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Command::Suggest { inbox, min_count } => {
            let mut config = Config::default();
            if let Some(mc) = min_count {
                config.min_count = *mc;
            }
            let all_folders = account.folders();
            let inbox_folder = account.folder(inbox);
            eprintln!("scanning {} …", inbox_folder.dir.display());
            let messages = inbox_folder.messages();

            // Existing taxonomy = every folder except the inbox and excluded ones.
            let existing: Vec<ExistingFolder> = all_folders
                .iter()
                .filter(|f| &f.dotpath != inbox && !config.excluded_folders.contains(&f.dotpath))
                .map(|f| ExistingFolder::new(&f.dotpath))
                .collect();

            let clusters = suggest::suggest(&messages, &existing, &config);
            let clustered_messages = clusters.iter().map(|c| c.count).sum();
            let out = SuggestOutput {
                root: root.display().to_string(),
                inbox: inbox.clone(),
                inbox_messages: messages.len(),
                clustered_messages,
                clusters,
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
    }
    Ok(())
}
