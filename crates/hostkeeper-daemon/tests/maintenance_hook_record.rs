//! The maintenance hook's run record over the maintenance run record's file: what each kind's
//! state reads from it, what the hook's two writes change (and leave), the failures that name what
//! is wrong with the record, where the record is found from the parameters file, and a `cutover`
//! that records its reboot there.

mod support_maintenance;

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::maintenance_hook::{
    HookArgs, HookPorts, HookStep, Kind, KindState, RecordError, Registry, RunRecord, StoreRecord,
    main_with,
};
use hostkeeper_hook::Outcome;
use hostkeeper_steps::maintenance::{
    Kind as RecordKind, MAINTENANCE_RECORD_FILE, MaintenanceLoaded, MaintenanceRecord,
    MaintenanceStore, RunStep,
};
use hostkeeper_steps::{BootId, CrashGate};
use hostkeeper_testkit::Scratch;
use support_maintenance::{
    BOOT_A, BOOT_B, Effects, FakeBootId, FakeKind, FakeRebooter, RUN, rebooting, taken,
};

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch")
}

fn boot(text: &str) -> BootId {
    BootId::uuid("the test's boot id", text).expect("a boot id")
}

fn failed(text: &str) -> RecordError {
    RecordError::Failed(text.to_owned())
}

/// A record of the run [`RUN`] with `kinds`, two services of which the first is drained, at the
/// `drain` step with its launch written: every field the hook does not own set to something.
fn record(kinds: &[RecordKind]) -> MaintenanceRecord {
    let services = ["web", "db"]
        .map(|name| ServiceName::new(name).expect("a service name"))
        .to_vec();
    let mut record =
        MaintenanceRecord::new(RUN, "order-1", 3, kinds, services).expect("a valid record");
    record.drained = record.services[..1].to_vec();
    record.step = RunStep::Apply;
    record.running = true;
    record
}

/// A `StoreRecord` over `dir`, holding `record` when there is one.
fn holding(dir: &Path, record: Option<&MaintenanceRecord>) -> StoreRecord {
    if let Some(record) = record {
        MaintenanceStore::new(dir).save(record).expect("save");
    }
    StoreRecord::new(MaintenanceStore::new(dir))
}

fn on_disk(dir: &Path) -> MaintenanceRecord {
    match MaintenanceStore::new(dir).load().expect("load") {
        MaintenanceLoaded::Current(record) => *record,
        other => panic!("no current record: {other:?}"),
    }
}

/// Proves: each kind's state is read from the saved record: applied, with a reboot under way (and
/// the boot it was asked in), and neither; a kind the record does not hold is pending.
///
/// Catches: a kind read as the progress of another, an applied kind read as pending (which would
/// install it again), or a kind missing from the record taken as a failure.
#[test]
fn each_kind_reads_its_progress_from_the_saved_record() {
    let scratch = scratch("mh-record-state");
    let mut saved = record(&[RecordKind::Macos, RecordKind::Xcode, RecordKind::Linux]);
    saved.kinds[1].reboot_under_way = Some(boot(BOOT_A));
    saved.kinds[2].applied = true;
    let store = holding(scratch.path(), Some(&saved));
    assert_eq!(store.state(RUN, Kind::Macos), Ok(KindState::Pending));
    assert_eq!(
        store.state(RUN, Kind::Xcode),
        Ok(KindState::RebootUnderWay { boot: boot(BOOT_A) })
    );
    assert_eq!(store.state(RUN, Kind::Linux), Ok(KindState::Applied));
    assert_eq!(store.state(RUN, Kind::Reboot), Ok(KindState::Pending));
}

/// Proves: recording a reboot under way is read back as that kind's state, and changes nothing
/// else of the record: the whole record loaded afterwards is the saved one with that one field.
///
/// Catches: a write that drops the step, the drained services or the other kinds, which the
/// daemon's sequencer owns.
#[test]
fn a_reboot_under_way_is_recorded_and_the_rest_of_the_record_is_kept() {
    let scratch = scratch("mh-record-reboot");
    let saved = record(&[RecordKind::Linux, RecordKind::Reboot]);
    let store = holding(scratch.path(), Some(&saved));
    store
        .reboot_under_way(RUN, Kind::Linux, &boot(BOOT_A))
        .expect("recorded");
    assert_eq!(
        store.state(RUN, Kind::Linux),
        Ok(KindState::RebootUnderWay { boot: boot(BOOT_A) })
    );
    let mut expected = saved;
    expected.kinds[0].reboot_under_way = Some(boot(BOOT_A));
    assert_eq!(on_disk(scratch.path()), expected);
}

/// Proves: recording a kind applied sets it applied and clears its reboot under way, and changes
/// nothing else of the record.
///
/// Catches: an applied kind still holding its reboot (which a later step would wait on again), or
/// a write that touches another kind.
#[test]
fn applied_is_recorded_and_clears_the_reboot_under_way() {
    let scratch = scratch("mh-record-applied");
    let mut saved = record(&[RecordKind::Linux, RecordKind::Reboot]);
    saved.kinds[0].reboot_under_way = Some(boot(BOOT_A));
    let store = holding(scratch.path(), Some(&saved));
    store.applied(RUN, Kind::Linux, "1.2.3").expect("recorded");
    assert_eq!(store.state(RUN, Kind::Linux), Ok(KindState::Applied));
    let mut expected = saved;
    expected.kinds[0].applied = true;
    expected.kinds[0].reboot_under_way = None;
    assert_eq!(on_disk(scratch.path()), expected);
}

/// Proves: a record of another run, no record, a record of a newer schema, one that does not read,
/// and a kind the record does not hold each fail the read and the writes with the exact text that
/// names what is wrong; a write the file refuses fails with the writer's own words, and the record
/// is as it was.
///
/// Catches: another run's progress taken for this run's, a missing record taken as every kind
/// pending, or a newer record read as an older one and written over.
#[test]
fn a_record_that_is_not_this_runs_fails_naming_what_is_wrong() {
    let scratch = scratch("mh-record-failures");
    let dir = scratch.path();
    let saved = record(&[RecordKind::Linux]);
    let writes = |store: &StoreRecord| {
        [
            store.state(RUN, Kind::Linux).map(|_| ()),
            store.reboot_under_way(RUN, Kind::Linux, &boot(BOOT_A)),
            store.applied(RUN, Kind::Linux, "1"),
        ]
    };

    let store = holding(dir, None);
    for got in writes(&store) {
        assert_eq!(got, Err(failed("there is no run record")));
    }

    let store = holding(dir, Some(&saved));
    for got in writes(&store) {
        assert_eq!(got, Ok(()));
    }
    for got in [
        store.state("run-8", Kind::Linux).map(|_| ()),
        store.reboot_under_way("run-8", Kind::Linux, &boot(BOOT_A)),
        store.applied("run-8", Kind::Linux, "1"),
    ] {
        assert_eq!(
            got,
            Err(failed("the run record holds run run-7, not run-8"))
        );
    }
    for got in [
        store.reboot_under_way(RUN, Kind::Xcode, &boot(BOOT_A)),
        store.applied(RUN, Kind::Xcode, "1"),
    ] {
        assert_eq!(got, Err(failed("the run record has no kind xcode")));
    }

    fs::write(dir.join(MAINTENANCE_RECORD_FILE), r#"{"schema":2}"#).expect("write");
    for got in writes(&store) {
        assert_eq!(
            got,
            Err(failed("the run record has schema 2, of a newer hostkeeper"))
        );
    }
    assert_eq!(
        fs::read_to_string(dir.join(MAINTENANCE_RECORD_FILE)).expect("read"),
        r#"{"schema":2}"#
    );

    fs::write(dir.join(MAINTENANCE_RECORD_FILE), "not json").expect("write");
    let unreadable = MaintenanceStore::new(dir).load().expect_err("unreadable");
    for got in writes(&store) {
        assert_eq!(got, Err(failed(&unreadable.to_string())));
    }

    fs::remove_file(dir.join(MAINTENANCE_RECORD_FILE)).expect("remove");
    MaintenanceStore::new(dir).save(&saved).expect("save");
    let gate: CrashGate = Arc::new(|_| true);
    let stopped = StoreRecord::new(MaintenanceStore::new(dir).with_crash_gate(gate));
    // The store counts its writes: the first is stopped as write 1, the second as write 2.
    assert_eq!(
        [
            stopped.reboot_under_way(RUN, Kind::Linux, &boot(BOOT_A)),
            stopped.applied(RUN, Kind::Linux, "1"),
        ],
        [
            Err(failed("stopped at BeforeWrite(1)")),
            Err(failed("stopped at BeforeWrite(2)")),
        ]
    );
    assert_eq!(on_disk(dir), saved);
}

/// Proves: the record beside a parameters file is the one in that file's directory (a state read
/// finds the record saved there), and parameters on standard input, or a path with no directory,
/// have none.
///
/// Catches: the record looked for in the working directory instead of beside the parameters, or a
/// record invented for parameters that have no place.
#[test]
fn the_record_is_found_beside_the_parameters_file() {
    assert!(StoreRecord::beside_params(Path::new("-")).is_none());
    assert!(StoreRecord::beside_params(Path::new("")).is_none());
    let scratch = scratch("mh-record-beside");
    let mut saved = record(&[RecordKind::Linux]);
    saved.kinds[0].applied = true;
    let _written = holding(scratch.path(), Some(&saved));
    let beside = StoreRecord::beside_params(&scratch.path().join("params.json")).expect("a record");
    assert_eq!(beside.state(RUN, Kind::Linux), Ok(KindState::Applied));
}

/// Proves: a `cutover` over a kind that needs a reboot, with the record beside its parameters
/// file, writes the kind's reboot under way and the boot id to the file before it asks for the
/// reboot, and ends `reboot_under_way`; the `cutover` of the next boot records the kind applied
/// and clears the reboot, without installing the kind again.
///
/// Catches: a reboot asked for with nothing on disk to find it by, or a kind installed again after
/// its reboot.
#[test]
fn a_cutover_records_its_reboot_in_the_file_beside_its_parameters() {
    let scratch = scratch("mh-record-cutover");
    let dir = scratch.path();
    let params = dir.join("params.json");
    fs::write(
        &params,
        format!(r#"{{"runId":"{RUN}","kinds":["MAINTENANCE_KIND_LINUX"]}}"#),
    )
    .expect("write");
    let effects = Effects::default();
    let registry = Registry::new().with(
        Kind::Linux,
        Box::new(FakeKind::rebooting(Kind::Linux, "1.2.3", &effects)),
    );
    let store = holding(dir, Some(&record(&[RecordKind::Linux])));
    let rebooter = FakeRebooter {
        effects: effects.clone(),
        fail: false,
    };
    let boot_id = FakeBootId(Mutex::new(BOOT_A.to_owned()));
    let ports = HookPorts {
        kinds: &registry,
        record: &store,
        rebooter: &rebooter,
        boot_id: &boot_id,
    };
    let args = HookArgs {
        step: HookStep::Cutover,
        params: params.clone(),
    };
    let run = || {
        let mut stdout = Vec::new();
        let outcome = main_with(&args, &ports, &mut io::empty(), &mut stdout);
        (outcome, String::from_utf8(stdout).expect("utf-8"))
    };

    let (outcome, stdout) = run();
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, format!("{}\n", rebooting("linux").line));
    assert_eq!(taken(&effects), ["linux cutover", "reboot"]);
    let mut expected = record(&[RecordKind::Linux]);
    expected.kinds[0].reboot_under_way = Some(boot(BOOT_A));
    assert_eq!(on_disk(dir), expected);

    boot_id.set(BOOT_B);
    let (outcome, _) = run();
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(taken(&effects), ["linux after_reboot"]);
    expected.kinds[0].reboot_under_way = None;
    expected.kinds[0].applied = true;
    assert_eq!(on_disk(dir), expected);
}
