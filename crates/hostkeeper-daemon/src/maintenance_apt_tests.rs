//! Tests of the `apt` collector and its parser, over fixtures of `apt-get --simulate -qq
//! dist-upgrade` as Ubuntu 24.04 and Debian 12 write it.

use std::collections::BTreeSet;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};

use super::*;
use crate::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS;
use crate::maintenance_collect::LinuxScope;
use crate::testing::RecordedWaits;

const UBUNTU: &str = include_str!("../tests/fixtures/maintenance/apt/ubuntu_2404.txt");
const DEBIAN: &str = include_str!("../tests/fixtures/maintenance/apt/debian12_security.txt");
const NOTHING: &str = include_str!("../tests/fixtures/maintenance/apt/nothing_to_do.txt");
const HELD: &str = include_str!("../tests/fixtures/maintenance/apt/debian12_held.txt");
const UNKNOWN: &str = include_str!("../tests/fixtures/maintenance/apt/unknown_line.txt");

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A fake `apt-get` whose update succeeds and whose simulation writes `simulation`.
fn host(simulation: &str) -> (FakeLaunchctl, Apt) {
    let fake = FakeLaunchctl::new();
    fake.always(&args(&UPDATE_ARGS), Ok(LaunchctlOutput::success("")));
    fake.always(
        &args(&SIMULATE_ARGS),
        Ok(LaunchctlOutput::success(simulation)),
    );
    let apt = Apt::new(
        Box::new(fake.clone()),
        Box::new(fake.clone()),
        RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS).0,
    );
    (fake, apt)
}

fn own() -> BTreeSet<String> {
    BTreeSet::from(["hostkeeper".to_owned()])
}

fn collect(apt: &Apt, scope: LinuxScope) -> Result<Section, String> {
    let own = own();
    apt.collect(&Look {
        scope,
        own_packages: &own,
    })
}

fn labels(section: &Section) -> Vec<&str> {
    section.due.iter().map(|item| item.label.as_str()).collect()
}

/// Proves the collector is named `apt`, the name a failed look's reason starts with.
#[test]
fn it_is_named_apt() {
    let (_, apt) = host("");
    assert_eq!(apt.name(), "apt");
}

/// Proves the host's collector is made over `/usr/bin/apt-get` (nothing is run).
#[test]
fn the_host_collector_is_apt() {
    assert_eq!(APT_GET, "/usr/bin/apt-get");
    assert_eq!(Apt::at_host(DEFAULT_PACKAGE_LOCK_WAIT_SECS).name(), "apt");
}

/// Proves a security-only look of an Ubuntu 24.04 listing reports the security updates and the new
/// kernel, each with `security` true, the kernel needing a reboot, in the order found.
#[test]
fn security_scope_reports_the_security_updates_and_the_kernel() {
    let (fake, apt) = host(UBUNTU);
    let section = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(
        section.due,
        vec![
            linux_item("libc6", "2.39-0ubuntu8.2", "2.39-0ubuntu8.3", true, false),
            linux_item(
                "libc-bin",
                "2.39-0ubuntu8.2",
                "2.39-0ubuntu8.3",
                true,
                false
            ),
            linux_item(
                "openssl",
                "3.0.13-0ubuntu3.2",
                "3.0.13-0ubuntu3.4",
                true,
                false
            ),
            linux_item(
                "linux-image-6.8.0-45-generic",
                "",
                "6.8.0-45.45",
                true,
                true
            ),
            linux_item(
                "linux-modules-6.8.0-45-generic",
                "",
                "6.8.0-45.45",
                true,
                false
            ),
            linux_item("linux-generic", "6.8.0.41.43", "6.8.0.45.47", true, false),
        ]
    );
    assert!(section.due.iter().all(|item| item.security));
    assert_eq!(section.unparsed, 0);
    assert_eq!(section.host, Default::default());
    assert_eq!(
        fake.calls(),
        vec![
            args(&["update", "-qq"]),
            args(&["--simulate", "-qq", "dist-upgrade"])
        ]
    );
}

/// Proves an update that is not a security update is due only under scope `all`.
#[test]
fn other_updates_are_due_only_under_scope_all() {
    let (_, apt) = host(UBUNTU);
    let security = collect(&apt, LinuxScope::Security).expect("collects");
    assert!(!labels(&security).contains(&"vim-tiny"));
    let all = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(
        labels(&all),
        [
            "libc6",
            "libc-bin",
            "openssl",
            "linux-image-6.8.0-45-generic",
            "linux-modules-6.8.0-45-generic",
            "linux-generic",
            "vim-tiny"
        ]
    );
    assert_eq!(
        all.due[6],
        linux_item(
            "vim-tiny",
            "2:9.1-1ubuntu7.2",
            "2:9.1-1ubuntu7.3",
            false,
            false
        )
    );
}

/// Proves a new kernel package is due under the security scope even when it is not a security
/// update, with `needs_reboot` and no `security`.
#[test]
fn a_new_kernel_is_due_and_needs_a_reboot() {
    let (_, apt) =
        host("Inst linux-image-6.1.0-25-amd64 (6.1.106-3 Debian:12.7/stable-updates [amd64])\n");
    let section = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(
        section.due,
        vec![linux_item(
            "linux-image-6.1.0-25-amd64",
            "",
            "6.1.106-3",
            false,
            true
        )]
    );
    assert!(section.due[0].needs_reboot);
    assert!(!section.due[0].security);
}

/// Proves a Debian 12 listing reports the `Debian-Security` updates as security, and a plain
/// `stable-updates` update only under scope `all`.
#[test]
fn debian_security_origins_are_security() {
    let (_, apt) = host(DEBIAN);
    let section = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(
        labels(&section),
        ["libssl3", "openssl", "curl", "linux-image-6.1.0-26-amd64"]
    );
    assert!(section.due.iter().all(|item| item.security));
    let all = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(all.due.len(), 5);
    assert_eq!(
        all.due[4],
        linux_item("tzdata", "2024a-0+deb12u1", "2024b-0+deb12u1", false, false)
    );
}

/// Proves hostkeeper's own package never appears, under either scope, though the simulation
/// upgrades it from its own repository.
#[test]
fn hostkeeper_own_packages_never_appear() {
    let (_, apt) = host(UBUNTU);
    for scope in [LinuxScope::Security, LinuxScope::All] {
        let section = collect(&apt, scope).expect("collects");
        assert!(!labels(&section).contains(&"hostkeeper"));
    }
    let listing = parse_apt_simulation(UBUNTU);
    assert!(
        listing
            .updates
            .iter()
            .any(|update| update.package == "hostkeeper")
    );
}

/// Proves a package held back is never due: it is in the kept-back list of the simulation and has
/// no `Inst` line, and the lists, summary and removals around it are known lines.
#[test]
fn held_packages_never_appear() {
    let (_, apt) = host(HELD);
    let section = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(labels(&section), ["libssl3", "openssl"]);
    assert_eq!(section.unparsed, 0);
}

/// Proves a simulation with nothing to do gives nothing due and no unparsed line.
#[test]
fn nothing_to_do_is_nothing_due() {
    let (_, apt) = host(NOTHING);
    assert_eq!(
        collect(&apt, LinuxScope::All).expect("collects"),
        Section::default()
    );
    let (_, quiet) = host("");
    assert_eq!(
        collect(&quiet, LinuxScope::All).expect("collects"),
        Section::default()
    );
}

/// Proves a line no parser knows is counted and is not a due item, while the known `Inst` line
/// beside it still is.
#[test]
fn an_unknown_line_is_counted_not_due() {
    let (_, apt) = host(UNKNOWN);
    let section = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(
        section.due,
        vec![linux_item("nano", "7.2-1", "7.2-1+deb12u1", true, false)]
    );
    assert_eq!(section.unparsed, 2);
}

/// Proves `Inst` lines of the wrong shape, one for each way a part can be missing, and lines that
/// only resemble the known ones, are each counted and give no update.
#[test]
fn lines_of_another_shape_are_counted() {
    let text = [
        "Inst lonely",
        "Inst x [1.0 (2 o [amd64])",
        "Inst x [1] nope",
        "Inst x [1] (2 o [amd64]",
        "Inst x [1] (2)",
        "Inst x [1] (2 o)",
        "Inst x [1] (2 o])",
        "The following packages",
        "Reading package lists... Finished",
        "upgraded, newly installed, to remove and not upgraded.!",
    ]
    .join("\n");
    assert_eq!(
        parse_apt_simulation(&text),
        AptListing {
            updates: Vec::new(),
            unparsed: 10
        }
    );
}

/// Proves the known lines of an unquiet run are not counted: the simulation note, progress lines
/// with and without `Done`, the summary, lists, and removals.
#[test]
fn known_lines_are_not_counted() {
    let text = "\
NOTE: This is only a simulation!
      apt-get needs root privileges for real execution.
Reading package lists... Done
Building dependency tree... Done
Reading state information...
Calculating upgrade... Done
The following packages will be upgraded:
  a b
12 upgraded, 3 newly installed, 0 to remove and 1 not upgraded.

Conf a (1 Debian:12/stable [amd64])
Remv c [1]
Purg d [1]
";
    assert_eq!(
        parse_apt_simulation(text),
        AptListing {
            updates: Vec::new(),
            unparsed: 0
        }
    );
}

/// Proves the origins are judged one by one: a label `Debian-Security` is security without a
/// suite, a suite ending `-security` is security, any one of several origins is enough, and an
/// origin without a `/` that is not `Debian-Security` is not.
#[test]
fn security_is_judged_per_origin() {
    let text = "\
Inst a [1] (2 Debian-Security:12 [amd64])
Inst b [1] (2 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])
Inst c [1] (2 hostkeeper:stable [amd64])
Inst d [1] (2 Ubuntu:24.04/noble-updates [amd64])
Inst linux-image-6.8.0-45-generic:amd64 (2 Ubuntu:24.04/noble-updates [amd64])
";
    let parsed = parse_apt_simulation(text);
    let seen: Vec<(&str, bool, bool)> = parsed
        .updates
        .iter()
        .map(|update| (update.package.as_str(), update.security, update.kernel))
        .collect();
    assert_eq!(
        seen,
        [
            ("a", true, false),
            ("b", true, false),
            ("c", false, false),
            ("d", false, false),
            ("linux-image-6.8.0-45-generic:amd64", false, true)
        ]
    );
}

/// Proves a failing `apt-get update` fails the collector with the step, its exit code and the
/// first line of its standard error, and the simulation is not run.
#[test]
fn a_failing_update_fails_and_the_simulation_is_not_run() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &args(&UPDATE_ARGS),
        Ok(LaunchctlOutput::exit(
            100,
            "E: Failed to fetch http://deb.example/dists/stable/InRelease\nE: more\n",
        )),
    );
    let (lock, waits) = RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS);
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()), lock);
    assert_eq!(
        collect(&apt, LinuxScope::All).err(),
        Some(
            "apt-get update failed with exit code 100: E: Failed to fetch http://deb.example/dists/stable/InRelease"
                .to_owned()
        )
    );
    assert_eq!(fake.calls(), vec![args(&["update", "-qq"])]);
    assert_eq!(waits.waits(), Vec::<u64>::new());
}

/// Proves an `apt-get update` that does not finish by its deadline fails the collector with the
/// deadline as the reason, and the simulation is not run.
#[test]
fn an_update_that_times_out_fails_and_the_simulation_is_not_run() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &args(&UPDATE_ARGS),
        Err(LaunchctlError::TimedOut {
            args: "update -qq".to_owned(),
        }),
    );
    let apt = Apt::new(
        Box::new(fake.clone()),
        Box::new(fake.clone()),
        RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS).0,
    );
    assert_eq!(
        collect(&apt, LinuxScope::All).err(),
        Some("apt-get update did not finish within its deadline".to_owned())
    );
    assert_eq!(fake.calls(), vec![args(&["update", "-qq"])]);
}

/// Proves a failing simulation fails the collector with its step, exit code and first error line.
#[test]
fn a_failing_simulation_fails() {
    let fake = FakeLaunchctl::new();
    fake.always(&args(&UPDATE_ARGS), Ok(LaunchctlOutput::success("")));
    fake.always(
        &args(&SIMULATE_ARGS),
        Ok(LaunchctlOutput::exit(
            100,
            "E: Unable to correct problems\n",
        )),
    );
    let apt = Apt::new(
        Box::new(fake.clone()),
        Box::new(fake.clone()),
        RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS).0,
    );
    assert_eq!(
        collect(&apt, LinuxScope::All).err(),
        Some("apt-get dist-upgrade simulation failed with exit code 100: E: Unable to correct problems".to_owned())
    );
}

/// What `apt-get update` (apt 2.8) writes when another `apt-get` holds the lists' lock.
const LISTS_LOCKED: &str = "E: Could not get lock /var/lib/apt/lists/lock. It is held by process 2413341 (apt-get)\nE: Unable to lock directory /var/lib/apt/lists/\n";

/// Proves an `apt-get update` that finds the lists' lock held is run again every 2 seconds until
/// the lock is free, and the look then goes on to the simulation and reports what is due: the
/// waits asked for are exactly two of 2 seconds, and nothing else was run.
#[test]
fn a_held_lists_lock_is_waited_for_and_the_look_goes_on() {
    let fake = FakeLaunchctl::new();
    fake.then(
        &args(&UPDATE_ARGS),
        Ok(LaunchctlOutput::exit(100, LISTS_LOCKED)),
    );
    fake.then(
        &args(&UPDATE_ARGS),
        Ok(LaunchctlOutput::exit(100, LISTS_LOCKED)),
    );
    fake.always(&args(&UPDATE_ARGS), Ok(LaunchctlOutput::success("")));
    fake.always(&args(&SIMULATE_ARGS), Ok(LaunchctlOutput::success(UBUNTU)));
    let (lock, waits) = RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS);
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()), lock);
    let waited = collect(&apt, LinuxScope::Security).expect("collects once the lock is free");
    let (_, unlocked) = host(UBUNTU);
    assert_eq!(
        waited,
        collect(&unlocked, LinuxScope::Security).expect("collects")
    );
    assert_eq!(waits.waits(), [2, 2]);
    assert_eq!(
        fake.calls(),
        [
            args(&UPDATE_ARGS),
            args(&UPDATE_ARGS),
            args(&UPDATE_ARGS),
            args(&SIMULATE_ARGS)
        ]
    );
}

/// Proves a lists' lock still held once the bound is waited fails the look with a reason that
/// names the wait and the lock, by exact equality; the last pause is cut so that the waits add up
/// to the bound (2, 2 and 1 of 5 seconds), and the simulation is not run.
#[test]
fn a_lists_lock_held_past_the_bound_fails_the_look_naming_the_lock() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &args(&UPDATE_ARGS),
        Ok(LaunchctlOutput::exit(100, LISTS_LOCKED)),
    );
    let (lock, waits) = RecordedWaits::bounded(5);
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()), lock);
    assert_eq!(
        collect(&apt, LinuxScope::All).err(),
        Some(
            "the package manager's lock was held for 5s of 5s: apt-get update failed with exit \
             code 100: E: Could not get lock /var/lib/apt/lists/lock. It is held by process \
             2413341 (apt-get)"
                .to_owned()
        )
    );
    assert_eq!(waits.waits(), [2, 2, 1]);
    assert_eq!(fake.calls(), vec![args(&UPDATE_ARGS); 4]);
}

/// Proves a lock is held only by a run that did not exit 0 and says so in one of apt's forms
/// (a lock it could not get, the dpkg frontend lock, a lock another process holds); another
/// failure, or those words in a run that exited 0, is not a held lock.
#[test]
fn a_lock_is_held_only_by_a_failed_run_that_says_so() {
    assert!(lock_held(Some(100), LISTS_LOCKED));
    assert!(lock_held(
        Some(100),
        "E: Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend)"
    ));
    assert!(lock_held(
        Some(100),
        "E: dpkg status database is locked by another process"
    ));
    assert!(lock_held(None, LISTS_LOCKED));
    assert!(!lock_held(Some(0), LISTS_LOCKED));
    assert!(!lock_held(
        Some(100),
        "E: Failed to fetch http://deb.example/"
    ));
}
