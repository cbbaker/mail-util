//! `mail-util` CLI.
//!
//! Read-only subcommands emit JSON on stdout so Emacs (and humans via `jq`) can drive the
//! tool; human-readable logs go to stderr.
//!
//! - `scan` / `suggest`: inventory and ranked suggestions.
//! - `plan`: build a complete, reviewable sorting plan (folders + per-message move actions
//!   + sieve rules). Pure — mutates nothing.
//! - `verify`: re-check a plan against the current cache (read-only preflight).
//!
//! Mutating subcommands (`apply`, …) arrive in later milestones.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use mailcache::{Account, FolderRef};
use model::{
    Cluster, Config, FolderSpec, MoveAction, MoverKind, Plan, Precheck, SieveRule,
};
use namemap::NameMap;
use serde::{Deserialize, Serialize};
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
        #[arg(long, default_value = ".INBOX")]
        inbox: String,
        /// Minimum messages for a cluster to be surfaced.
        #[arg(long)]
        min_count: Option<usize>,
    },
    /// Build a complete sorting plan as JSON (pure; mutates nothing).
    Plan {
        #[arg(long, default_value = ".INBOX")]
        inbox: String,
        #[arg(long)]
        min_count: Option<usize>,
        /// Which mover the plan targets (recorded for `apply`).
        #[arg(long, value_enum, default_value_t = MoverArg::Imap)]
        mover: MoverArg,
        /// IMAP hierarchy separator for name mapping (probed against the server in a
        /// later milestone; assumed here).
        #[arg(long, default_value_t = '.')]
        separator: char,
        /// Restrict the plan to clusters approved in this JSON file (the Emacs export).
        #[arg(long)]
        approved: Option<PathBuf>,
    },
    /// Re-check a plan against the current cache (read-only preflight).
    Verify {
        /// Path to a plan JSON file produced by `plan`.
        #[arg(long)]
        plan: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum MoverArg {
    Imap,
    Local,
}

impl From<MoverArg> for MoverKind {
    fn from(m: MoverArg) -> Self {
        match m {
            MoverArg::Imap => MoverKind::Imap,
            MoverArg::Local => MoverKind::Local,
        }
    }
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

fn build_config(min_count: Option<usize>) -> Config {
    let mut config = Config::default();
    if let Some(mc) = min_count {
        config.min_count = mc;
    }
    config
}

/// Existing taxonomy = every folder except the inbox and excluded ones.
fn existing_folders(all: &[FolderRef], inbox: &str, config: &Config) -> Vec<ExistingFolder> {
    all.iter()
        .filter(|f| f.dotpath != inbox && !config.excluded_folders.contains(&f.dotpath))
        .map(|f| ExistingFolder::new(&f.dotpath))
        .collect()
}

// ---- JSON output shapes for the read-only inventory commands ----

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
    clusters: Vec<Cluster>,
}

/// The Emacs approved-clusters export; we only need the keys.
#[derive(Deserialize)]
struct ApprovedExport {
    approved: Vec<ApprovedItem>,
}
#[derive(Deserialize)]
struct ApprovedItem {
    key: String,
}

fn read_approved_keys(path: &PathBuf) -> Result<HashSet<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading approved file {}", path.display()))?;
    let export: ApprovedExport =
        serde_json::from_str(&text).context("parsing approved-clusters JSON")?;
    Ok(export.approved.into_iter().map(|a| a.key).collect())
}

fn plan_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("plan-{millis}")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = account_root(&cli)?;
    let account = Account::new(&root);

    match &cli.command {
        Command::Scan { folder } => cmd_scan(&account, &root, folder.as_deref()),
        Command::Suggest { inbox, min_count } => cmd_suggest(&account, &root, inbox, *min_count),
        Command::Plan {
            inbox,
            min_count,
            mover,
            separator,
            approved,
        } => cmd_plan(&account, &root, inbox, *min_count, (*mover).into(), *separator, approved.as_ref()),
        Command::Verify { plan } => cmd_verify(&account, plan),
    }
}

fn cmd_scan(account: &Account, root: &PathBuf, folder: Option<&str>) -> Result<()> {
    let folders = account.folders();
    let selected: Vec<_> = match folder {
        Some(f) => folders.into_iter().filter(|fr| fr.dotpath == f).collect(),
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
    Ok(())
}

fn cmd_suggest(account: &Account, root: &PathBuf, inbox: &str, min_count: Option<usize>) -> Result<()> {
    let config = build_config(min_count);
    let all = account.folders();
    let inbox_folder = account.folder(inbox);
    eprintln!("scanning {} …", inbox_folder.dir.display());
    let messages = inbox_folder.messages();
    let existing = existing_folders(&all, inbox, &config);
    let clusters = suggest::suggest(&messages, &existing, &config);
    let clustered_messages = clusters.iter().map(|c| c.count).sum();
    let out = SuggestOutput {
        root: root.display().to_string(),
        inbox: inbox.to_string(),
        inbox_messages: messages.len(),
        clustered_messages,
        clusters,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_plan(
    account: &Account,
    root: &PathBuf,
    inbox: &str,
    min_count: Option<usize>,
    mover: MoverKind,
    separator: char,
    approved: Option<&PathBuf>,
) -> Result<()> {
    let config = build_config(min_count);
    let all = account.folders();
    let inbox_folder = account.folder(inbox);
    eprintln!("scanning {} …", inbox_folder.dir.display());
    let messages = inbox_folder.messages();
    let existing = existing_folders(&all, inbox, &config);
    let clusters = suggest::suggest(&messages, &existing, &config);

    // Optionally restrict to approved cluster keys (from the Emacs export).
    let approved_keys = match approved {
        Some(p) => Some(read_approved_keys(p)?),
        None => None,
    };
    let selected: Vec<&Cluster> = clusters
        .iter()
        .filter(|c| approved_keys.as_ref().is_none_or(|s| s.contains(&c.key)))
        .collect();

    let nm = NameMap::new(separator);

    // Index selected clusters by their classification key so each message can find its
    // cluster (this covers uid-less messages, unlike the cluster's `uids` list).
    let mut by_key: HashMap<(model::Signal, String), &Cluster> = HashMap::new();
    for c in &selected {
        by_key.insert((c.signal, c.key.clone()), c);
    }

    // Folders to create: the *new* destinations of selected clusters, deduped.
    let mut folders_to_create: Vec<FolderSpec> = Vec::new();
    let mut seen_dotpaths: HashSet<String> = HashSet::new();
    for c in &selected {
        let dotpath = c.destination.dotpath().to_string();
        let exists = matches!(c.destination, model::Destination::Existing { .. });
        if !exists && seen_dotpaths.insert(dotpath.clone()) {
            folders_to_create.push(FolderSpec {
                imap_name: nm.dotpath_to_imap(&dotpath),
                sieve_target: nm.dotpath_to_sieve(&dotpath),
                exists,
                dotpath,
            });
        }
    }

    // Per-message move actions, in the scan's deterministic order.
    let mut actions: Vec<MoveAction> = Vec::new();
    for m in &messages {
        let Some((signal, key)) = suggest::classify(m) else {
            continue;
        };
        if let Some(c) = by_key.get(&(signal, key.clone())) {
            let dotpath = c.destination.dotpath().to_string();
            actions.push(MoveAction {
                message_id: m.message_id.clone(),
                src_folder: inbox.to_string(),
                uid: m.uid,
                src_filename: m.filename.clone(),
                dst_imap: nm.dotpath_to_imap(&dotpath),
                dst_dotpath: dotpath,
                cluster_key: c.key.clone(),
            });
        }
    }

    // Sieve rules: one per selected cluster.
    let sieve_rules: Vec<SieveRule> = selected
        .iter()
        .map(|c| sieve::rule_for_cluster(c, &nm.dotpath_to_sieve(c.destination.dotpath())))
        .collect();
    let sieve_text = sieve::render_script(&sieve_rules);

    // Precheck over the source (inbox) scope.
    let mut dest_dotpaths: HashSet<&str> =
        selected.iter().map(|c| c.destination.dotpath()).collect();
    dest_dotpaths.insert(inbox);
    let distinct_message_ids = messages
        .iter()
        .filter_map(|m| m.message_id.as_deref())
        .collect::<HashSet<_>>()
        .len();
    let precheck = Precheck {
        universe_folders: dest_dotpaths.len(),
        total_messages: messages.len(),
        distinct_message_ids,
        actions: actions.len(),
        actions_missing_message_id: actions.iter().filter(|a| a.message_id.is_none()).count(),
        actions_unresolved: 0, // built from freshly scanned messages
    };

    let plan = Plan {
        plan_id: plan_id(),
        account_root: root.display().to_string(),
        inbox: inbox.to_string(),
        mover,
        separator,
        folders_to_create,
        actions,
        sieve_rules,
        sieve_text,
        precheck,
    };
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

#[derive(Serialize)]
struct VerifyOutput {
    plan_id: String,
    actions: usize,
    resolved: usize,
    unresolved: usize,
    distinct_message_ids: usize,
    /// Actions whose destination is a configured excluded (spam/trash) folder — should be 0.
    actions_into_excluded: usize,
    ok: bool,
}

fn cmd_verify(account: &Account, plan_path: &PathBuf) -> Result<()> {
    let text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("reading plan {}", plan_path.display()))?;
    let plan: Plan = serde_json::from_str(&text).context("parsing plan JSON")?;
    let config = Config::default();

    // Scan every source folder referenced by the actions and index messages by uid and
    // by filename so each action can be resolved against current reality.
    let src_folders: HashSet<&str> = plan.actions.iter().map(|a| a.src_folder.as_str()).collect();
    let mut by_uid: HashMap<(String, u32), Option<String>> = HashMap::new();
    let mut by_name: HashMap<(String, String), Option<String>> = HashMap::new();
    for folder in &src_folders {
        for m in account.folder(folder).messages() {
            if let Some(uid) = m.uid {
                by_uid.insert((folder.to_string(), uid), m.message_id.clone());
            }
            by_name.insert((folder.to_string(), m.filename.clone()), m.message_id.clone());
        }
    }

    let mut resolved = 0usize;
    let mut resolved_message_ids: HashSet<String> = HashSet::new();
    for a in &plan.actions {
        let mid = a
            .uid
            .and_then(|u| by_uid.get(&(a.src_folder.clone(), u)).cloned())
            .or_else(|| by_name.get(&(a.src_folder.clone(), a.src_filename.clone())).cloned());
        if let Some(mid) = mid {
            resolved += 1;
            if let Some(id) = mid {
                resolved_message_ids.insert(id);
            }
        }
    }

    let actions_into_excluded = plan
        .actions
        .iter()
        .filter(|a| config.excluded_folders.contains(&a.dst_dotpath))
        .count();

    let unresolved = plan.actions.len() - resolved;
    let out = VerifyOutput {
        plan_id: plan.plan_id.clone(),
        actions: plan.actions.len(),
        resolved,
        unresolved,
        distinct_message_ids: resolved_message_ids.len(),
        actions_into_excluded,
        ok: unresolved == 0 && actions_into_excluded == 0,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
