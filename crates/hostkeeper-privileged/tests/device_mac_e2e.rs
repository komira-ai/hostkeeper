#![cfg(all(target_os = "macos", feature = "privileged"))]
//! hello's macOS package installed, updated, rolled back and uninstalled end to end on a Mac, as
//! root, under the real launchd of a GitHub-hosted macOS runner, in `.github/workflows/
//! privileged.yml`'s job `macos-device` only. The real coordinator (in process) orders each
//! version; the real daemon (a launchd job of the system domain) fetches it from a release
//! channel the test serves on loopback, checks its attestation with the pinned `gh` against a
//! test trust root, unpacks it with the Mac's quarantine and code-signature checks, links its
//! plists into `/Library/LaunchDaemons` through `current`, loads them with `launchctl`, and runs
//! hello's hook in step jobs; hello's hook starts and stops its two slots, the jobs `hello.a` and
//! `hello.b`, with `launchctl` (`e2e::hello` says how the package is built).
//!
//! The test is one `#[test]` in four phases, each starting from the state the one before left:
//! the install of `1.0.0`, the update to `1.1.0`, the update to `1.2.0` whose server answers
//! `/readyz` with 503 so that hello's `verify` fails and the daemon rolls back to `1.1.0`, and
//! the uninstall. What each phase asserts is read from where the stage it witnesses leaves it:
//! the links in `/Library/LaunchDaemons` and the package directory (the switch and the placer),
//! `launchctl print system/hello.<slot>` through the daemon's own `launchctl` adapter (what
//! launchd loaded and runs: each version's plists send their output to a directory of that
//! version, so the `stdout path` launchd prints names the version of the plist it loaded), the
//! server's `/version` (the slot that serves), the daemon's `Status` on its admin socket (the
//! observer: the service live, the running executable confirmed as the installed version's), and
//! `hostkeeper ctl` against the coordinator (the heartbeat's report). Every wait is a bounded
//! count of tries of an explicit state; nothing is asserted about time.

mod e2e;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use e2e::channel::ReleaseChannel;
use e2e::coordinator::CoordinatorHandle;
use e2e::hello::{HELLO_E2E, HelloLayout, HelloPackage, SERVICE, SLOTS, label};
use e2e::mac_host::{LAUNCHD_DIR, MacHost, PLACED_DIR, is_loaded, job_fields, plist_link};
use e2e::scenario::{COORDINATED, LOCAL, pinned_gh};
use e2e::signer::FixtureSigner;
use e2e::{HOST_ID, ROLE, http_get, wait_for};
use hostkeeper_proto::hostkeeper::local::v1::{Lifecycle, Liveness, ServiceRecord};
use serde_json::Value;

/// The first release.
const V1: &str = "1.0.0";
/// A good update.
const V2: &str = "1.1.0";
/// An update whose server is not ready, so that its `verify` fails.
const V3: &str = "1.2.0";

/// The rig of the test. The fields are dropped in this order: the coordinator and the channel,
/// then the host (the daemon booted out, every job and path the test made taken away).
struct Rig {
    coordinator: CoordinatorHandle,
    channel: ReleaseChannel,
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
        let ctl = |args: &[&str]| {
            let output = self.coordinator.ctl_output(args);
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        };
        eprintln!(
            "the coordinator's view when the test failed:\nreleases status:\n{}\n\
             services list:\n{}\nrelease channel requests:\n{:#?}\nstep record:\n{:#?}\n",
            ctl(&["releases", "status", "--service", SERVICE]),
            ctl(&["services", "list"]),
            self.channel.requests(),
            self.host.step_record(),
        );
    }
}

impl Rig {
    /// The host taken over, `gh` and the fixture signer's trusted root installed, the three
    /// versions built and published on the release channel, the coordinator started, the daemon
    /// started as a launchd job and its enrollment approved with hello assigned. Nothing is
    /// installed.
    fn start() -> Rig {
        let host = MacHost::new("mac");
        host.install_gh(&pinned_gh());
        let signer = FixtureSigner::new();
        host.install_trusted_root(&signer.trusted_root_jsonl());
        let channel = ReleaseChannel::start();
        host.trust_ca(&channel.ca_pem());

        let port = e2e::free_port();
        let program = fs::read(HELLO_E2E).expect("the built hello-e2e");
        let mut packages = BTreeMap::new();
        for (version, fail_ready) in [(V1, false), (V2, false), (V3, true)] {
            let layout = HelloLayout {
                packages: host.packages(),
                record_dir: host.records().join(SERVICE),
                log_dir: host.log_dir(version),
                port,
            };
            let package = layout.build(version, fail_ready, &program);
            let bundle = signer.bundle(&package.file_name, &package.sha256);
            channel.publish(
                SERVICE,
                version,
                &package.file_name,
                package.bytes.clone(),
                bundle,
            );
            packages.insert(version, package);
        }

        let coordinator = CoordinatorHandle::start(&host.base().join("coordinator"), SERVICE, ROLE);
        host.write_config(&format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"{}\"\n\n{}",
            coordinator.heartbeat_url(),
            coordinator.ca_sha256(),
            channel.base_url(),
            signer.trusted_signers_toml(),
        ));
        host.start_daemon();
        coordinator.approve_enrollment(HOST_ID, SERVICE);
        Rig {
            coordinator,
            channel,
            packages,
            port,
            host,
        }
    }

    /// Sets hello's desired version to `version`, with its package's digest.
    fn order(&self, version: &str) {
        let digest = self.packages[version].sha256.clone();
        self.coordinator.set_desired(SERVICE, version, &digest);
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

    /// `path` with every link on the way resolved.
    fn resolved(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap_or_else(|e| panic!("resolve {}: {e}", path.display()))
    }

    /// The file of `version`'s directory at `relative`, resolved.
    fn of_version(&self, version: &str, relative: &str) -> PathBuf {
        Rig::resolved(&self.host.service_dir().join(version).join(relative))
    }

    /// The plist of `slot` placed by the daemon: a link in `/Library/LaunchDaemons` to the
    /// plist of `current`, which resolves to the plist of `version`.
    fn assert_plist_placed(&self, slot: &str, version: &str) {
        let label = label(slot);
        let link = plist_link(&label);
        let meta = fs::symlink_metadata(&link)
            .unwrap_or_else(|e| panic!("{} is not there: {e}", link.display()));
        assert!(
            meta.file_type().is_symlink(),
            "{} is a link",
            link.display()
        );
        assert_eq!(
            fs::read_link(&link).expect("the plist link"),
            self.host
                .service_dir()
                .join("current")
                .join("units")
                .join(format!("{label}.plist")),
            "{} points into current",
            link.display()
        );
        assert_eq!(
            Rig::resolved(&link),
            self.of_version(version, &format!("units/{label}.plist")),
            "{} resolves to the plist of {version}",
            link.display()
        );
    }

    /// The fields launchd prints of the job of `slot`, which must be loaded.
    fn job(&self, slot: &str) -> BTreeMap<String, String> {
        job_fields(&label(slot)).unwrap_or_else(|| panic!("launchd holds no job hello.{slot}"))
    }

    /// The job of `slot` is loaded from the plist of `version` (its `stdout path` is in that
    /// version's log directory) and runs `version`'s program (its program, through `current`,
    /// resolves to `version`'s `bin/hello`).
    fn assert_job_of(&self, slot: &str, version: &str) {
        let job = self.job(slot);
        assert_eq!(
            job.get("stdout path").map(PathBuf::from),
            Some(
                self.host
                    .log_dir(version)
                    .join(format!("hello.{slot}.out.log"))
            ),
            "hello.{slot} was loaded from the plist of {version}: {job:?}"
        );
        let program = job
            .get("program")
            .or_else(|| job.get("arguments[0]"))
            .unwrap_or_else(|| panic!("hello.{slot} has no program: {job:?}"));
        assert_eq!(
            Rig::resolved(Path::new(program)),
            self.of_version(version, "bin/hello"),
            "hello.{slot} runs the program of {version}: {job:?}"
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

    /// hello as the daemon's `Status` lists it, `None` when it is not listed.
    fn service_status(&self) -> Option<ServiceRecord> {
        let status = self
            .host
            .status()
            .unwrap_or_else(|why| panic!("Status: {why}"));
        status.services.into_iter().find(|s| s.service == SERVICE)
    }

    /// Waits until the daemon's `Status` reports hello running, live, healthy, and its running
    /// executable confirmed as `version`'s.
    fn wait_observed(&self, version: &str) {
        let mut last = None;
        wait_for(
            &format!("Status reports hello live on {version}"),
            LOCAL,
            || {
                let record = self.service_status();
                let done = record.as_ref().is_some_and(|record| {
                    let state = record.state.clone().unwrap_or_default();
                    record.lifecycle == Lifecycle::Running as i32
                        && state.liveness == Liveness::Live as i32
                        && state.version.as_deref() == Some(version)
                        && state.healthy
                });
                last = record;
                done
            },
        );
        println!("Status of hello on {version}: {last:?}");
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
/// both slots' plists are links in `/Library/LaunchDaemons` into `current`, resolving to
/// `1.0.0`'s; launchd holds both jobs, loaded from `1.0.0`'s plists and running `1.0.0`'s
/// program; `hello.a` serves `1.0.0` and `hello.b` does not run; and the daemon's `Status`
/// reports hello live and healthy, the running executable `1.0.0`'s.
fn install(rig: &Rig) {
    for slot in SLOTS {
        assert!(
            fs::symlink_metadata(plist_link(&label(slot))).is_err(),
            "no plist of hello.{slot} before the install"
        );
    }
    rig.order(V1);
    rig.wait_converged(V1);

    assert_eq!(rig.link("current").as_deref(), Some(V1));
    for slot in SLOTS {
        rig.assert_plist_placed(slot, V1);
        rig.assert_job_of(slot, V1);
    }
    rig.wait_serving("a", V1);
    rig.wait_observed(V1);
}

/// The update to `1.1.0`: `current` is `1.1.0` and `previous` `1.0.0`; both plists still are
/// links into `current` and now resolve to `1.1.0`'s; launchd holds both jobs loaded again from
/// `1.1.0`'s plists (booted out and bootstrapped: `1.0.0`'s plists sent their output elsewhere)
/// and running `1.1.0`'s program; `hello.b` serves `1.1.0` and `hello.a` no longer runs; and
/// `Status` reports `1.1.0` live and healthy.
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
    for slot in SLOTS {
        rig.assert_plist_placed(slot, V2);
        rig.assert_job_of(slot, V2);
    }
    rig.wait_serving("b", V2);
    rig.wait_observed(V2);
}

/// The update to `1.2.0`, whose server answers `/readyz` with 503: the host reports it failed
/// in `gate_release`, class `release`, reason `not_ready`, and the step record holds that
/// verdict; `current` is `1.1.0` again (`previous` the version rolled back from); both plists
/// resolve to `1.1.0`'s; launchd holds both jobs loaded from `1.1.0`'s plists and running
/// `1.1.0`'s program, so no job of `1.2.0` is loaded; `hello.b` serves `1.1.0` again and is
/// ready; `Status` reports `1.1.0` live and healthy; and the coordinator marks `1.2.0` bad and
/// wants `1.1.0`, which the host is converged on.
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
    rig.wait_serving("b", V2);
    for slot in SLOTS {
        rig.assert_plist_placed(slot, V2);
        rig.assert_job_of(slot, V2);
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

/// The uninstall the operator orders (`services unassign --uninstall true`): once the
/// coordinator lists hello uninstalling on no host, both plists are gone from
/// `/Library/LaunchDaemons`, launchd holds neither job, hello's package directory is gone with
/// every version, link and seal in it, `placed.json` (if any is left) holds no path and no
/// tombstone of hello, the hook's slot
/// markers are gone, nothing answers on the port, and `Status` no longer reports hello running.
fn uninstall(rig: &Rig) {
    rig.coordinator.ctl(&[
        "services",
        "unassign",
        "--node-id",
        HOST_ID,
        "--service",
        SERVICE,
        "--uninstall",
        "true",
    ]);
    wait_for("the uninstall of hello ends", COORDINATED, || {
        let list = rig.coordinator.ctl(&["services", "list"]);
        let uninstalling_none = list["services"].as_array().is_some_and(|all| {
            all.iter()
                .filter(|service| service["service"] == SERVICE)
                .all(|service| {
                    service["uninstalling"]
                        .as_array()
                        .is_none_or(|hosts| hosts.is_empty())
                })
        });
        uninstalling_none && !rig.host.service_dir().exists()
    });

    for slot in SLOTS {
        let label = label(slot);
        assert!(
            fs::symlink_metadata(plist_link(&label)).is_err(),
            "the plist of {label} is gone from {LAUNCHD_DIR}"
        );
        assert!(!is_loaded(&label), "launchd holds no job {label}");
    }
    assert!(
        !rig.host.service_dir().exists(),
        "hello's package directory is gone"
    );
    // An empty record is removed with its last path; one that is left holds nothing of hello.
    let placed: Value = fs::read(Path::new(PLACED_DIR).join("placed.json"))
        .map(|bytes| serde_json::from_slice(&bytes).expect("placed.json is JSON"))
        .unwrap_or(Value::Null);
    let of_hello: Vec<&String> = placed["paths"]
        .as_object()
        .map(|paths| {
            paths
                .iter()
                .filter(|(_, entry)| entry["service"] == SERVICE)
                .map(|(path, _)| path)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        of_hello,
        Vec::<&String>::new(),
        "placed.json holds no path of hello"
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
    let record = rig.service_status();
    assert!(
        record
            .as_ref()
            .is_none_or(|record| record.lifecycle != Lifecycle::Running as i32),
        "Status reports hello running after its uninstall: {record:?}"
    );
}
