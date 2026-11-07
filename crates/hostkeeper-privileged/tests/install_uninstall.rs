#![cfg(all(target_os = "linux", feature = "privileged"))]
//! A package assigned to an enrolled host, updated and taken off it, with the real programs: the
//! coordinator and the daemon as child processes on loopback (`support::pair`), and a release
//! channel the test serves over HTTPS (`support::channel`), as root against the host's systemd.
//! The operator acts through `hostkeeper ctl` only; what the tests read is the coordinator's
//! `hosts list` (and `services list` for the hosts uninstalling a service), the daemon's log (each
//! line a hook step wrote, under its step's unit), and the host: the file the package places, its
//! unit file and the link that enables it.
//!
//! What they assert: a host assigned the fixture service is ordered to install it and does, once,
//! when the coordinator is killed after the assignment, again while the install runs and again
//! after it; a daemon killed and started again (its boot pass) keeps the service as it was,
//! reports it and runs none of its steps again; a new desired version is installed over it, also
//! by a coordinator restarted with no word of the install (its verdicts file gone); and
//! the uninstall the operator orders runs the package's `uninstall` hook and takes the file, the
//! unit and the package off the host, after which the coordinator lists the host neither as
//! running the service nor as uninstalling it.
//!
//! The daemon keeps its packages in `/var/lib/hostkeeper/packages`, links and enables the fixture
//! service's unit (`hostkeeper-ci-fixture.service`) in `/etc/systemd/system`, and runs the
//! verifier in `/usr/lib/hostkeeper`; a test refuses to run on a host that has any of these or the
//! daemon's other fixed paths, and removes them when it ends ([`support::pair::HostPaths`]), with
//! the CA bundle written back ([`support::channel::HostTrust`]). The tests of this file run one at a
//! time (`--test-threads=1` in privileged.yml's job first-boot). Waits look at a state a bounded
//! number of times; nothing reads a clock.

mod support;

use std::path::{Path, PathBuf};

use serde_json::Value;
use support::channel::{Channel, Fixture, HostTrust, Package, VERIFIER_DIR};
use support::pair::{Coordinator, Daemon, HostPaths, PACKAGES_DIR, TestCa, put, rollout};
use support::spawn::poll;
use support::{Bench, FIXTURE};

/// The fixture service, which the rollout file runs on a role no test host has ([`NO_HOST_ROLE`]):
/// only the operator's assignment brings it to a host.
const SERVICE: &str = "hostkeeper-ci-fixture";
/// Its one unit.
const UNIT: &str = "hostkeeper-ci-fixture.service";
/// Where the daemon links the unit's file.
const UNIT_LINK: &str = "/etc/systemd/system/hostkeeper-ci-fixture.service";
/// The link that enables it (its `WantedBy=`).
const WANTS: &str = "/etc/systemd/system/multi-user.target.wants/hostkeeper-ci-fixture.service";
/// The role of the fixture service in the rollout file (a service runs on at least one).
const NO_HOST_ROLE: &str = "unused";
/// The host.
const NODE: &str = "ci-host-3";
/// The steps of an install and of an update, in the order the daemon runs them.
const STEPS: [&str; 4] = ["prepare", "cutover", "verify", "cleanup"];

/// A host with the fixture service installed, and what it took: the programs, the channel, the
/// claims on the host. The fields are dropped in their order: the programs first, the host's
/// paths and the scratch directory last.
struct Installed {
    daemon: Daemon,
    coordinator: Coordinator,
    _channel: Channel,
    _trust: HostTrust,
    _host: HostPaths,
    bench: Bench,
    placed: PathBuf,
    v2: Package,
}

impl Installed {
    /// Enrolls a host, sets the desired version 1.0.0 of the fixture service and assigns it to the
    /// host, and waits until the install has finished once: the coordinator is killed and started
    /// again right after the assignment, once the install's first step has run, and once after
    /// the install.
    fn new(test: &str) -> Installed {
        let bench = Bench::new(test);
        let host = HostPaths::claim(&[VERIFIER_DIR, UNIT_LINK, WANTS], &[UNIT]);
        let scratch = bench.scratch().to_path_buf();
        let ca = TestCa::new("release channel CA");
        let trust = HostTrust::install(&ca.pem());
        let channel = Channel::start(&ca);
        let placed = scratch.join("placed/fixture.conf");
        std::fs::create_dir_all(placed.parent().expect("a parent")).expect("the placed directory");
        let build = |version| {
            Fixture {
                service: SERVICE,
                version,
                program: Path::new(FIXTURE),
                placed: &placed,
                packages: Path::new(PACKAGES_DIR),
            }
            .build()
        };
        let (v1, v2) = (build("1.0.0"), build("2.0.0"));
        channel.serve(&v1);
        channel.serve(&v2);

        // 1. An enrolled host that heartbeats.
        let services = format!("[[service]]\nname = \"{SERVICE}\"\nroles = [\"{NO_HOST_ROLE}\"]\n");
        let mut coordinator = Coordinator::start(&scratch, &rollout(&services), &[]);
        let mut daemon = Daemon::start(&scratch, &coordinator, &channel.url());
        coordinator.approve_first(NODE, &mut daemon);
        coordinator.wait_heard(NODE, &mut daemon);

        // 2. The operator sets the desired version and assigns the service to the host; the
        // coordinator is killed and started again at once, so the assignment and the desired
        // version it orders by are the ones it reads back.
        set_desired(&coordinator, &scratch, &v1);
        coordinator.ctl_ok(&[
            "services",
            "assign",
            "--node-id",
            NODE,
            "--service",
            SERVICE,
        ]);
        coordinator.restart();

        // 5. Once the install runs, the coordinator is killed and started again: the order
        // completes, and once.
        wait_steps(&mut daemon, "prepare", 1);
        coordinator.restart();
        let mut installed = Installed {
            daemon,
            coordinator,
            _channel: channel,
            _trust: trust,
            _host: host,
            bench,
            placed,
            v2,
        };
        installed.wait_settled("1.0.0", 1);
        installed.assert_on_host("1.0.0");
        // Another start of the coordinator hears the host as running it, and orders nothing more.
        installed.coordinator.restart();
        installed.wait_settled("1.0.0", 1);
        installed.assert_steps(1, "the install ran each step once");
        installed
    }

    /// Waits until the hook has run `cleanup` `cleanups` times and the coordinator lists the host
    /// under the fixture service as running `version`, as the host reported it in a heartbeat
    /// since the coordinator started, with no order of the service under way.
    fn wait_settled(&mut self, version: &str, cleanups: usize) {
        let Installed {
            daemon,
            coordinator,
            ..
        } = self;
        wait_steps(daemon, "cleanup", cleanups);
        poll(&format!("{NODE} reporting {SERVICE} {version}"), || {
            coordinator.assert_running();
            daemon.assert_running();
            let host = coordinator
                .host(NODE)?
                .ok_or_else(|| format!("hosts list lists no {NODE}"))?;
            let (seen, beat) = (seen(&host), beat(&host));
            match seen["installedVersion"].as_str() {
                Some(v) if v == version && beat["updating"].is_null() => Ok(()),
                _ => Err(format!("{NODE} reports {SERVICE} as {seen} (beat {beat})")),
            }
        });
    }

    /// The host runs `version` of the fixture: its file is placed with that version's content,
    /// its unit file is linked and enabled.
    fn assert_on_host(&self, version: &str) {
        assert_eq!(
            std::fs::read_to_string(&self.placed).ok().as_deref(),
            Some(format!("fixture {version}\n").as_str()),
            "the placed file is {version}'s"
        );
        assert!(Path::new(UNIT_LINK).exists(), "the unit file is linked");
        assert!(Path::new(WANTS).exists(), "the unit is enabled");
    }

    /// Each step of an install or update ran `times` times, by the daemon's log.
    fn assert_steps(&self, times: usize, what: &str) {
        for step in STEPS {
            assert_eq!(
                ran(&self.daemon, step),
                times,
                "{what}: {step}\n{}",
                self.daemon.log()
            );
        }
    }
}

/// How many times the hook ran `step` of the fixture service and said it was done, by the
/// daemon's log over all its starts.
fn ran(daemon: &Daemon, step: &str) -> usize {
    let unit = format!(": updater-{SERVICE}-{step}-");
    let done = format!("fixture: {step} ok");
    daemon
        .log()
        .lines()
        .filter(|line| line.contains(&unit) && line.ends_with(&done))
        .count()
}

/// Waits until the hook has run `step` `times` times.
fn wait_steps(daemon: &mut Daemon, step: &str, times: usize) {
    poll(&format!("{step} run {times} times"), || {
        daemon.assert_running();
        match ran(daemon, step) {
            n if n >= times => Ok(()),
            n => Err(format!("{n} runs; the daemon's log:\n{}", daemon.log())),
        }
    });
}

/// How the fixture service sees `host` (the `services` of a `Host`), as `hosts list` gives it:
/// `null` when it does not count the host.
fn seen(host: &Value) -> Value {
    named(&host["services"], "service")
}

/// The host's heartbeat of the fixture service (the `services` of its `heartbeat`): `null` when
/// the host listed no such service in its last heartbeat.
fn beat(host: &Value) -> Value {
    named(&host["heartbeat"]["services"], "name")
}

/// The entry of `list` whose `key` is [`SERVICE`], or `null`.
fn named(list: &Value, key: &str) -> Value {
    list.as_array()
        .and_then(|all| all.iter().find(|entry| entry[key] == SERVICE))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Sets the desired version of the fixture service to `package`'s, with its digest.
fn set_desired(coordinator: &Coordinator, scratch: &Path, package: &Package) {
    let request = scratch.join(format!("desired-{}.json", package.version));
    put(
        &request,
        format!("{{\"desired\":{{\"version\":\"{}\"}}}}", package.version).as_bytes(),
        0o644,
    );
    coordinator.ctl_ok(&[
        "releases",
        "set-desired",
        "--service",
        SERVICE,
        "--package-digest",
        &package.digest,
        "--request",
        &request.display().to_string(),
    ]);
}

/// Catches an install order that is not sent or not run, one that runs a second time after a
/// coordinator's restart, and a boot pass that undoes, reruns or stops reporting an installed
/// service.
#[test]
fn an_assigned_package_installs_once_across_restarts_and_the_boot_pass_keeps_it() {
    let mut host = Installed::new("install_once");

    // The daemon is killed while the service is installed. A coordinator started again while it
    // is down hears nothing of it; the daemon, started again (its boot pass), keeps the service
    // as it was, reports it, and runs none of its steps again.
    host.daemon.kill();
    host.coordinator.restart();
    assert_eq!(host.coordinator.host(NODE), Ok(None));
    host.daemon.start_again();
    host.wait_settled("1.0.0", 1);
    host.assert_on_host("1.0.0");
    host.assert_steps(1, "the boot pass ran no step");
}

/// Catches an update that is not ordered, refused or not installed, and one that runs a step
/// twice.
#[test]
fn a_new_desired_version_is_installed_over_the_installed_one() {
    let mut host = Installed::new("install_update");
    let scratch = host.bench.scratch().to_path_buf();

    // 3. A new desired version: the host converges to it.
    set_desired(&host.coordinator, &scratch, &host.v2);
    host.wait_settled("2.0.0", 2);
    host.assert_on_host("2.0.0");
    host.assert_steps(2, "the update ran each step once");
}

/// The farm's case: the coordinator's first update of a host whose last order of the service
/// (the install, try 1) it holds no word of, neither in memory nor in its verdicts file, as for
/// a coordinator replaced with its in-memory fleet rebuilt from heartbeats after an install the
/// daemon ran across its own restart. The daemon is started again too, so it resends no verdict.
/// The update is ordered past the try the daemon's step record holds and runs once, and the
/// coordinator never hears a refusal for the try.
///
/// Catches: a coordinator that numbers that update try 1, which the daemon refuses
/// `try_conflict` (its record holds the install's try 1 for another package) and the
/// coordinator then reads as the host failing the version, so the host never updates.
#[test]
fn an_update_after_a_coordinator_restart_that_forgot_the_install_runs_once() {
    let mut host = Installed::new("install_restart_update");
    let scratch = host.bench.scratch().to_path_buf();
    host.coordinator.kill();
    std::fs::remove_file(host.coordinator.verdicts_path())
        .expect("the verdicts file, which holds the install's acknowledged verdict, is removed");
    host.daemon.restart();
    host.coordinator.start_again();
    host.wait_settled("1.0.0", 1);

    set_desired(&host.coordinator, &scratch, &host.v2);
    host.wait_settled("2.0.0", 2);
    host.assert_on_host("2.0.0");
    host.assert_steps(2, "the install and the update ran each step once");
    let log = host.coordinator.log();
    assert!(
        !log.contains("refused the order for its try"),
        "the coordinator heard a refusal for the try:\n{log}"
    );
}

/// Catches an uninstall that is not ordered or not run, one that leaves the placed file, the unit
/// or the package on the host, and a coordinator that still lists the host as running the service
/// or as uninstalling it.
#[test]
fn an_uninstall_takes_the_package_its_file_and_its_unit_off_the_host() {
    let mut host = Installed::new("install_uninstall");
    let package = Path::new(PACKAGES_DIR).join(SERVICE);

    // 4. The operator takes the service off the host with an uninstall.
    host.coordinator.ctl_ok(&[
        "services",
        "unassign",
        "--node-id",
        NODE,
        "--service",
        SERVICE,
        "--uninstall",
        "true",
    ]);
    let Installed {
        daemon,
        coordinator,
        ..
    } = &mut host;
    poll(&format!("{NODE} reporting {SERVICE} gone"), || {
        coordinator.assert_running();
        daemon.assert_running();
        let entry = coordinator.service(SERVICE)?;
        let uninstalling = entry["uninstalling"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if ran(daemon, "uninstall") == 1 && uninstalling.is_empty() && !package.exists() {
            Ok(())
        } else {
            Err(format!("{entry}"))
        }
    });
    let listed = coordinator
        .host(NODE)
        .unwrap_or_else(|e| panic!("{e}"))
        .expect("the host is still heard");
    assert_eq!(
        beat(&listed),
        Value::Null,
        "the host runs no {SERVICE}: {listed}"
    );
    assert_eq!(
        seen(&listed)["installedVersion"],
        Value::Null,
        "the host is listed as running no {SERVICE}: {listed}"
    );
    assert!(!host.placed.exists(), "the placed file is removed");
    assert!(
        std::fs::symlink_metadata(UNIT_LINK).is_err(),
        "the unit file is removed"
    );
    assert!(
        std::fs::symlink_metadata(WANTS).is_err(),
        "the unit is disabled"
    );
    assert_eq!(ran(&host.daemon, "uninstall"), 1, "{}", host.daemon.log());
}
