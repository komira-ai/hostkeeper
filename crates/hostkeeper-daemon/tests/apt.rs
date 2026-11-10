//! The daemon runs the repository installer's `apt` backend (packages-repositories.md section 4):
//! an order whose entry is an `apt` entry is staged into a sealed version directory with its
//! `target.json`, activated before `cutover`, rolled back from the kept previous version with no
//! fetch, uninstalled by a purge, and observed for drift at each heartbeat; each failure is the
//! outcome, class and reason of section 4.5. apt, dpkg and dpkg-deb are the repository
//! installer's fake, answering the daemon's own step launcher, over a `file:` repository.

mod support;
mod support_apt;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::link_adapters::DaemonObserver;
use hostkeeper_daemon::target::{self, TARGET_FILE};
use hostkeeper_daemon::{Daemon, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_link::{Drift, ServiceObserver};
use hostkeeper_placed::testing::{Event, UnitCall};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use support::{install, uninstall, update};
use support_apt::{AptRig, Inst, PROGRAM, SERVICE, UNIT, entry, package, with_entry};

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

fn failed(class: Class, reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::BeforeCommit,
            class,
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

fn deferred(reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(60),
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

fn held(version: &str) -> Option<Inst> {
    Some(Inst {
        version: version.to_owned(),
        state: 'i',
        held: true,
    })
}

fn web() -> ServiceName {
    ServiceName::new(SERVICE).expect("a name")
}

/// The version `link` (`current` or `previous`) of the service names.
fn linked(apt: &AptRig, link: &str) -> Option<String> {
    fs::read_link(apt.rig.packages.join(SERVICE).join(link))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

/// The `installed` entry of the service in `placed.json`.
fn recorded(daemon: &Daemon) -> Option<hostkeeper_placed::InstalledEntry> {
    let record = daemon
        .ctx()
        .placer()
        .store()
        .load()
        .expect("placed.json reads");
    record.installed.get(SERVICE).cloned()
}

/// The units the daemon asked the service manager to stop and start, in order.
fn unit_calls(apt: &AptRig) -> Vec<UnitCall> {
    apt.rig
        .placed_log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Unit(call @ (UnitCall::Stop(_) | UnitCall::Start(_))) => Some(call),
            _ => None,
        })
        .collect()
}

/// The order to install `version` from `package_version`, pinned to `pin`.
fn install_of(version: &str, package_version: &str, pin: &str, order_try: u64) -> Order {
    with_entry(
        install(SERVICE, version, pin, order_try),
        &entry(package_version, pin),
    )
}

/// The order to update to `version` from `package_version`, pinned to `pin`.
fn update_of(version: &str, package_version: &str, pin: &str, order_try: u64) -> Order {
    with_entry(
        update(SERVICE, version, pin, order_try),
        &entry(package_version, pin),
    )
}

/// The drift the heartbeat reports of the service.
fn drift(daemon: Arc<Daemon>) -> Option<Drift> {
    DaemonObserver::new(daemon).observe(&web()).drift
}

/// Proves the lifecycle of an `apt` service in the daemon: an install stages the version
/// (sealed, `target.json`, `tree/` and the kept `apt/` index and package), activates it before
/// `cutover` (installed, held, recorded done in `placed.json`) and restarts its unit in the
/// built-in `cutover`, with no hook step run; an update moves the package and keeps the
/// previous version; an update whose install fails in a maintainer script is rolled back to the
/// previous version from its own copy of the index, with every repository gone, and ends
/// `Failed`, `release`, `dpkg_error`; an uninstall purges the package, drops its record and
/// leaves nothing of the service.
///
/// Catches: an apt entry that the daemon defers, fetches as an artifact, or installs without
/// the hold, the record or the rollback.
#[test]
fn an_apt_package_installs_updates_rolls_back_and_is_removed_from_a_file_repository() {
    let apt = AptRig::new("apt-lifecycle");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    let three = apt.publish(package("3.0.0-1"));

    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(apt.rig.launcher.steps(SERVICE), Vec::<Step>::new());
    let dir = apt.version_dir("1.0.0");
    let sealed = target::read(&dir)
        .expect("it reads")
        .expect("a target.json");
    assert_eq!(sealed, entry("1.0.0-1", &one));
    assert!(dir.join("tree").join(PROGRAM).is_file());
    assert!(dir.join("tree/usr/lib/web/current").is_symlink());
    assert!(dir.join("apt").is_dir());
    assert!(apt.rig.packages.join(SERVICE).join("1.0.0.seal").is_file());
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    let record = recorded(&daemon).expect("an installed entry");
    assert_eq!(record.installer, "apt");
    assert_eq!(record.package.as_deref(), Some(SERVICE));
    assert_eq!(record.version.as_deref(), Some("1.0.0-1"));
    assert_eq!(record.repository.as_deref(), Some("komira-stable"));
    assert!(!record.pending);
    let link = "/etc/systemd/system/multi-user.target.wants/web.service";
    assert!(record.links.contains(link));
    assert!(apt.rig.host(link).is_symlink());
    assert_eq!(
        unit_calls(&apt),
        [
            UnitCall::Stop(UNIT.to_owned()),
            UnitCall::Start(UNIT.to_owned())
        ]
    );

    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        succeeded()
    );
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
    assert_eq!(linked(&apt, "previous").as_deref(), Some("1.0.0"));

    apt.host
        .edit(|state| state.faults.script_fails_for = Some("3.0.0-1".to_owned()));
    let mut fetches = 0;
    apt.host.edit(|state| fetches = state.calls.len());
    let ended = daemon.order(&update_of("3.0.0", "3.0.0-1", &three, 3));
    assert_eq!(ended, failed(Class::Release, "dpkg_error"));
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
    let after: Vec<String> = apt.host.calls()[fetches..]
        .iter()
        .map(|call| call.label.clone())
        .collect();
    let rollback = after
        .iter()
        .rposition(|label| label == "apt-get install")
        .expect("the rollback's install");
    assert!(
        !after[rollback..]
            .iter()
            .any(|l| l == "apt-get update" || l == "apt-get download"),
        "the rollback fetched nothing: {after:?}"
    );
    assert_eq!(
        recorded(&daemon).and_then(|r| r.version).as_deref(),
        Some("2.0.0-1")
    );

    support_apt::open_up(&apt.rig.packages.join(SERVICE));
    assert_eq!(daemon.order(&uninstall(SERVICE, 4)), succeeded());
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(recorded(&daemon), None);
    assert!(!apt.rig.host(link).exists());
    assert!(!apt.rig.packages.join(SERVICE).exists());
    assert!(apt.host.labels().contains(&"dpkg --purge".to_owned()));
}

/// Proves a rollback needs neither the repository nor the network: an update whose install
/// fails is ordered again with every repository gone; its version is on the host, so nothing is
/// fetched, and the rollback installs the previous version from its sealed copy.
///
/// Catches: a rollback, or a staged version ordered again, that runs `apt-get update` or
/// downloads a package.
#[test]
fn a_rollback_installs_the_kept_previous_version_with_the_repository_gone() {
    let apt = AptRig::new("apt-rollback-offline");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.host
        .edit(|state| state.faults.script_fails_for = Some("2.0.0-1".to_owned()));
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        failed(Class::Release, "dpkg_error")
    );
    assert!(
        apt.version_dir("2.0.0").join(TARGET_FILE).is_file(),
        "2.0.0 stays staged"
    );
    apt.host.empty_repositories();
    let seen = apt.host.calls().len();
    let again = with_entry(update(SERVICE, "2.0.0", &two, 3), &entry("2.0.0-1", &two));
    assert_eq!(daemon.order(&again), failed(Class::Release, "dpkg_error"));
    let labels: Vec<String> = apt.host.calls()[seen..]
        .iter()
        .map(|call| call.label.clone())
        .collect();
    assert!(
        !labels
            .iter()
            .any(|l| l == "apt-get update" || l == "apt-get download"),
        "nothing was fetched: {labels:?}"
    );
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
}

/// Proves drift is reported at the heartbeat and repaired by the next install: a clean host has
/// no drift; a package moved by hand is reported with the version found and the one recorded;
/// the next update installs the desired version over it and ends `Succeeded` with the reason
/// `drift_repaired`; the heartbeat after it has no drift again.
///
/// Catches: a drift report that is never sent, or a repair that fails the update.
#[test]
fn drift_is_reported_at_the_heartbeat_and_repaired_by_the_next_update() {
    let apt = AptRig::new("apt-drift");
    let daemon = Arc::new(apt.daemon());
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert_eq!(drift(Arc::clone(&daemon)), None);

    apt.host.set_installed(SERVICE, "0.9.0-1", 'i', false);
    assert_eq!(
        drift(Arc::clone(&daemon)),
        Some(Drift {
            found: Some("0.9.0-1".to_owned()),
            recorded: Some("1.0.0-1".to_owned()),
        })
    );
    let repaired = daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2));
    assert_eq!(
        repaired,
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: Some("drift_repaired".to_owned()),
            resent: false,
        }
    );
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
    assert_eq!(drift(Arc::clone(&daemon)), None);
}

/// Proves the stage's failures are the order's verdict before anything changes on the host
/// (packages-repositories.md section 4.5): a version the index does not list is `Deferred`,
/// `not_in_repository`; an index whose SHA-256 is not the pin is `Failed`, `release`,
/// `package_digest_mismatch`; a repository that cannot be reached is `Deferred`,
/// `fetch_failed`; an entry naming a repository the host does not configure is `Failed`,
/// `environment`, `repository_unknown`. None leaves a version directory or a record.
///
/// Catches: a stage failure reported as a refusal without its class, or one that leaves a half
/// version behind.
#[test]
fn a_stage_that_fails_is_the_orders_verdict_and_leaves_nothing() {
    let apt = AptRig::new("apt-stage-fails");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let unlisted = with_entry(install(SERVICE, "1.1.0", &one, 1), &entry("1.1.0-1", &one));
    assert_eq!(daemon.order(&unlisted), deferred("not_in_repository"));
    let other = "0".repeat(64);
    let mismatch = with_entry(
        install(SERVICE, "1.0.0", &other, 2),
        &entry("1.0.0-1", &other),
    );
    assert_eq!(
        daemon.order(&mismatch),
        failed(Class::Release, "package_digest_mismatch")
    );
    let mut unknown = entry("1.0.0-1", &one);
    unknown.repository = Some("elsewhere".to_owned());
    let order = with_entry(install(SERVICE, "1.0.0", &one, 3), &unknown);
    assert_eq!(
        daemon.order(&order),
        failed(Class::Environment, "repository_unknown")
    );
    apt.host
        .set_health(support_apt::URI, support_apt::Health::Unreachable);
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 4)),
        deferred("fetch_failed")
    );
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(recorded(&daemon), None);
    let left: Vec<_> = fs::read_dir(apt.rig.packages.join(SERVICE))
        .map(|dir| dir.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "nothing is left: {left:?}");
    assert!(!apt.version_dir("1.0.0").join(TARGET_FILE).exists());
}

/// Proves a stage that fails or defers says why in the daemon's log, which the verdict does
/// not carry: one line per order, at warn level for a deferral and at error level for a
/// failure, naming the service, the reason and the installer's detail.
///
/// Catches: a stage failure whose apt message is lost, leaving an operator only the reason.
#[test]
fn a_stage_that_fails_says_why_in_the_log() {
    let apt = AptRig::new("apt-stage-log");
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    let daemon = apt.daemon().with_alerts(Arc::new(move |line: &str| {
        sink.lock().expect("the lines").push(line.to_owned())
    }));
    let one = apt.publish(package("1.0.0-1"));
    let unlisted = with_entry(install(SERVICE, "1.1.0", &one, 1), &entry("1.1.0-1", &one));
    assert_eq!(daemon.order(&unlisted), deferred("not_in_repository"));
    let other = "0".repeat(64);
    let mismatch = with_entry(
        install(SERVICE, "1.0.0", &other, 2),
        &entry("1.0.0-1", &other),
    );
    assert_eq!(
        daemon.order(&mismatch),
        failed(Class::Release, "package_digest_mismatch")
    );
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(
        lines,
        [
            "hostkeeper daemon: level=warn alert=stage_failed service=web: not_in_repository: \
             the repository komira-stable lists no web 1.1.0-1 for amd64"
                .to_owned(),
            format!(
                "hostkeeper daemon: level=error alert=stage_failed service=web: \
                 package_digest_mismatch: the index's SHA-256 of web 1.0.0-1 is {one}, not the \
                 order's {other}"
            ),
        ]
    );
}
