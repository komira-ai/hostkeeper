//! `clear_record`: only a step record that does not read is set aside, as
//! `step-record.json.corrupt` with its bytes, and nothing else of the host changes; a record that
//! reads, a held lock and a coordinator's order are refused.

mod support;

use std::fs;
use std::path::PathBuf;

use hostkeeper_config::names::Version;
use hostkeeper_daemon::halt::HELD_HALT_FILE;
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::verbs::clear_record::CORRUPT_FILE;
use hostkeeper_daemon::{Refusal, Refused, Reply, Verb};
use hostkeeper_hook::Verdict;
use hostkeeper_steps::record::{PARAMS_FILE, RECORD_FILE};
use support::{Rig, d, name, snapshot, update};

/// A record of the step runner's committed fixtures of schema 1 (and one of schema 2).
fn fixture(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-steps/tests/fixtures/kbf-v0.0.64")
        .join(file);
    fs::read_to_string(path).expect("the fixture")
}

/// `web`'s state directory, made, with `record` as its step record.
fn with_record(rig: &Rig, record: &[u8]) -> PathBuf {
    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the service's directory");
    fs::write(dir.join(RECORD_FILE), record).expect("the record");
    dir
}

/// Catches a record that reads removed: an absent record, one in each state (`pending`,
/// `running`, `done`, `finished`), and one of a newer schema are each refused `record_reads`
/// with their own text, and the state directory is byte for byte as it was.
#[test]
fn clear_record_refuses_a_record_that_reads_fine() {
    let rig = Rig::new("clear-reads");
    let daemon = rig.daemon();
    fs::create_dir_all(rig.state.join("web")).expect("the service's directory");
    let reads = |what: &str| {
        Refused::new(
            Refusal::RecordReads,
            format!("the step record of web {what}; only a corrupt one is cleared"),
        )
    };
    let before = snapshot(&rig.state);
    assert_eq!(daemon.clear_record(&name("web")), Err(reads("is absent")));
    assert_eq!(snapshot(&rig.state), before);
    let rows = [
        ("pending.json", "reads"),
        ("running.json", "reads"),
        ("done.json", "reads"),
        ("finished.json", "reads"),
        ("newer-schema.json", "has schema 2, of a newer hostkeeper"),
    ];
    for (file, what) in rows {
        with_record(&rig, fixture(file).as_bytes());
        let before = snapshot(&rig.state);
        assert_eq!(
            daemon.clear_record(&name("web")),
            Err(reads(what)),
            "{file}"
        );
        assert_eq!(snapshot(&rig.state), before, "{file}");
    }
}

/// Catches a clear that deletes anything beyond the record, or leaves the corrupt record in
/// place: an empty record, a truncated one and one with a field of the wrong type are each
/// renamed to `step-record.json.corrupt` with the same bytes (replacing the one before); the
/// held halt, the params file, the package directory with `current` and `previous`, and another
/// service's state directory are byte for byte as they were; and the next order of the service
/// runs as generation 1.
#[test]
fn clear_record_clears_a_corrupt_record_and_nothing_else() {
    let rig = Rig::new("clear-corrupt");
    rig.running("web", "1.0.0");
    rig.stage("web", "0.9.0");
    rig.store.link("web", "previous", "0.9.0");
    rig.add("web", "2.0.0");
    rig.add("api", "2.0.0");
    let daemon = rig.daemon();
    let succeeded = Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    };
    assert_eq!(daemon.order(&update("api", "2.0.0", &d(), 1)), succeeded);
    let finished = fixture("finished.json");
    let truncated = &finished.as_bytes()[..finished.len() / 2];
    let wrong_type = finished.replace("\"generation\": 13", "\"generation\": \"thirteen\"");
    assert_ne!(wrong_type, finished, "the fixture has the field");
    let dir = with_record(&rig, b"");
    fs::write(dir.join(HELD_HALT_FILE), "3\n").expect("a held halt");
    fs::write(dir.join(PARAMS_FILE), "left = 1\n").expect("a params file");
    let kept = || {
        let web = [HELD_HALT_FILE, PARAMS_FILE].map(|file| fs::read(dir.join(file)).ok());
        (
            web,
            snapshot(&rig.packages),
            snapshot(&rig.state.join("api")),
        )
    };
    let before = kept();
    for corrupt in [&b""[..], truncated, wrong_type.as_bytes()] {
        with_record(&rig, corrupt);
        let cleared = daemon.clear_record(&name("web")).expect("cleared");
        assert!(!cleared.why.is_empty());
        assert!(!dir.join(RECORD_FILE).exists());
        assert_eq!(
            fs::read(dir.join(CORRUPT_FILE)).expect("set aside"),
            corrupt
        );
        assert_eq!(kept(), before);
    }
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 1)), succeeded);
    assert_eq!(rig.record("web").expect("the record").generation, 1);
}

/// Catches `clear_record` that ignores the service's lock: with the lock held, a corrupt record
/// is refused `busy` and stays where it is.
#[test]
fn clear_record_is_refused_while_the_service_lock_is_held() {
    let rig = Rig::new("clear-record-busy");
    let daemon = rig.daemon();
    let dir = with_record(&rig, b"{");
    let package = Package {
        verb: HookedVerb::Update,
        version: Version::new("2.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
    };
    let guard = daemon
        .ctx()
        .locks
        .take(&name("web"), package)
        .expect("free");
    let refused = daemon.clear_record(&name("web")).expect_err("busy");
    let detail = "web is locked by an order to update it to 2.0.0";
    assert_eq!(refused, Refused::new(Refusal::Busy, detail));
    assert_eq!(fs::read(dir.join(RECORD_FILE)).expect("in place"), b"{");
    drop(guard);
}

/// Catches `clear_record` reachable from the coordinator: the verb in an order's path is refused
/// `forbidden` before any lock is tried (with the service's lock held it is still `forbidden`,
/// never `busy`), and the corrupt record stays where it is. No `Order` can carry the verb.
#[test]
fn clear_record_from_a_coordinator_order_is_refused_before_any_lock() {
    let rig = Rig::new("clear-record-order");
    let daemon = rig.daemon();
    let dir = with_record(&rig, b"{");
    let package = Package {
        verb: HookedVerb::Update,
        version: Version::new("2.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
    };
    let detail = "`clear_record` of web is the admin socket's, not an order's";
    let forbidden = Reply::refused(Refusal::Forbidden, detail);
    assert_eq!(daemon.execute(Verb::ClearRecord(name("web"))), forbidden);
    let guard = daemon
        .ctx()
        .locks
        .take(&name("web"), package)
        .expect("free");
    assert_eq!(daemon.execute(Verb::ClearRecord(name("web"))), forbidden);
    drop(guard);
    assert_eq!(fs::read(dir.join(RECORD_FILE)).expect("in place"), b"{");
    assert_eq!(daemon.ctx().locks.holder(&name("web")), None);
}

/// Catches a record that cannot be read at all taken for a corrupt one: a directory in the
/// record's place is `store_failed`, and nothing is renamed.
#[test]
fn clear_record_of_a_record_that_cannot_be_read_is_store_failed() {
    let rig = Rig::new("clear-record-dir");
    let daemon = rig.daemon();
    let dir = rig.state.join("web");
    fs::create_dir_all(dir.join(RECORD_FILE)).expect("a directory in the record's place");
    let refused = daemon.clear_record(&name("web")).expect_err("not read");
    assert_eq!(refused.refusal, Refusal::StoreFailed);
    assert!(dir.join(RECORD_FILE).is_dir());
    assert!(!dir.join(CORRUPT_FILE).exists());
}

/// Catches a rename that fails reported as cleared: with a directory that holds a file in the
/// place of `step-record.json.corrupt`, the rename fails, the answer is `store_failed`, and the
/// corrupt record is still where it was.
#[test]
fn clear_record_whose_rename_fails_is_store_failed_and_leaves_the_record() {
    let rig = Rig::new("clear-record-rename");
    let daemon = rig.daemon();
    let dir = with_record(&rig, b"{");
    fs::create_dir_all(dir.join(CORRUPT_FILE).join("in-the-way")).expect("a directory in the way");
    let refused = daemon.clear_record(&name("web")).expect_err("not renamed");
    assert_eq!(refused.refusal, Refusal::StoreFailed);
    assert_eq!(fs::read(dir.join(RECORD_FILE)).expect("in place"), b"{");
}
