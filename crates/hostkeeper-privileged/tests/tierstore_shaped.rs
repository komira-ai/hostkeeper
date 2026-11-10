#![cfg(feature = "privileged")]
//! A tierstore-shaped package updated end to end, as root, under the real systemd of the host:
//! the real coordinator (in process) and the real daemon (the `hostkeeper` program under a
//! `Type=notify` unit) take the package's `apt` entry from a signed apt repository the test builds
//! on disk, stage it (the repository's signature, the pin, the simulation, the extracted tree
//! sealed), install it with apt, switch `current`, and run its hook in step units; the fixture's
//! hook drains in `prepare` and starts the new release beside the old in `cutover`
//! (`tests/bin/tierstore_like.rs`).
//!
//! Each test builds its own state with the shared helpers of `e2e::scenario`, so none depends on
//! another or on their order; each starts by checking it is root on a host whose service manager
//! is systemd, and fails with that reason otherwise. Every wait is a bounded count of tries of an
//! explicit condition; nothing is asserted about time.
//!
//! What each assertion reads is produced only by the real stage it witnesses: the version
//! directory, its mode, its tree and its seal (the stage), dpkg's database and hold (the
//! installer), the `current` and `previous` links (the switch), the version string an instance
//! serves, read from the release file of the package it runs from (the installer and the hook),
//! `systemctl is-active` (the units), the step record and the hook's record files (the step
//! launcher), and the operator API of the coordinator (the heartbeat's report).

mod e2e;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use e2e::host_root::is_active;
use e2e::scenario::{COORDINATED, LOCAL, Scenario, V1, V2, V3};
use e2e::{FIXTURE_BINARY, SERVICE, instance, served_version, wait_for};
use hostkeeper_steps::selfupdate::Layout;
use serde_json::Value;

/// The unit directory of the host the package installs its units in, and the directory the
/// installer links the units it enables from.
const UNIT_DIR: &str = "/usr/lib/systemd/system";
const WANTS_DIR: &str = "/etc/systemd/system/multi-user.target.wants";

/// The version of the fixture package dpkg has fully installed; `None` when it has none.
fn dpkg_version() -> Option<String> {
    let shown = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}\t${db:Status-Abbrev}\n", SERVICE])
        .output()
        .expect("run dpkg-query");
    if !shown.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&shown.stdout).into_owned();
    let (version, status) = text.trim_end().split_once('\t')?;
    // The abbreviation is the wanted state, the state and the error: `hi ` is installed and held.
    (status.as_bytes().get(1) == Some(&b'i')).then(|| version.to_owned())
}

/// The packages `apt-mark showhold` lists.
fn held() -> Vec<String> {
    let shown = Command::new("apt-mark")
        .arg("showhold")
        .output()
        .expect("run apt-mark");
    String::from_utf8_lossy(&shown.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Whether systemd reports `unit` active.
fn active(port: u16) -> bool {
    is_active(&instance(port))
}

/// Every file and directory under `dir`, by path relative to it, with its mode and its bytes:
/// what "byte-identical" compares.
fn tree(dir: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let mut entries: Vec<_> = fs::read_dir(&next)
            .expect("a directory of the version")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = fs::symlink_metadata(&path).expect("an entry's metadata");
            let relative = path
                .strip_prefix(dir)
                .expect("under the version")
                .to_path_buf();
            if meta.is_dir() {
                found.push((relative, meta.mode(), Vec::new()));
                pending.push(path);
            } else {
                found.push((
                    relative,
                    meta.mode(),
                    fs::read(&path).expect("a file's bytes"),
                ));
            }
        }
    }
    found.sort();
    found
}

/// The whole path from a desired version to a running new release, with the drain holding the
/// old release up until it is released: `1.0.0` installed and serving, the drain gate busy, the
/// coordinator told `1.1.0`. The daemon stages `1.1.0` from the repository once, extracts it
/// root-owned and mode 0555 with a seal naming the package's pin and a `target.json` that is the
/// entry it was ordered through, and runs `prepare`, which waits while the old instances keep
/// serving `1.0.0`; once the gate is idle, `current` is `1.1.0` and `previous` `1.0.0`, dpkg has
/// `1.1.0` held, the new instances serve `1.1.0` and are active, the old ones are stopped, the
/// coordinator reports the host converged on `1.1.0`, and exactly `1.0.0` and `1.1.0` are on disk
/// with their seals.
#[test]
fn a_tierstore_shaped_package_updates_end_to_end() {
    let s = Scenario::installed("upd");
    let old = s.package(V1).clone();
    let new = s.package(V2).clone();
    let dir = s.host.service_dir().join(V2);
    assert!(!dir.exists(), "1.1.0 is not staged before it is ordered");
    assert_eq!(dpkg_version().as_deref(), Some(V1));

    s.host.set_gate(true);
    s.order(V2);

    wait_for("1.1.0 is staged and sealed", LOCAL, || {
        dir.is_dir() && s.host.service_dir().join(format!("{V2}.seal")).is_file()
    });
    let meta = fs::metadata(&dir).expect("the version directory");
    assert_eq!(meta.uid(), 0, "the version directory is root's");
    assert_eq!(meta.permissions().mode() & 0o7777, 0o555);
    assert_eq!(s.seal(V2)["package_digest"], new.sha256.as_str());
    assert_eq!(
        hostkeeper_daemon::target::read(&dir)
            .expect("target.json")
            .as_ref(),
        Some(&new.entry),
        "the version directory holds the entry it was ordered through"
    );
    assert_eq!(
        fs::read(dir.join("tree/usr/bin/tierstore-like")).expect("the staged program"),
        fs::read(FIXTURE_BINARY).expect("the built fixture program"),
        "the staged tree holds the package's program"
    );

    wait_for("prepare waits on the busy drain gate", LOCAL, || {
        s.host.record(&format!("prepare-{V2}.waiting")).is_some()
    });
    let record = s.host.step_record().expect("the step record of the update");
    assert_eq!(record["target_version"], V2);
    assert_eq!(record["step"], "prepare");
    assert_eq!(record["state"]["state"], "running");
    for port in old.ports {
        assert_eq!(
            served_version(port).as_deref(),
            Some(V1),
            "the old instance on {port} serves while prepare drains"
        );
    }
    assert_eq!(s.link("current").as_deref(), Some(V1));

    s.host.set_gate(false);
    wait_for("current points at 1.1.0", LOCAL, || {
        s.link("current").as_deref() == Some(V2)
    });
    assert_eq!(s.link("previous").as_deref(), Some(V1));
    s.wait_converged(V2);
    assert_eq!(dpkg_version().as_deref(), Some(V2));
    assert!(
        held().iter().any(|package| package == SERVICE),
        "the installed package is held"
    );
    for port in new.ports {
        assert_eq!(
            served_version(port).as_deref(),
            Some(V2),
            "new instance {port}"
        );
        assert!(active(port), "the new instance {port} is active");
    }
    for port in old.ports {
        assert!(!active(port), "the old instance {port} is stopped");
        assert_eq!(served_version(port), None, "nothing answers on {port}");
    }
    assert_eq!(
        s.version_dirs(),
        BTreeSet::from([V1.to_owned(), V2.to_owned()])
    );
    for version in [V1, V2] {
        assert_eq!(
            s.seal(version)["package_digest"],
            s.package(version).sha256.as_str()
        );
    }
}

/// A release that starts but fails its verification leaves the host serving the old release
/// under real systemd: from `1.1.0` converged, `1.2.0` (whose `verify` exits 1) is staged once
/// and sealed; its `cutover` starts its instances; `verify` fails; the daemon runs `rollback` from
/// the new release's hook, switches `current` back to `1.1.0` (`previous` is the version rolled
/// back from) and installs `1.1.0` again from the copy it kept, so dpkg has `1.1.0`, the old
/// instances serve `1.1.0` and the new ones are stopped; the update ends `Failed`, phase
/// `gate_release`, class `release`, and `cleanup` never runs; the coordinator marks `1.2.0` bad and
/// puts the desired version back to `1.1.0`, which the host is converged on, with no second try of
/// `1.2.0`. The end-to-end witness of `a_failed_verify_rolls_back_and_cleanup_never_runs`.
#[test]
fn a_failed_verify_rolls_back_and_the_old_release_serves() {
    let s = Scenario::updated("rbk");
    let old = s.package(V2).clone();
    let bad = s.package(V3).clone();

    s.order(V3);

    wait_for("the host reports 1.2.0 failed", COORDINATED, || {
        let update = s.last_update();
        update["target"] == V3 && update["outcome"] == "OUTCOME_FAILED"
    });
    let update = s.last_update();
    assert_eq!(update["phase"], "FAILURE_PHASE_GATE_RELEASE", "{update}");
    assert_eq!(update["class"], "FAILURE_CLASS_RELEASE", "{update}");
    assert_eq!(update["reason"], "fixture_verify_failed", "{update}");

    assert!(s.host.service_dir().join(V3).is_dir(), "1.2.0 was staged");
    assert_eq!(s.seal(V3)["package_digest"], bad.sha256.as_str());
    assert_eq!(
        s.host.record(&format!("cutover-{V3}")).as_deref(),
        Some("ok")
    );
    assert_eq!(
        s.host.record(&format!("verify-{V3}")).as_deref(),
        Some("failed")
    );
    assert_eq!(
        s.host.record(&format!("rollback-{V3}")).as_deref(),
        Some("ok")
    );
    assert_eq!(
        s.host.record(&format!("cleanup-{V3}")),
        None,
        "cleanup never ran"
    );

    assert_eq!(s.link("current").as_deref(), Some(V2));
    assert_eq!(s.link("previous").as_deref(), Some(V3));
    assert_eq!(dpkg_version().as_deref(), Some(V2));
    // The rollback hook starts the old units and returns at once (`Type=exec` is active when the
    // program is executed, before it has bound its port), so the answer is waited for.
    s.wait_serving(V2);
    for port in old.ports {
        assert_eq!(
            served_version(port).as_deref(),
            Some(V2),
            "old instance {port}"
        );
        assert!(active(port), "the old instance {port} is active again");
    }
    for port in bad.ports {
        assert!(!active(port), "the new instance {port} is stopped");
    }

    let record = s.host.step_record().expect("the step record of the update");
    assert_eq!(record["target_version"], V3);
    assert_eq!(record["state"]["state"], "finished", "{record}");
    let failed = &record["state"]["verdict"]["Failed"];
    assert_eq!(failed["phase"], "gate_release", "{record}");
    assert_eq!(failed["class"], "release", "{record}");
    let tries = record["order_try"].clone();

    // The coordinator holds 1.2.0 bad and wants 1.1.0 again: no order of 1.2.0 can follow
    // (a bad version is never ordered), and the one try above is the host's only one.
    wait_for(
        "the coordinator marks 1.2.0 bad and wants 1.1.0",
        COORDINATED,
        || {
            let status = s.status();
            let bad_marked = status["marks"]["bad"]
                .as_array()
                .is_some_and(|bad| bad.iter().any(|version| version == V3));
            bad_marked && status["desired"]["version"] == V2
        },
    );
    s.wait_converged(V2);
    assert_ne!(s.convergence()["desired"], V3);
    let record = s.host.step_record().expect("the step record");
    assert_eq!(
        (
            record["target_version"].clone(),
            record["order_try"].clone()
        ),
        (Value::from(V3), tries),
        "the host did not try 1.2.0 again"
    );
}

/// The previous version kept on disk is enough to repair a host while the repository is gone, and
/// the pruning rule keeps exactly the two versions: from `1.1.0` converged (`previous` `1.0.0`,
/// both sealed), the repository removed from the disk, a force to `1.0.0` through the
/// coordinator. The force installs `1.0.0` from the copy of the index and the package it kept, so
/// it needs nothing the repository held: `1.0.0`'s tree and seal are byte-identical after and its
/// tree still matches its seal; `current` is `1.0.0` and `previous` `1.1.0`; dpkg has `1.0.0`; the
/// instances of `1.0.0` are active and serve it, those of `1.1.0` are stopped; exactly `1.0.0`
/// and `1.1.0` are on disk with their seals; the host reports the forced order succeeded.
#[test]
fn force_back_to_previous_needs_no_fetch() {
    let s = Scenario::updated("frc");
    let target = s.package(V1).clone();
    let left = s.package(V2).clone();
    assert_eq!(s.link("current").as_deref(), Some(V2));
    assert_eq!(s.link("previous").as_deref(), Some(V1));
    assert_eq!(
        s.version_dirs(),
        BTreeSet::from([V1.to_owned(), V2.to_owned()])
    );
    let layout = Layout::package(&s.host.service_dir());
    layout
        .check_tree(V1)
        .expect("1.0.0's tree matches its seal");
    let seal_path = s.host.service_dir().join(format!("{V1}.seal"));
    let seal_before = fs::read(&seal_path).expect("1.0.0's seal");
    let tree_before = tree(&s.host.service_dir().join(V1));

    fs::remove_dir_all(s.repo.dir()).expect("remove the repository from the disk");
    assert!(!s.repo.dir().exists(), "the repository is gone");

    s.coordinator.force(SERVICE, V1, e2e::HOST_ID);

    // The verdict of the force order: the heartbeat wire carries no `forced` flag (the
    // coordinator's `update_result` says so), so the force's own order id is the witness.
    wait_for("the host reports the force to 1.0.0", COORDINATED, || {
        let update = s.last_update();
        update["target"] == V1
            && update["orderId"]
                .as_str()
                .is_some_and(|id| id.starts_with("force-"))
            && update["outcome"] != Value::Null
    });
    let update = s.last_update();
    assert_eq!(update["outcome"], "OUTCOME_SUCCEEDED", "{update}");
    // `releases status` reports the standing force in the service's convergence; `releases
    // converge` leaves it out.
    assert_eq!(
        s.status()["convergence"]["force"]["version"],
        V1,
        "the force stands"
    );
    assert!(!s.repo.dir().exists(), "the force needed no repository");

    assert_eq!(s.link("current").as_deref(), Some(V1));
    assert_eq!(s.link("previous").as_deref(), Some(V2));
    assert_eq!(dpkg_version().as_deref(), Some(V1));
    // A unit is active once its program is executed, a moment before it answers: wait for it.
    s.wait_serving(V1);
    for port in target.ports {
        assert!(active(port), "the instance {port} of 1.0.0 is active");
        assert_eq!(served_version(port).as_deref(), Some(V1), "instance {port}");
    }
    for port in left.ports {
        assert!(!active(port), "the instance {port} of 1.1.0 is stopped");
    }
    assert_eq!(
        s.version_dirs(),
        BTreeSet::from([V1.to_owned(), V2.to_owned()])
    );
    for version in [V1, V2] {
        layout
            .check_tree(version)
            .unwrap_or_else(|error| panic!("{version}'s tree matches its seal: {error}"));
    }
    assert_eq!(fs::read(&seal_path).expect("1.0.0's seal"), seal_before);
    assert_eq!(tree(&s.host.service_dir().join(V1)), tree_before);
    s.wait_converged(V1);
}

/// Every socket under `dir`, as paths (links are not followed).
fn sockets_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).expect("a readable directory").flatten() {
            let kind = entry.file_type().expect("an entry's type");
            if kind.is_socket() {
                found.push(entry.path());
            } else if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    found
}

/// The daemon is ready under a `Type=notify` unit only once its start is done: with `1.0.0`
/// installed, the daemon stopped and the boot id it recorded removed, `systemctl start` returns
/// (which, for `Type=notify`, is after the daemon sent `READY=1`) with the boot id recorded again
/// (the start pass ran before the notice), the unit active and of type notify, no socket bound
/// (none anywhere in the test's tree), the installer's enablement link of the run unit as it was,
/// and the fixture's instances still serving.
#[test]
fn the_daemon_is_ready_under_a_notify_unit_once_its_start_is_done() {
    let s = Scenario::installed("boot");
    let installed = s.package(V1).clone();
    let unit = instance(installed.ports[0]);
    let wants = Path::new(WANTS_DIR).join(&unit);
    let unit_file = Path::new(UNIT_DIR).join(&unit);
    assert_eq!(
        fs::read_link(&wants).expect("the enablement link the installer made"),
        unit_file
    );

    s.daemon.stop();
    assert!(!s.daemon.is_active(), "the daemon is stopped");
    let recorded_at = s.host.state().join(".boot-id");
    fs::remove_file(&recorded_at).expect("remove the recorded boot id");
    s.daemon.start();

    let unit_state = s.daemon.show(&["Type", "ActiveState"]);
    assert_eq!(unit_state.get("Type").map(String::as_str), Some("notify"));
    assert_eq!(
        unit_state.get("ActiveState").map(String::as_str),
        Some("active")
    );
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").expect("the boot id");
    let recorded = fs::read_to_string(&recorded_at).expect("the boot id is recorded again");
    assert_eq!(recorded.trim(), boot_id.trim());
    assert_eq!(
        fs::read_link(&wants).expect("the enablement link"),
        unit_file
    );
    assert_eq!(sockets_under(s.host.base()), Vec::<PathBuf>::new());
    for port in installed.ports {
        assert_eq!(served_version(port).as_deref(), Some(V1), "instance {port}");
    }
}
