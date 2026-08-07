# mail-util

A **local** tool that analyzes an mbsync-cached IMAP inbox, suggests new folders and
sieve rules for sorting, and (in later milestones) applies them by creating folders and
moving mail — without ever losing a message and without corrupting mbsync's sync state.
Nothing is ever uploaded to a third party; the optional labeling step uses a local
ollama instance only.

See `DESIGN.md` for the full design and the milestone roadmap.

## Status

**All milestones (M1–M6) done.** The tool scans the Maildir, clusters the inbox, and
produces ranked suggestions (`suggest`), a complete sorting plan with per-message move
actions + a generated Sieve script (`plan`), a preflight check (`verify`), and a crash-safe
`apply` engine with an append-only journal and no-loss invariants. Two movers are
available: the default **server-side IMAP mover** (`UID MOVE` with a `COPY`+`EXPUNGE`
fallback), and an opt-in **offline local mover** that rewrites the Maildir directly (fresh
`,U=`-less filenames, copy-verify-then-delete). Both reconcile with `mbsync` and verify
UIDVALIDITY is unchanged. `probe` reports the server's separator and MOVE support, and
`sieve` deploys the generated rules to the server via **ManageSieve** (merging into your
active script, preserving hand-written rules). The Emacs UI drives the whole loop:
review → plan → verify → dry-run → (guarded) real apply → deploy sieve.

## Workspace layout

| crate | role |
|-------|------|
| `crates/model` | shared serde types (`Message`, `Cluster`, `Destination`, `Config`) |
| `crates/mailcache` | read-only Maildir access: native `,U=` filename parse/emit, `.uidvalidity`, streaming header extraction, folder scanning |
| `crates/mockcache` | test-only builder that writes a real temp Maildir tree (native filenames, nested folders, `.uidvalidity`) |
| `crates/suggest` | deterministic clustering (List-Id > sender-domain > person), scoring, taxonomy reuse, human-readable slug generation |
| `crates/namemap` | maps a folder among its local dotpath, IMAP name, and Sieve target (separator-parameterized) |
| `crates/sieve` | renders sorting rules to a Sieve script and merges them into an existing user script |
| `crates/mover` | the `Mover` trait plus `DryRunMover` and an in-memory `FakeMover` for safety tests (real IMAP/local movers are later) |
| `crates/journal` | crash-safe apply engine, append-only journal, and the no-loss invariants |
| `crates/imapmover` | server-side `Mover`: `ImapOps` trait, `ImapMover` logic, `FakeImapOps` for tests, and a real `imap`-crate backend (`real-imap` feature) + `.netrc` auth |
| `crates/localmover` | offline `Mover`: rewrites the Maildir directly (fresh `,U=`-less names, copy-verify-then-delete) |
| `crates/managesieve` | deploy Sieve rules via ManageSieve: `SieveOps` trait, `SieveDeployer` (fetch→merge→put→activate), `FakeSieveOps`, and a real RFC 5804 backend (`real-sieve`) |
| `bin/mail-util` | CLI (`scan`, `suggest`, `plan`, `verify`, `probe`, `apply`, `sieve`) emitting JSON / NDJSON |

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

# A complete sorting plan: folders to create, per-message move actions, sieve script.
mail-util plan --min-count 30 > plan.json
jq '.sieve_text' -r plan.json          # the generated Sieve rules
jq '.folders_to_create' plan.json

# Restrict a plan to clusters you approved in the Emacs UI (its JSON export).
mail-util plan --approved approved.json > plan.json

# Preflight: re-check the plan against the current cache (read-only).
mail-util verify --plan plan.json      # -> { ok, resolved, unresolved, … }

# Dry-run the plan: stream one JSON journal record per line, moving nothing.
mail-util apply --plan plan.json --dry-run

# Read-only probe: the server's hierarchy separator and MOVE support.
# --imap-user picks the account when several share a host.
mail-util probe --imap-host imap.example.com --imap-user me@example.com

# Real apply: move mail server-side via IMAP, then reconcile the local cache.
# Requires --yes; credentials come from ~/.netrc (same machine line mbsync uses).
mail-util apply --plan plan.json --yes \
  --imap-host imap.example.com --mbsync-channel <your-channel>

# Offline alternative: build a plan for the local mover, then apply it without a
# network (rewrites the Maildir directly, then mbsync propagates the moves).
mail-util plan --mover local > plan.json
mail-util apply --plan plan.json --yes --mbsync-channel <your-channel>

# Sieve: preview the merged server script (read-only)…
mail-util sieve --plan plan.json --imap-host imap.example.com | jq -r .merged
# …then deploy it (uploads + activates; changes server-side filtering).
mail-util sieve --plan plan.json --imap-host imap.example.com --deploy
```

The inbox folder defaults to the Maildir++ convention `.INBOX`; override with
`--inbox <dotpath>` for other layouts. `plan` mutates nothing — it only emits JSON.

## Emacs

`emacs/mail-util.el` provides a review UI over the `suggest` command. Load it and point
it at your binary and account root:

```elisp
(add-to-list 'load-path "/path/to/mail-util/emacs")
(require 'mail-util)
(setq mail-util-executable "/path/to/mail-util/target/release/mail-util"
      mail-util-root "~/Maildir/myaccount"   ; or leave nil and set MAILUTIL_ROOT
      mail-util-min-count 30
      ;; For probe / real apply:
      mail-util-imap-host "imap.example.com"
      mail-util-imap-user "me@example.com"       ; pick the account when a host is shared
      mail-util-mbsync-channel "your-channel"    ; run after moves to reconcile
      mail-util-mover "imap")                    ; or "local" for the offline mover
```

`M-x mail-util-probe` checks the server connection (read-only).

Run `M-x mail-util-review` to analyze the inbox and open `*mail-util-review*`. In that
buffer:

| key | action |
|-----|--------|
| `n` / `p` | move between clusters |
| `a` / `r` / `u` | approve / reject / unset the cluster at point |
| `A` | approve all high-confidence clusters |
| `e` | edit the destination folder for the cluster at point (used when the plan is built) |
| `TAB` | toggle sample senders/subjects |
| `P` | build a plan from the approved clusters (or all, if none marked) |
| `g` | re-run analysis (keeps your marks by cluster key) |
| `x` | export the approved clusters to JSON |
| `q` | quit |

`P` opens a `*mail-util-plan*` buffer showing the folders to create and the generated
Sieve script. There:

| key | action |
|-----|--------|
| `v` | verify the plan against the current cache (resolve every action) |
| `d` | dry-run the plan — stream live journal progress into `*mail-util-apply*` |
| `X` | **apply for real** — move mail server-side (prompts for confirmation first) |
| `e` | **preview merged Sieve** — fetch your server script (read-only); the plan's Sieve section then shows the *merged* result (your rules + the tool's block) |
| `E` | view your current server Sieve script (after a preview) |
| `w` | write the Sieve script to a file |
| `D` | **deploy Sieve** to the server via ManageSieve (prompts for confirmation) |
| `s` | save the plan JSON to a file |
| `q` | quit |

When `mail-util-imap-host` is set, building a plan **automatically** fetches your live
server script and shows the Sieve section as the *merged* result — exactly what `D` would
upload — so you never see just the new rules in isolation. (`e` re-fetches on demand and
opens the side-by-side comparison; set `mail-util-sieve-auto-merge` to nil to fetch only
on `e`.) Merging **accumulates**: previously-deployed rules are kept, the new ones are
added, and your hand-written rules outside the markers are never touched — deploying never
overwrites.

`d` runs `apply --dry-run` (moves nothing). `X` runs the real apply: it asks
`REALLY move N messages on <host>?`, then moves them server-side via IMAP and reconciles
the cache with `mbsync` (needs `mail-util-imap-host`; `mail-util-mbsync-channel` for the
reconcile). Both stream live counters into `*mail-util-apply*`.

## Key design invariant

mbsync's native scheme embeds the IMAP UID in each filename as `,U=<uid>`. Copying that
suffix into another folder duplicates the UID and forces a UIDVALIDITY reset, breaking
sync. All move logic (M4/M5) therefore either moves server-side via IMAP `UID MOVE`
(default) or, offline, writes a fresh `,U=`-less filename and lets mbsync re-assign the
UID — never a raw `mv`. See `crates/mailcache/src/filename.rs`.
