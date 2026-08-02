# mail-util

A **local** tool that analyzes an mbsync-cached IMAP inbox, suggests new folders and
sieve rules for sorting, and (in later milestones) applies them by creating folders and
moving mail — without ever losing a message and without corrupting mbsync's sync state.
Nothing is ever uploaded to a third party; the optional labeling step uses a local
ollama instance only.

See `DESIGN.md` for the full design and the milestone roadmap.

## Status

**M1 (read-only vertical slice) — done.** Scans the Maildir, clusters the inbox, and
prints ranked folder/rule suggestions as JSON. Zero mutation.

Remaining milestones: M2 plan+sieve (pure), M3 journal/apply safety machinery, M4
server-side IMAP mover (default), M5 offline local mover, M6 Emacs UI + ManageSieve.

## Workspace layout

| crate | role |
|-------|------|
| `crates/model` | shared serde types (`Message`, `Cluster`, `Destination`, `Config`) |
| `crates/mailcache` | read-only Maildir access: native `,U=` filename parse/emit, `.uidvalidity`, streaming header extraction, folder scanning |
| `crates/mockcache` | test-only builder that writes a real temp Maildir tree (native filenames, nested folders, `.uidvalidity`) |
| `crates/suggest` | deterministic clustering (List-Id > sender-domain > person), scoring, taxonomy reuse, human-readable slug generation |
| `bin/mail-util` | CLI (`scan`, `suggest`) emitting JSON on stdout |

## Build & test

```sh
cargo test              # full suite, runs entirely on mockcache (no network/real mail)
cargo build --release
```

## Read-only usage

Point the tool at your account's Maildir root — the directory whose subdirectories are
the folders (`.INBOX`, `.lists/…`, …), i.e. the mbsync *Slave* path. Supply it with
`--root` or the `MAILUTIL_ROOT` environment variable; there is no built-in default.

```sh
export MAILUTIL_ROOT=~/Maildir/myaccount   # or pass --root each time

# Folder inventory + message counts.
mail-util scan
mail-util scan --folder .INBOX

# Ranked sorting suggestions for the inbox.
mail-util suggest --min-count 30 | jq '.clusters[] | {count, signal, destination}'
```

The inbox folder defaults to the Maildir++ convention `.INBOX`; override with
`--inbox <dotpath>` for other layouts.

## Emacs

`emacs/mail-util.el` provides a review UI over the `suggest` command. Load it and point
it at your binary and account root:

```elisp
(add-to-list 'load-path "/path/to/mail-util/emacs")
(require 'mail-util)
(setq mail-util-executable "/path/to/mail-util/target/release/mail-util"
      mail-util-root "~/Maildir/myaccount"   ; or leave nil and set MAILUTIL_ROOT
      mail-util-min-count 30)
```

Run `M-x mail-util-review` to analyze the inbox and open `*mail-util-review*`. In that
buffer:

| key | action |
|-----|--------|
| `n` / `p` | move between clusters |
| `a` / `r` / `u` | approve / reject / unset the cluster at point |
| `A` | approve all high-confidence clusters |
| `TAB` | toggle sample senders/subjects |
| `g` | re-run analysis (keeps your marks by cluster key) |
| `x` | export the approved clusters to JSON |
| `q` | quit |

Applying (creating folders and moving mail) is not in the CLI yet; today the workflow is
review + export. The exported JSON records the approved destinations and message UIDs and
is the seed for the forthcoming `plan`/`apply` commands.

## Key design invariant

mbsync's native scheme embeds the IMAP UID in each filename as `,U=<uid>`. Copying that
suffix into another folder duplicates the UID and forces a UIDVALIDITY reset, breaking
sync. All move logic (M4/M5) therefore either moves server-side via IMAP `UID MOVE`
(default) or, offline, writes a fresh `,U=`-less filename and lets mbsync re-assign the
UID — never a raw `mv`. See `crates/mailcache/src/filename.rs`.
