//! A step record written before the tree digest took its package format (kind, name, mode,
//! size and bytes; no owner) names a digest no tree has any more. No release of hostkeeper
//! wrote such a record; what the runner does with one is held here, so that the change cannot
//! make a step run from a record it does not match.

mod support;

use std::fs;

use hostkeeper_steps::RunError;
use hostkeeper_steps::record::RECORD_FILE;
use support::Rig;

/// The digest the earlier format gave a tree: any digest the current format does not give it.
const EARLIER: &str = "5d1b4e4f2a0c9f39b1e3c7a0d8f6e2b4a9c3d5e7f1a2b3c4d5e6f7a8b9c0d1e2";

/// Writes `EARLIER` as the record's artifact digest.
fn age(rig: &Rig) {
    let path = rig.state.join(RECORD_FILE);
    let text = fs::read_to_string(&path).unwrap();
    let mut record: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_ne!(record["artifact_digest"], EARLIER);
    record["artifact_digest"] = serde_json::Value::String(EARLIER.to_owned());
    fs::write(&path, serde_json::to_string(&record).unwrap()).unwrap();
}

/// Catches an unfinished record of the earlier digest being resumed, or overwritten, for an
/// order whose tree now has another digest: the order is `Busy` (no step runs and the record
/// stays), as for any other update; and after a verdict such a record is a finished update of
/// another release, so the same order runs as a new update.
#[test]
fn a_record_of_the_earlier_digest_is_another_update() {
    let rig = Rig::new("dc-unfinished");
    rig.fake
        .lose_run
        .store(2, std::sync::atomic::Ordering::SeqCst);
    assert!(rig.runner().run(&rig.order()).is_err());
    age(&rig);
    let kept = fs::read(rig.state.join(RECORD_FILE)).unwrap();
    let runs = rig.fake.runs().len();
    let err = rig.runner().run(&rig.order()).unwrap_err();
    assert!(matches!(err, RunError::Busy { .. }), "{err:?}");
    assert_eq!(rig.fake.runs().len(), runs);
    assert_eq!(fs::read(rig.state.join(RECORD_FILE)).unwrap(), kept);

    let rig = Rig::new("dc-finished");
    let first = rig.runner().run(&rig.order()).unwrap();
    age(&rig);
    let runs = rig.fake.runs().len();
    assert_eq!(rig.runner().run(&rig.order()).unwrap(), first);
    assert!(
        rig.fake.runs().len() > runs,
        "the order ran again as a new update"
    );
}
