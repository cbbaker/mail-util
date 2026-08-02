//! End-to-end: build a realistic mock Maildir, scan it with `mailcache`, and verify
//! the `suggest` engine produces the expected folder proposals — the M1 vertical slice.

use mailcache::Account;
use model::{Config, Destination, Signal};
use suggest::{suggest, ExistingFolder};

#[test]
fn scan_then_suggest_on_mock_cache() {
    let mc = mockcache::MockCache::new();

    // A mailing list (should propose .lists/.elixir).
    for uid in 1..=8u32 {
        mc.message(".INBOX")
            .uid(uid)
            .from("bounce@elixir-lang.groups.io")
            .list_id("elixir-lang.groups.io")
            .subject(&format!("[elixir] topic {uid}"))
            .write();
    }
    // A vendor domain (should propose .vendors/.github).
    for uid in 20..=26u32 {
        mc.message(".INBOX")
            .uid(uid)
            .from(&format!("notifications{uid}@github.com"))
            .subject("PR update")
            .write();
    }
    // A person via freemail (should propose .people/.jane-doe).
    for uid in 40..=45u32 {
        mc.message(".INBOX")
            .uid(uid)
            .from("jane.doe@gmail.com")
            .from_name("Jane Doe")
            .subject("dinner?")
            .write();
    }
    // Noise below threshold — must NOT be surfaced.
    mc.message(".INBOX").uid(99).from("random@nowhere.example").write();

    // An existing taxonomy folder that should be reused instead of created.
    mc.folder(".lists/.la-ruby");
    for uid in 1..=3u32 {
        mc.message(".lists/.la-ruby").uid(uid).from("x@laruby.org").write();
    }

    let account = Account::new(mc.root());
    let inbox = account.folder(".INBOX");
    assert_eq!(inbox.message_count(), 8 + 7 + 6 + 1);

    let messages = inbox.messages();
    let existing: Vec<ExistingFolder> = account
        .folders()
        .iter()
        .filter(|f| f.dotpath != ".INBOX")
        .map(|f| ExistingFolder::new(&f.dotpath))
        .collect();

    let config = Config { min_count: 5, ..Config::default() };
    let clusters = suggest(&messages, &existing, &config);

    // Three clusters surfaced; the singleton is dropped.
    assert_eq!(clusters.len(), 3, "clusters: {clusters:#?}");

    // List-Id cluster ranks first (strongest signal) and proposes a new .lists folder.
    let top = &clusters[0];
    assert_eq!(top.signal, Signal::ListId);
    assert_eq!(top.destination, Destination::New { dotpath: ".lists/.elixir-lang".into() });
    assert_eq!(top.count, 8);
    assert_eq!(top.uids.len(), 8);

    let by_dest = |d: &str| clusters.iter().find(|c| c.destination.dotpath() == d);
    assert!(by_dest(".vendors/.github").is_some());
    assert!(by_dest(".people/.jane-doe").is_some());

    // Determinism: identical inputs -> identical output.
    assert_eq!(clusters, suggest(&messages, &existing, &config));
}
