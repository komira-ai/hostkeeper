//! The repository installer's ports in the daemon that need no daemon: the ports' directories,
//! the purge's pause on the daemon's clock, the `installed` records kept in `placed.json`, the
//! backend of an entry, and how a failure of the installer becomes a verdict, a step exit or a
//! switch error.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hostkeeper_install::repository::{Reason, Repositories};
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs, Log};
use hostkeeper_release::Installer;
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::Owner;

use super::*;
use crate::testing::{FakeClock, ScriptedLauncher};

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn web() -> ServiceName {
    ServiceName::new("web").expect("a name")
}

fn api() -> ServiceName {
    ServiceName::new("api").expect("a name")
}

fn roots() -> Roots {
    Roots::new(
        std::path::Path::new("/placed"),
        std::path::Path::new("/units"),
        std::path::Path::new("/packages"),
    )
}

/// The records on a host whose root is a scratch directory.
struct Host {
    scratch: Scratch,
    records: PlacedRecords,
}

impl Host {
    fn new(name: &str) -> Host {
        let scratch = scratch(name);
        fs::create_dir(scratch.path().join("placed")).expect("the state directory");
        let records = PlacedRecords {
            roots: roots(),
            fs: Arc::new(FaultFs::new(scratch.path(), Log::new())),
            units: Arc::new(FakeUnitControl::new(Log::new())),
        };
        Host { scratch, records }
    }

    fn placed(&self) -> PathBuf {
        self.scratch.path().join("placed/placed.json")
    }

    /// Writes `placed.json` as the text `json`.
    fn write(&self, json: &str) {
        fs::write(self.placed(), json).expect("write placed.json");
    }

    fn read(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.placed()).expect("placed.json")).expect("JSON")
    }
}

fn record(version: &str) -> PackageRecord {
    PackageRecord {
        installer: "apt".to_owned(),
        package: "web-pkg".to_owned(),
        version: version.to_owned(),
        repository: "main".to_owned(),
        links: BTreeSet::from([
            "/etc/systemd/system/multi-user.target.wants/web.service".to_owned()
        ]),
        pending: true,
    }
}

/// Proves the work directory and the lock sit beside the packages directory, and the packages
/// directory, host root and owner are the ones given.
#[test]
fn the_ports_put_the_work_directory_and_lock_beside_the_packages_directory() {
    let ports = RepositoryPorts::new(
        std::path::Path::new("/var/lib/hostkeeper/packages"),
        std::path::Path::new("/"),
        Repositories::default(),
        Owner::ROOT,
    );
    assert_eq!(
        ports.packages_dir,
        PathBuf::from("/var/lib/hostkeeper/packages")
    );
    assert_eq!(ports.dirs.root, PathBuf::from("/"));
    assert_eq!(ports.dirs.work, PathBuf::from("/var/lib/hostkeeper/apt"));
    assert_eq!(
        ports.lock,
        PathBuf::from("/var/lib/hostkeeper/installer.lock")
    );
}

/// Proves a packages directory with no parent (the root) puts them inside itself rather than
/// panicking.
#[test]
fn a_packages_directory_without_a_parent_keeps_its_work_inside_itself() {
    let ports = RepositoryPorts::new(
        std::path::Path::new("/"),
        std::path::Path::new("/"),
        Repositories::default(),
        Owner::ROOT,
    );
    assert_eq!(ports.dirs.work, PathBuf::from("/apt"));
    assert_eq!(ports.lock, PathBuf::from("/installer.lock"));
}

/// Proves the purge's pause is a sleep on the daemon's clock, of the seconds asked.
#[test]
fn the_purge_pauses_on_the_daemons_clock() {
    let clock = FakeClock::new();
    let pause = ClockPause(clock.clone());
    pause.pause(7);
    pause.pause(5);
    assert_eq!(clock.mono_secs(), 1012);
}

/// Proves an unwritten host has no record of a service, and a saved record reads back whole.
#[test]
fn a_saved_record_loads_back_and_a_missing_one_is_none() {
    let host = Host::new("repo-roundtrip");
    assert_eq!(host.records.load(&web()), Ok(None));
    host.records
        .save(&web(), &record("1.0.0-1"))
        .expect("saved");
    assert_eq!(host.records.load(&web()), Ok(Some(record("1.0.0-1"))));
    assert_eq!(host.records.load(&api()), Ok(None));
    assert_eq!(host.read()["installed"]["web"]["installer"], "apt");
}

/// Proves saving over a record replaces its fields, and clears `pending` when the new record is
/// not pending.
#[test]
fn saving_again_replaces_the_record() {
    let host = Host::new("repo-resave");
    host.records
        .save(&web(), &record("1.0.0-1"))
        .expect("saved");
    let settled = PackageRecord {
        pending: false,
        links: BTreeSet::new(),
        ..record("2.0.0-1")
    };
    host.records.save(&web(), &settled).expect("saved");
    assert_eq!(host.records.load(&web()), Ok(Some(settled)));
}

/// Proves a save keeps the other services' entries, the tombstones, the placed paths and keys it
/// does not know. Catches a save that rewrites the file from the one service's record.
#[test]
fn a_save_keeps_every_other_entry_and_the_tombstones() {
    let host = Host::new("repo-keeps");
    host.write(
        r#"{
  "schema": 1,
  "paths": {},
  "uninstalling": {"gone": {"hook_done": true}},
  "installed": {"api": {"installer": "apt", "package": "api-pkg", "version": "3", "repository": "main"}},
  "newer_key": 5
}"#,
    );
    host.records
        .save(&web(), &record("1.0.0-1"))
        .expect("saved");
    let after = host.read();
    assert_eq!(after["uninstalling"]["gone"]["hook_done"], true);
    assert_eq!(after["installed"]["api"]["package"], "api-pkg");
    assert_eq!(after["installed"]["web"]["version"], "1.0.0-1");
    assert_eq!(after["newer_key"], 5);
}

/// Proves an entry that names no package (a service installed by another installer, or an apt
/// entry not yet filled) loads as no record.
#[test]
fn an_entry_that_names_no_package_is_no_record() {
    let host = Host::new("repo-nameless");
    host.write(
        r#"{"schema": 1, "installed": {
  "web": {"installer": "apt"},
  "api": {"installer": "apt", "package": "p", "version": "1"},
  "db": {"installer": "apt", "package": "q", "repository": "main"}}}"#,
    );
    assert_eq!(host.records.load(&web()), Ok(None));
    assert_eq!(host.records.load(&api()), Ok(None), "no repository");
    assert_eq!(
        host.records.load(&ServiceName::new("db").expect("a name")),
        Ok(None),
        "no version"
    );
}

/// Proves saving over an entry that named no package keeps its installer spelling and fills the
/// rest.
#[test]
fn saving_fills_an_entry_that_named_no_package() {
    let host = Host::new("repo-fill");
    host.write(r#"{"schema": 1, "installed": {"web": {"installer": "apt"}}}"#);
    host.records
        .save(&web(), &record("1.0.0-1"))
        .expect("saved");
    assert_eq!(host.records.load(&web()), Ok(Some(record("1.0.0-1"))));
}

/// Proves clear drops the one service's entry and leaves the others and the tombstones.
#[test]
fn clear_drops_one_entry_and_keeps_the_rest() {
    let host = Host::new("repo-clear");
    host.records
        .save(&web(), &record("1.0.0-1"))
        .expect("saved");
    host.records
        .save(&api(), &record("2.0.0-1"))
        .expect("saved");
    host.records.clear(&web()).expect("cleared");
    assert_eq!(host.records.load(&web()), Ok(None));
    assert_eq!(host.records.load(&api()), Ok(Some(record("2.0.0-1"))));
    host.records
        .clear(&web())
        .expect("clearing nothing is no error");
}

/// Proves the holder of a package is another service that records it, never the service asking,
/// and never an entry that names no package.
#[test]
fn the_holder_is_another_service_that_records_the_package() {
    let host = Host::new("repo-holder");
    assert_eq!(host.records.holder("web-pkg", &api()), Ok(None));
    host.records
        .save(&web(), &record("1.0.0-1"))
        .expect("saved");
    assert_eq!(host.records.holder("web-pkg", &api()), Ok(Some(web())));
    assert_eq!(
        host.records.holder("web-pkg", &web()),
        Ok(None),
        "the service itself holds nothing against itself"
    );
    assert_eq!(host.records.holder("other-pkg", &api()), Ok(None));
    host.write(r#"{"schema": 1, "installed": {"web": {"installer": "apt"}}}"#);
    assert_eq!(host.records.holder("web-pkg", &api()), Ok(None));
}

/// Proves a `placed.json` that is not valid, or breaks a rule of the record, is a record error
/// from every operation, and is left as it was.
#[test]
fn a_corrupt_placed_json_is_a_record_error_everywhere() {
    for (name, text) in [
        ("repo-corrupt-json", "{ not json"),
        ("repo-corrupt-schema", r#"{"schema": 9}"#),
        (
            "repo-corrupt-name",
            r#"{"schema": 1, "installed": {"Bad Name": {"installer": "apt"}}}"#,
        ),
    ] {
        let host = Host::new(name);
        host.write(text);
        let service = web();
        let errors = [
            host.records.load(&service).expect_err("load"),
            host.records.save(&service, &record("1")).expect_err("save"),
            host.records.clear(&service).expect_err("clear"),
            host.records.holder("p", &service).expect_err("holder"),
        ];
        for error in errors {
            assert!(
                error.to_string().starts_with("the installed record: "),
                "{error}"
            );
        }
        assert_eq!(fs::read_to_string(host.placed()).expect("kept"), text);
    }
}

/// Proves a `placed.json` that cannot be read is a record error naming the read.
#[test]
fn an_unreadable_placed_json_is_a_record_error() {
    let host = Host::new("repo-unreadable");
    let records = PlacedRecords {
        fs: Arc::new(FaultFs::new(host.scratch.path(), Log::new()).failing_read(1, true)),
        ..host.records
    };
    let error = records.load(&web()).expect_err("unreadable");
    assert!(error.0.contains("read"), "{error}");
    assert!(records.holder("p", &web()).is_err());
}

/// Proves a write that fails is a record error and leaves no record, and a placed lock held
/// elsewhere is a record error before anything is read or written.
#[test]
fn a_failed_write_or_a_held_lock_is_a_record_error() {
    let host = Host::new("repo-write-fails");
    let records = PlacedRecords {
        fs: Arc::new(FaultFs::failing_at(host.scratch.path(), 1, Log::new())),
        units: Arc::new(FakeUnitControl::new(Log::new())),
        roots: roots(),
    };
    assert!(records.save(&web(), &record("1")).is_err());
    assert!(!host.placed().exists());
    let held = host.records.placer().lock().expect("the lock");
    let error = host.records.save(&web(), &record("1")).expect_err("locked");
    assert!(error.0.contains("lock"), "{error}");
    assert!(host.records.clear(&web()).is_err());
    drop(held);
    host.records
        .save(&web(), &record("1"))
        .expect("saved once released");
}

/// Proves a host with no state directory fails the write with a record error, not a panic.
#[test]
fn a_missing_state_directory_is_a_record_error() {
    let scratch = scratch("repo-no-state");
    let records = PlacedRecords {
        roots: roots(),
        fs: Arc::new(FaultFs::new(scratch.path(), Log::new())),
        units: Arc::new(FakeUnitControl::new(Log::new())),
    };
    assert!(records.save(&web(), &record("1")).is_err());
    assert_eq!(records.load(&web()), Ok(None));
}

fn entry(installer: &str) -> Target {
    Target {
        installer: installer.to_owned(),
        ..Target::default()
    }
}

/// Proves `apt` has the repository backend and every other installer name, or none, has not.
#[test]
fn only_an_apt_entry_has_a_backend() {
    let backend = backend_of_entry(&entry("apt")).expect("a backend");
    assert_eq!(backend.installer(), Installer::Apt);
    for other in [
        "hostkeeper-archive",
        "macos-pkg",
        "oci-image",
        "tarball",
        "nonsense",
        "",
    ] {
        assert!(backend_of_entry(&entry(other)).is_none(), "{other:?}");
    }
}

/// Proves a failure that fails the order before the commit point is `Failed` with its class and
/// token, and one a later try may pass is `Deferred` for [`DEFER_SECS`], never resent.
#[test]
fn a_failure_before_the_runner_is_a_failed_or_deferred_verdict() {
    let reply = |reason| finished(&Failure::new(reason, "detail"));
    assert_eq!(
        reply(Reason::PackageScripts),
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: HookClass::Release,
            },
            reason: Some("package_scripts".to_owned()),
            resent: false,
        }
    );
    assert_eq!(
        reply(Reason::Signature),
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: HookClass::Environment,
            },
            reason: Some("signature".to_owned()),
            resent: false,
        }
    );
    assert_eq!(
        reply(Reason::FetchFailed),
        Reply::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(DEFER_SECS),
            },
            reason: Some("fetch_failed".to_owned()),
            resent: false,
        }
    );
}

/// Proves every reason ends the order with its own token, and exactly the deferrable ones defer.
#[test]
fn every_reason_gives_its_token_and_only_the_deferrable_ones_defer() {
    for reason in Reason::ALL {
        let Reply::Finished {
            verdict,
            reason: token,
            ..
        } = finished(&Failure::new(reason, "x"))
        else {
            panic!("not finished");
        };
        assert_eq!(token.as_deref(), Some(reason.token()));
        assert_eq!(
            matches!(verdict, Verdict::Deferred { .. }),
            reason.outcome() == Outcome::Deferred,
            "{reason:?}"
        );
    }
}

/// Proves a failed step ends with exit 1 and a line of class, reason and detail, and a deferred
/// one with exit 75 and the retry hint too.
#[test]
fn a_failure_in_the_switch_ends_the_step_with_exit_1_or_75() {
    let failed = exit_of(&Failure::new(Reason::DpkgError, "dpkg said no"));
    assert_eq!(failed.code, Some(1));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(failed.last_line.as_deref().expect("a line"))
            .expect("JSON"),
        serde_json::json!({"class": "release", "reason": "dpkg_error", "detail": "dpkg said no"})
    );
    let deferred = exit_of(&Failure::new(Reason::PackageManagerBusy, "locked"));
    assert_eq!(deferred.code, Some(75));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(deferred.last_line.as_deref().expect("a line"))
            .expect("JSON"),
        serde_json::json!({
            "class": "environment",
            "reason": "package_manager_busy",
            "detail": "locked",
            "retry_after_secs": DEFER_SECS,
        })
    );
}

/// Proves a failure of the host's files is a switch that failed and is tried again, with the
/// failure's text, and every other kind of outcome ends the step with its exit.
#[test]
fn only_a_failure_of_the_hosts_files_is_retried_by_the_switch() {
    let io = Failure::new(Reason::IoError, "disk full");
    assert_eq!(
        switch_error(io),
        SwitchError::Failed("io_error: disk full".to_owned())
    );
    for reason in Reason::ALL.into_iter().filter(|r| *r != Reason::IoError) {
        let failure = Failure::new(reason, "why");
        assert_eq!(
            switch_error(failure.clone()),
            SwitchError::Ended(exit_of(&failure)),
            "{reason:?}"
        );
    }
}

/// A repository installer on the scratch host, with its service manager and the log of it.
fn repository_with_units(scratch: &Scratch) -> (Repository, Arc<FakeUnitControl>, Log) {
    let packages = scratch.path().join("packages");
    fs::create_dir(&packages).expect("packages");
    let log = Log::new();
    let units = Arc::new(FakeUnitControl::new(log.clone()));
    let repository = Repository {
        launcher: ScriptedLauncher::new(),
        ports: crate::testing::repository_ports(&packages, scratch.path(), Repositories::default()),
        clock: FakeClock::new(),
        roots: roots(),
        fs: Arc::new(FaultFs::new(scratch.path(), log.clone())),
        units: units.clone(),
        max_package_bytes: 1 << 20,
        repaired: Arc::default(),
        tool_runs: Arc::default(),
    };
    (repository, units, log)
}

fn repository(scratch: &Scratch) -> Repository {
    repository_with_units(scratch).0
}

/// Proves the drift an activate repaired is taken once, by service, and a service that repaired
/// nothing has none.
#[test]
fn the_repaired_drift_is_taken_once_by_service() {
    let scratch = scratch("repo-repaired");
    let repository = repository(&scratch);
    assert_eq!(repository.take_repaired(&web()), None);
    let drift = Drift {
        found: Some("1.0.0-1".to_owned()),
        recorded: Some("2.0.0-1".to_owned()),
    };
    repository
        .repaired
        .lock()
        .expect("unpoisoned")
        .insert("web".to_owned(), drift.clone());
    assert_eq!(repository.take_repaired(&api()), None);
    assert_eq!(repository.take_repaired(&web()), Some(drift));
    assert_eq!(repository.take_repaired(&web()), None);
}

/// Proves a drift left by a thread that panicked holding the map is still taken.
#[test]
fn the_repaired_drift_is_taken_after_a_panic_poisoned_the_map() {
    let scratch = scratch("repo-poisoned");
    let repository = repository(&scratch);
    let repaired = Arc::clone(&repository.repaired);
    let drift = Drift {
        found: None,
        recorded: Some("2.0.0-1".to_owned()),
    };
    repaired
        .lock()
        .expect("unpoisoned")
        .insert("web".to_owned(), drift.clone());
    let poisoner = Arc::clone(&repaired);
    std::thread::spawn(move || {
        let _held = poisoner.lock().expect("unpoisoned");
        panic!("the map is held while this thread panics");
    })
    .join()
    .expect_err("the thread panicked");
    assert!(Mutex::is_poisoned(&repaired));
    assert_eq!(repository.take_repaired(&web()), Some(drift));
}

/// Proves an entry whose installer has no backend here is a failed switch for an activate, a
/// remove and a drift read, and the installer's name is in the text.
#[test]
fn an_entry_with_no_backend_is_refused_by_activate_remove_and_drift() {
    let scratch = scratch("repo-no-backend");
    let repository = repository(&scratch);
    let archive = entry("hostkeeper-archive");
    let version = Version::new("1.0.0").expect("a version");
    let Err(SwitchError::Failed(text)) =
        repository.activate_version(&web(), &version, scratch.path(), &archive)
    else {
        panic!("not a failed switch");
    };
    assert!(
        text.contains("hostkeeper-archive") && text.contains("no backend"),
        "{text}"
    );
    let removed = repository.remove(&web(), &archive).expect_err("no backend");
    assert_eq!(removed.reason(), Reason::OrderInvalid);
    assert!(removed.detail().contains("hostkeeper-archive"));
    let drift = repository.drift(&web(), &archive).expect_err("no backend");
    assert_eq!(drift.reason(), Reason::OrderInvalid);
}

/// An apt entry of the package `web-pkg` that runs `web.service`, naming no repository, with the
/// purge bound `remove_secs` when given.
fn apt_entry(remove_secs: Option<u64>) -> Target {
    use hostkeeper_proto::hostkeeper::release::v1::{InstallerBounds, Manifest};
    Target {
        installer: "apt".to_owned(),
        manifest: Some(Manifest {
            package: Some("web-pkg".to_owned()),
            run: vec!["web.service".to_owned()],
            installer_bounds: remove_secs.map(|remove_secs| InstallerBounds {
                stage_secs: 1,
                activate_secs: 1,
                remove_secs,
            }),
            ..Manifest::default()
        }),
        ..Target::default()
    }
}

/// The update of web to 2.0.0 whose staged version directory is under the scratch packages.
fn order(scratch: &Scratch, service: &str, version: &str) -> UpdateOrder {
    let dir = scratch.path().join("packages/web/2.0.0");
    fs::create_dir_all(&dir).expect("the version directory");
    UpdateOrder {
        service: service.to_owned(),
        target_version: version.to_owned(),
        previous_version: None,
        artifact_digest: "ab".repeat(32),
        staged: hostkeeper_steps::Staged::seal(&dir).expect("sealed"),
        previous: None,
        hook_binary: PathBuf::from("bin/hook"),
        bounds: hostkeeper_steps::Bounds {
            prepare_secs: 1,
            cutover_secs: 1,
            verify_secs: 1,
            rollback_secs: 1,
            cleanup_secs: 1,
            verify_deadline_secs: 1,
            promotion_ceiling_secs: 1,
            cleanup_retries: 0,
        },
        declared_params: hostkeeper_hook::Params::default(),
        min_updater_protocol: None,
        order_try: None,
        builtin_steps: true,
    }
}

fn ended(error: SwitchError, code: i32, reason: &str) {
    let SwitchError::Ended(exit) = error else {
        panic!("not ended: {error:?}");
    };
    assert_eq!(exit.code, Some(code));
    let line: serde_json::Value =
        serde_json::from_str(exit.last_line.as_deref().expect("a line")).expect("JSON");
    assert_eq!(line["reason"], reason);
}

/// Proves the purge of an apt service that records no package runs no tool and succeeds, with and
/// without the entry's own purge bound.
#[test]
fn removing_an_apt_service_that_records_nothing_succeeds() {
    let scratch = scratch("repo-remove-none");
    let repository = repository(&scratch);
    assert_eq!(repository.remove(&web(), &apt_entry(Some(5))), Ok(()));
    assert_eq!(repository.remove(&web(), &apt_entry(None)), Ok(()));
    let no_manifest = Target {
        manifest: None,
        ..apt_entry(None)
    };
    assert_eq!(repository.remove(&web(), &no_manifest), Ok(()));
}

/// Proves the heartbeat's drift read asks the package manager for the entry's package and
/// reports a read that cannot be made as an `io_error`, whether or not the entry names one.
#[test]
fn a_drift_read_that_cannot_run_its_tool_is_an_io_error() {
    let scratch = scratch("repo-drift");
    let repository = repository(&scratch);
    let failure = repository
        .drift(&web(), &apt_entry(None))
        .expect_err("no tool runs");
    assert_eq!(failure.reason(), Reason::IoError);
    assert!(
        failure.detail().contains("dpkg-query could not be run"),
        "{failure}"
    );
    let nameless = Target {
        manifest: None,
        ..apt_entry(None)
    };
    let failure = repository
        .drift(&web(), &nameless)
        .expect_err("no tool runs");
    assert_eq!(failure.reason(), Reason::IoError);
}

/// Proves an order the installer refuses at its shape ends the activate with the installer's own
/// exit, and records no repaired drift.
#[test]
fn an_activate_whose_order_is_refused_ends_with_the_installers_exit() {
    let scratch = scratch("repo-activate-refused");
    let repository = repository(&scratch);
    let order = order(&scratch, "web", "2.0.0");
    let error = repository
        .activate(&order, &apt_entry(None))
        .expect_err("no repository");
    ended(error, 75, "coordinator_too_old");
    assert_eq!(repository.take_repaired(&web()), None);
}

/// Proves an order whose service or version is no name fails the activate and the switch back
/// before anything is asked of the host.
#[test]
fn an_order_with_a_bad_name_fails_the_switch_before_the_host_is_touched() {
    let scratch = scratch("repo-names");
    let (repository, _units, log) = repository_with_units(&scratch);
    for (service, version) in [("Not A Name", "2.0.0"), ("web", "no/version")] {
        let order = order(&scratch, service, version);
        let error = repository
            .activate(&order, &apt_entry(None))
            .expect_err("bad name");
        assert!(matches!(error, SwitchError::Failed(_)), "{error:?}");
        let error = repository
            .back(&order, &apt_entry(None), None)
            .expect_err("bad name");
        assert!(matches!(error, SwitchError::Failed(_)), "{error:?}");
    }
    assert_eq!(log.events(), vec![]);
}

/// Proves the switch back to a previous version activates it again from its sealed directory with
/// its own entry, and stops no unit.
#[test]
fn the_switch_back_activates_the_previous_version_from_its_own_entry() {
    let scratch = scratch("repo-back-previous");
    let (repository, _units, log) = repository_with_units(&scratch);
    let order = order(&scratch, "web", "2.0.0");
    let previous = Version::new("1.0.0").expect("a version");
    let error = repository
        .back(
            &order,
            &apt_entry(None),
            Some((&previous, scratch.path(), &apt_entry(None))),
        )
        .expect_err("no repository");
    ended(error, 75, "coordinator_too_old");
    assert_eq!(log.events(), vec![]);
    let archive = entry("hostkeeper-archive");
    let error = repository
        .back(
            &order,
            &apt_entry(None),
            Some((&previous, scratch.path(), &archive)),
        )
        .expect_err("no backend");
    assert!(matches!(error, SwitchError::Failed(_)), "{error:?}");
}

/// Proves the switch back of a first install stops each `run` unit in order, purges the package
/// and removes `current` when it names the target; and leaves `current` when it names another
/// version or there is none.
#[test]
fn the_switch_back_of_a_first_install_stops_purges_and_unlinks_current() {
    let scratch = scratch("repo-back-first");
    let (repository, _units, log) = repository_with_units(&scratch);
    let order = order(&scratch, "web", "2.0.0");
    let current = scratch.path().join("packages/web/current");
    assert_eq!(repository.back(&order, &apt_entry(Some(9)), None), Ok(()));
    assert_eq!(
        log.events()
            .into_iter()
            .filter(|event| matches!(event, hostkeeper_placed::testing::Event::Unit(_)))
            .count(),
        1,
        "one stop"
    );
    symlink("1.0.0", &current).expect("link");
    assert_eq!(repository.back(&order, &apt_entry(None), None), Ok(()));
    assert!(
        fs::symlink_metadata(&current).is_ok(),
        "another version stays"
    );
    fs::remove_file(&current).expect("remove");
    symlink("2.0.0", &current).expect("link");
    assert_eq!(repository.back(&order, &apt_entry(None), None), Ok(()));
    assert!(
        fs::symlink_metadata(&current).is_err(),
        "the target's link is gone"
    );
}

/// Proves the switch back fails, naming the cause, when a unit does not stop, when the installer
/// has no backend, and when `current` is no link to a version.
#[test]
fn the_switch_back_of_a_first_install_fails_when_a_step_does() {
    let scratch = scratch("repo-back-fails");
    let (repository, units, _log) = repository_with_units(&scratch);
    let order = order(&scratch, "web", "2.0.0");
    units.fail_stop("web.service");
    let error = repository
        .back(&order, &apt_entry(None), None)
        .expect_err("stuck unit");
    assert!(
        matches!(&error, SwitchError::Failed(text) if text.contains("web.service")),
        "{error:?}"
    );
    let error = repository
        .back(&order, &entry("hostkeeper-archive"), None)
        .expect_err("no backend");
    assert!(
        matches!(&error, SwitchError::Failed(text) if text.contains("no backend")),
        "{error:?}"
    );
    let current = scratch.path().join("packages/web/current");
    fs::write(&current, b"not a link").expect("a file");
    let free = Target {
        manifest: Some(hostkeeper_proto::hostkeeper::release::v1::Manifest::default()),
        installer: "apt".to_owned(),
        ..Target::default()
    };
    let error = repository
        .back(&order, &free, None)
        .expect_err("current is a file");
    assert!(matches!(error, SwitchError::Failed(_)), "{error:?}");
}

/// Proves the switch back fails, and `current` stays, when the link naming the target cannot be
/// removed.
#[test]
fn the_switch_back_fails_when_current_cannot_be_unlinked() {
    let scratch = scratch("repo-back-unlink");
    let (mut repository, _units, _log) = repository_with_units(&scratch);
    repository.fs = Arc::new(FaultFs::failing_at(scratch.path(), 1, Log::new()));
    let order = order(&scratch, "web", "2.0.0");
    let current = scratch.path().join("packages/web/current");
    symlink("2.0.0", &current).expect("link");
    let error = repository
        .back(&order, &apt_entry(None), None)
        .expect_err("cannot unlink");
    assert!(matches!(error, SwitchError::Failed(_)), "{error:?}");
    assert!(fs::symlink_metadata(&current).is_ok());
}

/// Proves every tool run of the daemon's repository installer has a unit of its own, however
/// many ports the daemon makes: two heartbeat reads of the same service, each through a port of
/// its own, run their `dpkg-query` in two units, numbered by the daemon's one counter.
///
/// Catches: a heartbeat's read and an order's step asking systemd for the same transient unit.
#[test]
fn each_tool_run_of_the_daemon_has_a_unit_of_its_own() {
    let scratch = scratch("repo-units");
    let mut repository = repository(&scratch);
    let launcher = ScriptedLauncher::new();
    let units = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&units);
    launcher.answer_tools(Arc::new(move |call: &hostkeeper_steps::ToolCall<'_>| {
        seen.lock().expect("the units").push(call.unit.to_owned());
        Ok(hostkeeper_steps::ToolExit {
            code: Some(1),
            stdout: Vec::new(),
            stderr: b"dpkg-query: no packages found matching web\n".to_vec(),
            cut: false,
        })
    }));
    repository.launcher = launcher;
    repository.tool_runs = Arc::new(std::sync::atomic::AtomicU64::new(first_tool_run()));
    for _ in 0..2 {
        let _ = repository.drift(&web(), &apt_entry(None));
    }
    let first = first_tool_run();
    assert_eq!(
        *units.lock().expect("the units"),
        [
            format!("hostkeeper-install-web-dpkg-query-{first}"),
            format!("hostkeeper-install-web-dpkg-query-{}", first + 1),
        ]
    );
    assert_eq!(first >> 32, u64::from(std::process::id()));
}
