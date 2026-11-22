//! The Full Disk Access state of a Mac: read from its source by each heartbeat, and shaped as JSON
//! for the heartbeat's bound. Its mapping onto the gRPC request is tested in `proto_map`, and the
//! coordinator's keeping of it in the coordinator's crate.

use std::sync::{Arc, Mutex};

use hostkeeper_link::heartbeater::{DEFAULT_HEARTBEAT_SECS, LoopConfig};
use hostkeeper_link::{
    BeatOutcome, FullDiskAccess, FullDiskAccessSource, Heartbeater, MAX_HEARTBEAT_BYTES, Platform,
    fit,
};

use crate::{Rig, version};

/// A source whose state a test sets.
struct Settable(Mutex<FullDiskAccess>);

impl FullDiskAccessSource for Settable {
    fn full_disk_access(&self) -> FullDiskAccess {
        *self.0.lock().expect("the state")
    }
}

/// A loop over the rig's fakes with `source` as its Full Disk Access source.
fn loop_with(rig: &Rig, source: Option<Arc<dyn FullDiskAccessSource>>) -> Heartbeater {
    let mut ports = rig.world.ports();
    ports.full_disk_access = source;
    Heartbeater::new(
        LoopConfig {
            platform: Platform::DarwinArm64,
            agent_version: version("0.9.0"),
            state_dir: rig.state().to_path_buf(),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: Vec::new(),
            private_repository: false,
        },
        ports,
    )
}

/// Proves: the loop reads its source at each heartbeat, so a state that changes between two
/// heartbeats is the one the second carries.
///
/// Catches: a state read once at the start of the loop, which would never show a profile
/// installed or removed.
#[test]
fn each_heartbeat_carries_the_state_the_source_holds_then() {
    let rig = Rig::new("fda-read-each-beat");
    let source = Arc::new(Settable(Mutex::new(FullDiskAccess::NotGranted)));
    let mut heartbeater = loop_with(&rig, Some(source.clone()));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    *source.0.lock().expect("the state") = FullDiskAccess::Granted;
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let sent: Vec<Option<FullDiskAccess>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.full_disk_access)
        .collect();
    assert_eq!(
        sent,
        [
            Some(FullDiskAccess::NotGranted),
            Some(FullDiskAccess::Granted)
        ]
    );
}

/// Proves: a loop with no source sends no state.
///
/// Catches: a state made up for a host that reports none, which the coordinator would show as
/// known.
#[test]
fn a_loop_without_a_source_sends_no_state() {
    let rig = Rig::new("fda-no-source");
    let mut heartbeater = loop_with(&rig, None);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(rig.world.link.beats()[0].full_disk_access, None);
}

/// Proves: each state has its word, as the JSON of a heartbeat and the daemon's log say it, and a
/// heartbeat that holds a state counts it toward the bound without dropping anything.
///
/// Catches: a state word changed without the log and the JSON following, and a state that costs a
/// heartbeat one of its optional facts.
#[test]
fn the_states_have_their_words_and_cost_nothing_of_the_bound() {
    let words: Vec<&str> = [
        FullDiskAccess::Granted,
        FullDiskAccess::NotGranted,
        FullDiskAccess::NotApplicable,
        FullDiskAccess::Undetermined,
    ]
    .into_iter()
    .map(FullDiskAccess::as_str)
    .collect();
    assert_eq!(
        words,
        ["granted", "not_granted", "not_applicable", "undetermined"]
    );
    let rig = Rig::new("fda-json");
    let source = Arc::new(Settable(Mutex::new(FullDiskAccess::Undetermined)));
    let mut heartbeater = loop_with(&rig, Some(source));
    heartbeater.beat_once();
    let beat = rig.world.link.beats().remove(0);
    let with = beat.encoded_len();
    let mut without = beat.clone();
    without.full_disk_access = None;
    assert!(with > without.encoded_len());
    assert!(with < MAX_HEARTBEAT_BYTES);
    let fitted = fit(beat.clone()).expect("fits");
    assert_eq!(fitted.beat, beat);
    assert!(fitted.dropped.is_empty());
}
