#![cfg(all(target_os = "macos", feature = "privileged"))]
//! hello's macOS package installed, updated, rolled back and uninstalled end to end on a Mac, as
//! root, under the real launchd of a GitHub-hosted macOS runner, in `.github/workflows/
//! privileged.yml`'s job `macos-device` (and pr-device.yml's copy of it). The real coordinator
//! (in process) orders each version by its entry, the one `macos-pkg` entry `release targets`
//! writes for a Mac; the daemon (the shipped daemon on the Mac's adapters, a launchd job of the
//! system domain, `e2e::mac_host` says what its program replaces) enrolls with it, beats over
//! mutual TLS, fetches the `.pkg` by the entry's pin from the files the test serves, has the
//! installer check its signature (answered as a signed package, `shared/macos_pkg.rs` says why),
//! installs it with the Mac's `installer`, and runs hello's hook in step jobs; hello's package
//! installs its program, its two plists in `/Library/LaunchDaemons` and a `postinstall` that
//! loads the two jobs `hello.a` and `hello.b`, and hello's hook starts and stops them, one at a
//! time, with `launchctl` (`e2e::hello` says how the package is built).
//!
//! The test is one `#[test]` in four phases, each starting from the state the one before left:
//! the install of `1.0.0`, the update to `1.1.0`, the update to `1.2.0` whose server answers
//! `/readyz` with 503 so that hello's `verify` fails and the daemon rolls back to `1.1.0` (its
//! kept `.pkg` installed again), and the uninstall. What each phase asserts is read from where
//! the stage it witnesses leaves it: the package directory (the switch), `pkgutil`'s receipt of
//! the package and the files and plists `installer` laid down, `launchctl print
//! system/hello.<slot>` through the daemon's own `launchctl` adapter (what launchd loaded and
//! runs), the server's `/version` (the slot that serves), and the operator API of the
//! coordinator (the heartbeat's report: `Hosts.Get` lists hello with the version the daemon's
//! observer confirmed running and its health). Every wait is a bounded count of tries of an
//! explicit state; nothing is asserted about time.

mod e2e;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use e2e::coordinator::CoordinatorHandle;
use e2e::hello::{
    HELLO_E2E, HelloLayout, HelloPackage, PROGRAM, SERVICE, SLOTS, SUPPORT, label, plist_path,
};
use e2e::mac_host::{MacHost, is_loaded, job_fields, receipt_version};
use e2e::scenario::{CONFIG_TAIL, COORDINATED, LOCAL};
use e2e::{HOST_ID, ROLE, http_get, wait_for};
use serde_json::Value;

/// The first release.
const V1: &str = "1.0.0";
/// A good update.
const V2: &str = "1.1.0";
/// An update whose server is not ready, so that its `verify` fails.
const V3: &str = "1.2.0";

/// The rig of the test. The fields are dropped in this order: the coordinator, then the host (the
/// daemon booted out, every job and path the test made taken away).
struct Rig {
    coordinator: CoordinatorHandle,
    layout: HelloLayout,
    packages: BTreeMap<&'static str, HelloPackage>,
    /// The port hello's slots serve on.
    port: u16,
    host: MacHost,
}

/// A failed test's view of the coordinator, printed while it still runs.
impl Drop for Rig {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        eprintln!(
            "the coordinator's view when the test failed:\nreleases status:\n{}\n\
             services list:\n{}\nstep record:\n{:#?}\n",
            self.coordinator.status_text(SERVICE),
            self.coordinator.services_text(),
            self.host.step_record(),
        );
    }
}

impl Rig {
    /// The host taken over, the three versions built and served to the daemon by their pins, the
    /// coordinator started, the daemon started as a launchd job and its enrollment approved with
    /// hello assigned. Nothing is installed.
    fn start() -> Rig {
        let host = MacHost::new("mac");
        let port = e2e::free_port();
        let layout = HelloLayout {
            record_dir: host.records().join(SERVICE),
            log_dir: host.log_dir(),
            port,
        };
        let program = fs::read(HELLO_E2E).expect("the built hello-e2e");
        let mut packages = BTreeMap::new();
        for (version, fail_ready) in [(V1, false), (V2, false), (V3, true)] {
            let dir = host.base().join(format!("build-{version}"));
            let package = layout.build(&dir, version, fail_ready, &program);
            let (digest, bytes) = host.serve_pkg(&package.pkg);
            assert_eq!(
                (digest.as_str(), bytes),
                (package.sha256.as_str(), package.bytes),
                "the served file is the package built"
            );
            packages.insert(version, package);
        }

        let coordinator = CoordinatorHandle::start(&host.base().join("coordinator"), SERVICE, ROLE);
        host.write_config(&format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\n{CONFIG_TAIL}",
            coordinator.heartbeat_url(),
            coordinator.ca_sha256(),
        ));
        host.start_daemon();
        coordinator.approve_enrollment(HOST_ID, SERVICE);
        Rig {
            coordinator,
            layout,
            packages,
            port,
            host,
        }
    }

    /// Sets hello's desired version to `version` by targets alone, as CI's set of a release whose
    /// entries are all native does: the one `macos-pkg` entry of its package for this Mac's
    /// platform, through the operator API.
    fn order(&self, version: &str) {
        self.coordinator.set_desired_targets_alone(
            SERVICE,
            version,
            self.packages[version].targets(),
        );
    }

    /// The test host as `releases status` lists it, `Null` before its first heartbeat.
    fn host_row(&self) -> Value {
        self.coordinator.status(SERVICE)["hosts"]
            .as_array()
            .and_then(|hosts| hosts.iter().find(|host| host["nodeId"] == HOST_ID))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// What the host's last heartbeat said of its last update of hello.
    fn last_update(&self) -> Value {
        self.host_row()["beat"]["lastUpdate"].clone()
    }

    /// Waits until the coordinator reports hello converged on `version`, the host running it.
    fn wait_converged(&self, version: &str) {
        wait_for(
            &format!("the coordinator reports the host converged on {version}"),
            COORDINATED,
            || {
                let convergence = self.coordinator.converge(SERVICE);
                convergence["state"] == "CONVERGENCE_STATE_CONVERGED"
                    && convergence["desired"] == version
                    && self.host_row()["beat"]["version"] == version
            },
        );
    }

    /// The target of the link `name` (`current`, `previous`) of hello's package directory.
    fn link(&self, name: &str) -> Option<String> {
        fs::read_link(self.host.service_dir().join(name))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// The plist of `slot`, which `installer` put in `/Library/LaunchDaemons`, is a file of its
    /// own (no link) with the text of `version`'s package: the plist of a failing build for
    /// `fail_ready`.
    fn assert_plist_installed(&self, slot: &str, fail_ready: bool) {
        let path = plist_path(slot);
        let meta = fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("{} is not there: {e}", path.display()));
        assert!(
            meta.is_file(),
            "{} is a file of the package",
            path.display()
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the plist"),
            self.layout.plist(slot, fail_ready),
            "{} is the plist of the package",
            path.display()
        );
    }

    /// The fields launchd prints of the job of `slot`, which must be loaded.
    fn job(&self, slot: &str) -> BTreeMap<String, String> {
        job_fields(&label(slot)).unwrap_or_else(|| panic!("launchd holds no job hello.{slot}"))
    }

    /// launchd holds the job of `slot`, loaded from the plist of the package: its program is the
    /// one the package installs.
    fn assert_job_loaded(&self, slot: &str) {
        let job = self.job(slot);
        let program = job
            .get("program")
            .or_else(|| job.get("arguments[0]"))
            .unwrap_or_else(|| panic!("hello.{slot} has no program: {job:?}"));
        assert_eq!(
            program, PROGRAM,
            "hello.{slot} runs the program the package installs: {job:?}"
        );
    }

    /// Whether launchd runs the job of `slot` (`state = running`).
    fn running(&self, slot: &str) -> bool {
        job_fields(&label(slot))
            .is_some_and(|job| job.get("state").map(String::as_str) == Some("running"))
    }

    /// Waits until `serving` runs and the other slot does not, and hello answers `/version`
    /// with `version` and `/readyz` with 200.
    fn wait_serving(&self, serving: &str, version: &str) {
        let idle = if serving == "a" { "b" } else { "a" };
        wait_for(
            &format!("hello.{serving} alone serves {version}"),
            LOCAL,
            || {
                let answers = http_get(self.port, "/version")
                    .is_some_and(|(status, body)| status == 200 && body.trim() == version);
                let ready = http_get(self.port, "/readyz").is_some_and(|(status, _)| status == 200);
                answers && ready && self.running(serving) && !self.running(idle)
            },
        );
    }

    /// hello as the host's last heartbeat lists it (the `services` of the `heartbeat` that
    /// `Hosts.Get` answers), `None` when the host or the entry is not listed.
    fn service_beat(&self) -> Option<Value> {
        let reply = self.coordinator.api().get_host(HOST_ID).ok()?;
        reply["host"]["heartbeat"]["services"]
            .as_array()?
            .iter()
            .find(|beat| beat["name"] == SERVICE)
            .cloned()
    }

    /// Waits until the host's heartbeat lists hello on `version` and healthy: the daemon's
    /// observer saw its running executable as the package's, and its health check passes.
    fn wait_observed(&self, version: &str) {
        let mut last = None;
        wait_for(
            &format!("the heartbeat reports hello healthy on {version}"),
            COORDINATED,
            || {
                let beat = self.service_beat();
                let done = beat.as_ref().is_some_and(|beat| {
                    beat["version"] == version && beat["health"] == "HEALTH_HEALTHY"
                });
                last = beat;
                done
            },
        );
        println!("The heartbeat's entry of hello on {version}: {last:?}");
    }
}

/// The four phases in order; see the module documentation.
#[test]
fn hello_installs_updates_rolls_back_and_uninstalls_under_launchd() {
    let rig = Rig::start();
    install(&rig);
    update(&rig);
    failed_verify_rolls_back(&rig);
    uninstall(&rig);
}

/// The install of `1.0.0`: once the coordinator reports the host on it, `current` is `1.0.0`;
/// `pkgutil` has the receipt of the package at `1.0.0`, its program and both plists are installed
/// (the plists files of the package, not links); launchd holds both jobs, loaded from the
/// plists, running the package's program; `hello.a` serves `1.0.0` and `hello.b` does not run;
/// the heartbeat reports hello healthy on `1.0.0`.
fn install(rig: &Rig) {
    assert_eq!(receipt_version(), None, "no receipt before the install");
    assert!(
        fs::symlink_metadata(SUPPORT).is_err(),
        "nothing of hello is installed before the install"
    );
    for slot in SLOTS {
        assert!(
            fs::symlink_metadata(plist_path(slot)).is_err(),
            "no plist of hello.{slot} before the install"
        );
    }
    rig.order(V1);
    rig.wait_converged(V1);

    assert_eq!(rig.link("current").as_deref(), Some(V1));
    assert_eq!(receipt_version().as_deref(), Some(V1));
    assert!(Path::new(PROGRAM).is_file(), "the program is installed");
    for slot in SLOTS {
        rig.assert_plist_installed(slot, false);
        rig.assert_job_loaded(slot);
    }
    rig.wait_serving("a", V1);
    rig.wait_observed(V1);
}

/// The update to `1.1.0`: `current` is `1.1.0` and `previous` `1.0.0`; the receipt says `1.1.0`;
/// both plists are the package's and launchd still holds both jobs, running the package's
/// program; `hello.b` serves `1.1.0` and `hello.a` no longer runs; and the heartbeat reports
/// `1.1.0` healthy.
fn update(rig: &Rig) {
    rig.order(V2);
    rig.wait_converged(V2);
    wait_for(
        "the coordinator records 1.1.0 as last good",
        COORDINATED,
        || rig.coordinator.status(SERVICE)["marks"]["lastGood"] == V2,
    );

    assert_eq!(rig.link("current").as_deref(), Some(V2));
    assert_eq!(rig.link("previous").as_deref(), Some(V1));
    assert_eq!(receipt_version().as_deref(), Some(V2));
    for slot in SLOTS {
        rig.assert_plist_installed(slot, false);
        rig.assert_job_loaded(slot);
    }
    rig.wait_serving("b", V2);
    rig.wait_observed(V2);
}

/// The update to `1.2.0`, whose server answers `/readyz` with 503: the host reports it failed
/// in `gate_release`, class `release`, reason `not_ready`, and the step record holds that
/// verdict; `current` is `1.1.0` again (`previous` the version rolled back from) and the receipt
/// says `1.1.0` (the kept package of `1.1.0` installed again, with no fetch); both plists are
/// `1.1.0`'s again, not the failing build's; launchd holds both jobs, running the package's
/// program; `hello.b` serves `1.1.0` again and is ready; the heartbeat reports `1.1.0` healthy;
/// and the coordinator marks `1.2.0` bad and wants `1.1.0`, which the host is converged on.
fn failed_verify_rolls_back(rig: &Rig) {
    rig.order(V3);
    wait_for("the host reports an end of 1.2.0", COORDINATED, || {
        let update = rig.last_update();
        update["target"] == V3 && update["outcome"] != Value::Null
    });
    let update = rig.last_update();
    assert_eq!(update["outcome"], "OUTCOME_FAILED", "{update}");
    assert_eq!(update["phase"], "FAILURE_PHASE_GATE_RELEASE", "{update}");
    assert_eq!(update["class"], "FAILURE_CLASS_RELEASE", "{update}");
    assert_eq!(update["reason"], "not_ready", "{update}");
    let record = rig
        .host
        .step_record()
        .expect("the step record of the update");
    assert_eq!(record["target_version"], V3, "{record}");
    let failed = &record["state"]["verdict"]["Failed"];
    assert_eq!(failed["phase"], "gate_release", "{record}");
    assert_eq!(failed["class"], "release", "{record}");

    assert_eq!(rig.link("current").as_deref(), Some(V2));
    assert_eq!(rig.link("previous").as_deref(), Some(V3));
    assert_eq!(receipt_version().as_deref(), Some(V2));
    rig.wait_serving("b", V2);
    for slot in SLOTS {
        rig.assert_plist_installed(slot, false);
        rig.assert_job_loaded(slot);
    }
    rig.wait_observed(V2);

    wait_for(
        "the coordinator marks 1.2.0 bad and wants 1.1.0",
        COORDINATED,
        || {
            let status = rig.coordinator.status(SERVICE);
            let bad = status["marks"]["bad"]
                .as_array()
                .is_some_and(|bad| bad.iter().any(|version| version == V3));
            bad && status["desired"]["version"] == V2
        },
    );
    rig.wait_converged(V2);
}

/// `placed.json`, `Null` when there is none (an empty record is removed with its last path).
fn placed(rig: &Rig) -> Value {
    fs::read(rig.host.fixture().join("placed").join("placed.json"))
        .map(|bytes| serde_json::from_slice(&bytes).expect("placed.json is JSON"))
        .unwrap_or(Value::Null)
}

/// The uninstall the operator orders (`Services.Unassign` with `uninstall`): once the
/// coordinator lists hello uninstalling on no host and the daemon dropped hello's tombstone from
/// `placed.json` (the last write of the uninstall, after the package directory and the records
/// are removed; the coordinator's list follows the heartbeats, so either may come first and the
/// wait holds for both), both plists are gone from
/// `/Library/LaunchDaemons`, launchd holds neither job, the program and its directory are gone,
/// `pkgutil` forgot the receipt, hello's package directory is gone with every version, link and
/// seal in it, `placed.json` holds no record of hello and no tombstone, the hook's slot markers
/// are gone, nothing answers on the port, and the heartbeat no longer reports hello healthy.
fn uninstall(rig: &Rig) {
    rig.coordinator.unassign_uninstall(HOST_ID, SERVICE);
    wait_for("the uninstall of hello ends", COORDINATED, || {
        let list = rig.coordinator.services();
        let uninstalling_none = list["services"].as_array().is_some_and(|all| {
            all.iter()
                .filter(|service| service["service"] == SERVICE)
                .all(|service| {
                    service["uninstalling"]
                        .as_array()
                        .is_none_or(|hosts| hosts.is_empty())
                })
        });
        uninstalling_none
            && !rig.host.service_dir().exists()
            && placed(rig)["uninstalling"][SERVICE].is_null()
    });

    for slot in SLOTS {
        let label = label(slot);
        assert!(
            fs::symlink_metadata(plist_path(slot)).is_err(),
            "the plist of {label} is gone from /Library/LaunchDaemons"
        );
        assert!(!is_loaded(&label), "launchd holds no job {label}");
    }
    assert!(
        fs::symlink_metadata(SUPPORT).is_err(),
        "hello's directory of /Library/Application Support is gone"
    );
    assert_eq!(receipt_version(), None, "pkgutil forgot the receipt");
    assert!(
        !rig.host.service_dir().exists(),
        "hello's package directory is gone"
    );
    // An empty record is removed with its last entry; one that is left holds nothing of hello.
    let placed = placed(rig);
    assert_eq!(
        placed["installed"][SERVICE],
        Value::Null,
        "placed.json holds no record of hello: {placed}"
    );
    assert_eq!(
        placed["uninstalling"][SERVICE],
        Value::Null,
        "hello's tombstone is gone: {placed}"
    );
    let record_dir = rig.host.records().join(SERVICE);
    assert!(
        !record_dir.join("slots").exists(),
        "the hook removed the slots' markers"
    );
    assert_eq!(
        http_get(rig.port, "/version"),
        None,
        "nothing answers on the port"
    );
    wait_for(
        "the heartbeat lists no healthy hello after its uninstall",
        COORDINATED,
        || {
            rig.service_beat()
                .is_none_or(|beat| beat["health"] != "HEALTH_HEALTHY")
        },
    );
}
