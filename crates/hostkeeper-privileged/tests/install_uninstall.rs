#![cfg(all(target_os = "linux", feature = "privileged"))]
//! A package put on an enrolled host by its host group, updated and taken off it, with the real programs: the
//! coordinator and the daemon as child processes on loopback (`support::pair`), and a signed apt
//! repository on disk the daemon installs the package from through its `apt` entry
//! (`support::aptrepo`), as root against the host's systemd, apt and dpkg. The operator acts
//! through the operator API only; what the tests read is the coordinator's `hosts list` (and
//! the host's heartbeat for the service it runs), the daemon's log (each line a hook step
//! wrote, under its step's unit), and the host: the file the package installs, its unit file and
//! the link that enables it.
//!
//! What they assert: a host whose group lists the fixture service is ordered to install it and does, once,
//! when the coordinator is killed after the group is put, again while the install runs and again
//! after it; a daemon killed and started again (its boot pass) keeps the service as it was,
//! reports it and runs none of its steps again; a new desired version is installed over it, also
//! by a coordinator restarted with no word of the install (its verdicts file gone); the
//! uninstall the operator orders runs the package's `uninstall` hook and takes the file, the
//! unit and the package off the host, after which the coordinator lists the host neither as
//! running the service nor as uninstalling it; and the three orders in one sequence (install
//! 1.0.0, update to 1.1.0, uninstall) each run once, with no order refused for its try.
//!
//! The daemon keeps its packages in `/var/lib/hostkeeper/packages` and reads its repository
//! entries at `/etc/hostkeeper/repositories`; the package it installs is the host's own in dpkg's
//! database, with its unit in `/usr/lib/systemd/system` and the link that enables it in
//! `/etc/systemd/system`. A test refuses to run on a host that has any of these or the daemon's
//! other fixed paths, and removes them when it ends, the package purged
//! ([`support::pair::HostPaths`]). The tests of this file run one at a time (`--test-threads=1` in
//! privileged.yml's job first-boot). Waits look at a state a bounded number of times; nothing reads
//! a clock.

mod support;

use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::release::v1::{StepBounds, Target};
use serde_json::Value;
use support::aptrepo::{self, AptRepo, Deb, File, Service};
use support::operator_api::must;
use support::pair::{BEACON, Coordinator, Daemon, HostPaths, PACKAGES_DIR, ROLE};
use support::spawn::poll;
use support::{Bench, FIXTURE};

/// The fixture service, which is also its package; the host group of the test host lists it only
/// once the operator adds it ([`Coordinator::add_package`]).
const SERVICE: &str = "hostkeeper-ci-fixture";
/// Its one unit.
const UNIT: &str = "hostkeeper-ci-fixture.service";
/// Where the package installs the unit's file.
const UNIT_FILE: &str = "/usr/lib/systemd/system/hostkeeper-ci-fixture.service";
/// The link that enables it (its `WantedBy=`), which the installer writes.
const WANTS: &str = "/etc/systemd/system/multi-user.target.wants/hostkeeper-ci-fixture.service";
/// The file the package installs, which says its version.
const PLACED: &str = "/etc/hostkeeper-ci-fixture/fixture.conf";
/// The channel of the fixture packages' repository: the name of the host's repository entry, which
/// an order's entry names as its `repository`.
const REPOSITORY: &str = aptrepo::REPOSITORY;
/// The channel the daemon's config names: the loader requires one and nothing is fetched from it.
const NO_CHANNEL: &str = "https://releases.example";
/// The unit the package runs: the fixture's `serve`, as its main process.
const SERVICE_UNIT: &str = "[Unit]\nDescription=the first-boot tests' fixture service\n\n[Service]\n\
     Type=exec\nExecStart=/usr/bin/hostkeeper-ci-fixture serve\n\n[Install]\n\
     WantedBy=multi-user.target\n";
/// The host.
const NODE: &str = "ci-host-3";
/// The steps of an install and of an update, in the order the daemon runs them.
const STEPS: [&str; 4] = ["prepare", "cutover", "verify", "cleanup"];

/// One version of the fixture package, published in the repository, and the entry that orders it.
struct Package {
    version: &'static str,
    entry: Target,
}

/// The package of `version`: the fixture program as the service's program and as its hook (at the
/// fixed path of a package's hook), the unit that runs it, and a file that says the version.
fn package(repo: &AptRepo, version: &'static str) -> Package {
    let program = std::fs::read(FIXTURE).expect("the fixture program");
    let deb = Deb {
        package: SERVICE.to_owned(),
        version: version.to_owned(),
        files: vec![
            File::new("usr/bin/hostkeeper-ci-fixture", program.clone(), 0o755),
            File::new(
                &format!("usr/lib/{SERVICE}/hostkeeper-hook"),
                program,
                0o755,
            ),
            File::new(
                "usr/lib/systemd/system/hostkeeper-ci-fixture.service",
                SERVICE_UNIT,
                0o644,
            ),
            File::new(
                "etc/hostkeeper-ci-fixture/fixture.conf",
                format!("fixture {version}\n"),
                0o644,
            ),
        ],
    };
    let published = repo.publish(&deb);
    let service = Service {
        repository: REPOSITORY.to_owned(),
        run: vec![UNIT.to_owned()],
        health_url: None,
        params: [("fixture.mode".to_owned(), aptrepo::text("ok"))]
            .into_iter()
            .collect(),
        hook_command: vec![format!("/usr/lib/{SERVICE}/hostkeeper-hook")],
        steps: StepBounds {
            prepare: 60,
            cutover: 60,
            verify: 60,
            rollback: 60,
            cleanup: 60,
            uninstall: Some(60),
        },
        verify_deadline_secs: 300,
    };
    Package {
        version,
        entry: aptrepo::entry(&service, &published),
    }
}

/// A host with the fixture service installed, and what it took: the programs, the repository, the
/// claims on the host. The fields are dropped in their order: the programs first, the host's
/// paths (the package purged) and the scratch directory last.
struct Installed {
    daemon: Daemon,
    coordinator: Coordinator,
    _repo: AptRepo,
    _host: HostPaths,
    _bench: Bench,
    placed: PathBuf,
    v1_1: Package,
    v2: Package,
}

impl Installed {
    /// Enrolls a host, adds the fixture service to its group, sets its desired version 1.0.0,
    /// and waits until the install has finished once: the coordinator is killed and started
    /// again right after the group is put, once the install's first step has run, and once after
    /// the install.
    fn new(test: &str) -> Installed {
        let bench = Bench::new(test);
        let host = HostPaths::claim(
            &[
                UNIT_FILE,
                WANTS,
                "/etc/hostkeeper-ci-fixture",
                "/usr/bin/hostkeeper-ci-fixture",
                "/usr/lib/hostkeeper-ci-fixture",
            ],
            &[UNIT],
        )
        .purging(&[SERVICE]);
        let scratch = bench.scratch().to_path_buf();
        let repo = AptRepo::new(&scratch, "fixture-ci");
        // The host's entry as the coordinator's registry renders it, and the registry's entry
        // (put once the coordinator runs, before any set).
        repo.install_registered(aptrepo::REGISTRY_NAME);
        let placed = PathBuf::from(PLACED);
        let (v1, v1_1, v2) = (
            package(&repo, "1.0.0"),
            package(&repo, "1.1.0"),
            package(&repo, "2.0.0"),
        );

        // 1. An enrolled host that heartbeats.
        let mut coordinator = Coordinator::start(&scratch, &[]);
        coordinator.put_group(ROLE, &[BEACON]);
        must(
            "repositories put",
            coordinator
                .api()
                .put_repository(repo.registry_entry(aptrepo::REGISTRY_NAME)),
        );
        let mut daemon = Daemon::start(&scratch, &coordinator, NO_CHANNEL);
        coordinator.approve_first(NODE, &mut daemon);
        coordinator.wait_heard(NODE, &mut daemon);

        // 2. The operator adds the service to the host's group (from the registry's repository)
        // and sets the desired version; the coordinator is killed and started again at once, so
        // the group and the desired version it orders by are the ones it reads back.
        coordinator.add_package(ROLE, SERVICE, aptrepo::REGISTRY_NAME);
        set_desired(&coordinator, &v1);
        coordinator.restart();

        // 5. Once the install runs, the coordinator is killed and started again: the order
        // completes, and once.
        wait_steps(&mut daemon, "prepare", 1);
        coordinator.restart();
        let mut installed = Installed {
            daemon,
            coordinator,
            _repo: repo,
            _host: host,
            _bench: bench,
            placed,
            v1_1,
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

    /// The host runs `version` of the fixture: its file is installed with that version's content,
    /// its unit file is installed and enabled.
    fn assert_on_host(&self, version: &str) {
        assert_eq!(
            std::fs::read_to_string(&self.placed).ok().as_deref(),
            Some(format!("fixture {version}\n").as_str()),
            "the installed file is {version}'s"
        );
        assert!(Path::new(UNIT_FILE).exists(), "the unit file is installed");
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

    /// The operator takes the service out of the host's group, which orders the host to uninstall
    /// it; waits until the hook has run `uninstall` once, the host's last heartbeat lists no such
    /// service and the package is gone, and asserts the host and the coordinator's listing hold
    /// nothing of it.
    fn uninstall(&mut self) {
        let package = Path::new(PACKAGES_DIR).join(SERVICE);
        self.coordinator.remove_package(ROLE, SERVICE);
        let Installed {
            daemon,
            coordinator,
            ..
        } = self;
        poll(&format!("{NODE} reporting {SERVICE} gone"), || {
            coordinator.assert_running();
            daemon.assert_running();
            let host = coordinator
                .host(NODE)?
                .ok_or_else(|| format!("hosts list lists no {NODE}"))?;
            let beat = beat(&host);
            if ran(daemon, "uninstall") == 1 && beat.is_null() && !package.exists() {
                Ok(())
            } else {
                Err(format!("{NODE} reports {SERVICE} as {beat}"))
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
        assert!(!self.placed.exists(), "the installed file is removed");
        assert!(
            std::fs::symlink_metadata(UNIT_FILE).is_err(),
            "the unit file is removed"
        );
        assert!(
            std::fs::symlink_metadata(WANTS).is_err(),
            "the unit is disabled"
        );
        assert_eq!(ran(&self.daemon, "uninstall"), 1, "{}", self.daemon.log());
    }

    /// The coordinator never heard the host refuse an order for its try (`try_conflict` or
    /// `stale_try`), by its log.
    fn assert_no_try_refusal(&self) {
        let log = self.coordinator.log();
        assert!(
            !log.contains("refused the order for its try"),
            "the coordinator heard a refusal for the try:\n{log}"
        );
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

/// Sets the desired version of the fixture service to `package`'s, by targets alone: the one
/// `apt` entry of the package, for the host's platform.
fn set_desired(coordinator: &Coordinator, package: &Package) {
    must(
        "releases set-desired",
        coordinator.api().set_desired_targets_alone(
            SERVICE,
            package.version,
            aptrepo::targets(package.entry.clone()),
        ),
    );
}

/// Catches an install order that is not sent or not run, one that runs a second time after a
/// coordinator's restart, and a boot pass that undoes, reruns or stops reporting an installed
/// service.
#[test]
fn an_assigned_package_installs_once_across_restarts_and_the_boot_pass_keeps_it() {
    let mut host = Installed::new("install_once");

    // The daemon is killed while the service is installed. A coordinator started again while it
    // is down hears nothing of it (it lists the host from its enrollment alone, not live); the
    // daemon, started again (its boot pass), keeps the service as it was, reports it, and runs
    // none of its steps again.
    host.daemon.kill();
    host.coordinator.restart();
    host.coordinator.assert_unheard(NODE);
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

    // 3. A new desired version: the host converges to it.
    set_desired(&host.coordinator, &host.v2);
    host.wait_settled("2.0.0", 2);
    host.assert_on_host("2.0.0");
    host.assert_steps(2, "the update ran each step once");
}

/// The coordinator's first update of a host whose last order of the service
/// (the install, try 1) it holds no word of, neither in memory nor in its verdicts file, as for
/// a coordinator replaced with its in-memory fleet rebuilt from heartbeats after an install the
/// daemon ran across its own restart. The daemon is started again too, so it resends no verdict.
/// The update is ordered past the try the daemon's step record holds and runs once, at try 2 by
/// the record, and the coordinator never hears a refusal for the try.
///
/// Catches: a coordinator that numbers that update try 1, which the daemon refuses
/// `try_conflict` (its record holds the install's try 1 for another package) and the
/// coordinator then reads as the host failing the version, so the host never updates.
#[test]
fn an_update_after_a_coordinator_restart_that_forgot_the_install_runs_once() {
    let mut host = Installed::new("install_restart_update");
    host.coordinator.kill();
    std::fs::remove_file(host.coordinator.verdicts_path())
        .expect("the verdicts file, which holds the install's acknowledged verdict, is removed");
    host.daemon.restart();
    host.coordinator.start_again();
    host.wait_settled("1.0.0", 1);

    set_desired(&host.coordinator, &host.v2);
    host.wait_settled("2.0.0", 2);
    host.assert_on_host("2.0.0");
    host.assert_steps(2, "the install and the update ran each step once");
    let record = host.daemon.step_record(SERVICE);
    assert_eq!(
        (
            record["target_version"].as_str(),
            record["order_try"].as_u64()
        ),
        (Some("2.0.0"), Some(2)),
        "the update ran at the try after the install's: {record}"
    );
    host.assert_no_try_refusal();
}

/// Catches an uninstall that is not ordered or not run, one that leaves the placed file, the unit
/// or the package on the host, and a coordinator that still lists the host as running the service
/// or as uninstalling it.
#[test]
fn an_uninstall_takes_the_package_its_file_and_its_unit_off_the_host() {
    let mut host = Installed::new("install_uninstall");

    // 4. The operator takes the service off the host with an uninstall.
    host.uninstall();
}

/// One host through the three orders of a service's life, in sequence, with the real coordinator
/// and daemon: the install of 1.0.0 (try 1 by the step record), the update to 1.1.0 (try 2) and
/// the uninstall. Each order runs once; the coordinator orders each past the try of the one before,
/// whatever its order id, and never hears the host refuse one for its try.
///
/// Catches: an update after an install refused `try_conflict` because both orders went out at try
/// 1 (komira-ai/hostkeeper#109, komira-ai/hostkeeper#115); an uninstall after an update refused for
/// its try the same way; an uninstall hook that does not run on Linux, so the package, its file
/// and its unit stay (komira-ai/hostkeeper#110); and an update or uninstall that runs a step twice.
#[test]
fn install_then_update_then_uninstall_each_run_once_with_no_try_refused() {
    let mut host = Installed::new("install_update_uninstall");
    let record = host.daemon.step_record(SERVICE);
    assert_eq!(
        (
            record["target_version"].as_str(),
            record["order_try"].as_u64()
        ),
        (Some("1.0.0"), Some(1)),
        "the install ran at the first try: {record}"
    );

    // The update to 1.1.0, a new order id after the install's.
    set_desired(&host.coordinator, &host.v1_1);
    host.wait_settled("1.1.0", 2);
    host.assert_on_host("1.1.0");
    host.assert_steps(2, "the install and the update ran each step once");
    let record = host.daemon.step_record(SERVICE);
    assert_eq!(
        (
            record["target_version"].as_str(),
            record["order_try"].as_u64()
        ),
        (Some("1.1.0"), Some(2)),
        "the update ran at the try after the install's: {record}"
    );

    // The uninstall, a third order id after the update's.
    host.uninstall();
    host.assert_steps(2, "the uninstall ran no install or update step");
    host.assert_no_try_refusal();
}
