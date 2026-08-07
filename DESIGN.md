# Design

`mail-util` analyzes an [mbsync/isync](https://isync.sourceforge.io/)-cached IMAP
mailbox, suggests new folders and Sieve rules for sorting the inbox, and — once
approved — applies them by creating folders and moving mail. Two hard requirements shape
the whole design:

1. **Never lose a message.**
2. **Never corrupt mbsync's sync state.**

Everything runs locally. Mail is never sent to a third party; an optional cluster
*labeling* step talks only to a local [ollama](https://ollama.com/) instance and can be
disabled entirely.

## The core hazard: mbsync's native UID scheme

With its default *native* scheme, mbsync embeds each message's IMAP UID directly in the
Maildir filename as a `,U=<uid>` segment, e.g.:

```
1700000000.12345_10.host,U=10:2,S
└──────── unique base ────────┘└ U ┘└flags┘
```

Per-folder state (the `UIDVALIDITY` and highest UID) lives in a `.uidvalidity` file.

A naive `mv` of a message between two mbsync-managed folders copies the `,U=<uid>`
segment, producing a **duplicate UID**. mbsync reacts by resetting `UIDVALIDITY`, which
forces a full resync and breaks synchronization. Avoiding this is the reason moves are
never done with a raw `mv`.

## Two move strategies

Moves go through a common `Mover` trait with two implementations:

- **Server-side IMAP move (default).** Connect over IMAPS, `UID MOVE`
  ([RFC 6851](https://www.rfc-editor.org/rfc/rfc6851); fall back to
  `UID COPY` + `STORE \Deleted` + `UID EXPUNGE` when the server lacks MOVE), then run
  mbsync to reconcile the local cache. Atomic server-side and sidesteps native-scheme
  corruption entirely, because no local files are fabricated.
- **Offline local move (opt-in).** Write the message into the destination `new/` with a
  **fresh unique filename that omits `,U=`** (preserving the `:2,<flags>` info), fsync,
  byte-verify, journal, then delete the source. On the next sync mbsync uploads the new
  message (server assigns a fresh UID) and expunges the original. Gated behind an
  explicit flag since it is the sharp edge.

## Two-phase safety model

- `plan` is **pure**: it scans, clusters, and emits a plan (folders to create, a Sieve
  script, per-message move actions) as JSON. It mutates nothing.
- `apply` is the only mutating command, driven by an append-only, fsync'd journal so it
  is crash-safe and resumable. Ordering for local moves is strict:
  **copy → fsync → byte-verify → journal → delete source** — never delete first.

**No-message-lost invariants** (checked in code and tests):

1. Message-ID multiset across the whole mailbox is identical before/after.
2. Each moved message ends in exactly one folder.
3. Destination bytes equal source bytes, verified before any deletion.
4. `.uidvalidity` (the `UIDVALIDITY` line) is unchanged for every touched folder.
5. Per-folder counts reconcile around each mbsync run.

## Suggestion engine

Deterministic and reproducible: same inbox in, same clusters out. Per message it
extracts `List-Id`, sender address/domain, recipients, and subject (headers only — the
body is never loaded), then clusters by priority **List-Id > sender-domain > person**
(freemail senders cluster per-person since the domain carries no signal). Clusters below
a count/score threshold stay in the inbox. Existing folders are **reused** rather than
duplicated when a cluster matches one. Folder slugs are made human-readable, falling back
from an opaque ESP list-id (e.g. a Mailchimp hash) to the sender's display name or domain.

An optional local-LLM pass may only *rename* slugs; it never regroups messages.

## Folder-name mapping

Three representations must round-trip: the local Maildir++ dotpath (`.lists/.elixir`),
the IMAP mailbox name, and the Sieve `fileinto` target. The IMAP hierarchy separator is
**probed** from the server (`LIST "" "*"`), never hardcoded, since it varies by server.

## Crate layout

| crate | role |
|-------|------|
| `model` | shared serde types |
| `mailcache` | read-only Maildir access: filename parse/emit, `.uidvalidity`, header extraction, scanning |
| `mockcache` | test-only builder writing a real temp Maildir tree |
| `suggest` | clustering, scoring, taxonomy reuse, slug generation |
| `bin/mail-util` | CLI emitting JSON / NDJSON |

Planned: `namemap`, `sieve`, `mover`, `journal`, and an Emacs front-end.

## Milestones

1. **M1 — read-only slice (done):** scan + suggest as JSON.
2. **M2 — pure plan + Sieve generation, folder-name mapping (done):** `plan`/`verify`
   commands, `namemap` and `sieve` crates.
3. **M3 — journal/apply safety machinery + no-loss property tests (done):** `mover`
   (trait + DryRun + Fake) and `journal` crates, the `apply` command (`--dry-run`
   runnable today), crash/resume and conservation tests.
4. **M4 — server-side IMAP mover (default) + mbsync reconcile (done):** `imapmover`
   crate (`ImapOps` trait + `ImapMover` + `FakeImapOps`, real `imap` backend, `.netrc`
   auth), `probe` command, real `apply` with UIDVALIDITY-stability check.
5. **M5 — offline local mover (done):** `localmover` crate (fresh `,U=`-less names,
   copy-verify-then-delete, idempotent), wired to `apply --mover local`.
6. **M6 — Emacs review UI + ManageSieve deployment + LLM labeling.**
