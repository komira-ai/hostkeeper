//! Tests of the `apt` collector and its parser, over fixtures of `apt-get --simulate -qq
//! dist-upgrade` as Ubuntu 24.04 and Debian 12 write it.

use std::collections::BTreeSet;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};

use super::*;
use crate::maintenance_collect::LinuxScope;

const UBUNTU: &str = include_str!("../tests/fixtures/maintenance/apt/ubuntu_2404.txt");
const DEBIAN: &str = include_str!("../tests/fixtures/maintenance/apt/debian12_security.txt");
const NOTHING: &str = include_str!("../tests/fixtures/maintenance/apt/nothing_to_do.txt");
const HELD: &str = include_str!("../tests/fixtures/maintenance/apt/debian12_held.txt");
const UNKNOWN: &str = include_str!("../tests/fixtures/maintenance/apt/unknown_line.txt");
/// An Ubuntu 24.04 host booted from a newer kernel series whose simulation installs two packages it
/// does not have yet, a library (a security update) and an older series' kernel image (not one),
/// beside one installed security update.
const NEW_PACKAGES: &str =
    include_str!("../tests/fixtures/maintenance/apt/ubuntu_2404_new_packages.txt");
/// The same host once the installed security update is installed: the two new packages alone.
const ONLY_NEW: &str = "\
Inst libdrm-amdgpu1 (2.4.125-1ubuntu0.1~24.04.2 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])
Inst linux-image-6.8.0-151-generic (6.8.0-151.151 Ubuntu:24.04/noble-updates [amd64])
";

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
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()));
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
    assert_eq!(Apt::at_host().name(), "apt");
}

/// Proves a security-only look of an Ubuntu 24.04 listing reports the installed security updates
/// and the upgraded kernel meta-package, each with `security` true, in the order found; the new
/// kernel image and modules it pulls in (not installed yet) are not due under that scope.
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

/// Proves an installed kernel image with a newer version is due under the security scope even
/// when it is not a security update, with `needs_reboot` and no `security`.
#[test]
fn an_installed_kernel_upgrade_is_due_and_needs_a_reboot() {
    let (_, apt) =
        host("Inst linux-image-amd64 [6.1.99-1] (6.1.106-3 Debian:12.7/stable-updates [amd64])\n");
    let section = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(
        section.due,
        vec![linux_item(
            "linux-image-amd64",
            "6.1.99-1",
            "6.1.106-3",
            false,
            true
        )]
    );
    assert!(section.due[0].needs_reboot);
    assert!(!section.due[0].security);
}

/// Proves a kernel image not installed yet is due under the scope `all`, which installs it
/// (`dist-upgrade`), needing a reboot, and not under the scope `security`, whose install upgrades
/// installed packages only.
#[test]
fn a_new_kernel_image_is_due_only_under_scope_all() {
    let (_, apt) =
        host("Inst linux-image-6.1.0-25-amd64 (6.1.106-3 Debian:12.7/stable-updates [amd64])\n");
    let security = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(security, Section::default());
    let all = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(
        all.due,
        vec![linux_item(
            "linux-image-6.1.0-25-amd64",
            "",
            "6.1.106-3",
            false,
            true
        )]
    );
}

/// Proves the look of a host whose simulation installs a new library (a security update) and a
/// new kernel image beside an installed security update reports, under the scope `security`,
/// the installed security update alone, which is what that scope's install names; under `all`
/// it reports all three, new ones with no `from`.
///
/// Catches: a security look that reports a package the install never installs, so that the check
/// after the install fails `update_due` on it and the host halts.
#[test]
fn new_packages_are_not_due_under_scope_security() {
    let (_, apt) = host(NEW_PACKAGES);
    let security = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(
        security.due,
        vec![linux_item(
            "python3-jwt",
            "2.7.0-1ubuntu0.4",
            "2.7.0-1ubuntu0.5",
            true,
            false
        )]
    );
    assert_eq!(security.unparsed, 0);
    let all = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(
        all.due,
        vec![
            linux_item(
                "libdrm-amdgpu1",
                "",
                "2.4.125-1ubuntu0.1~24.04.2",
                true,
                false
            ),
            linux_item(
                "python3-jwt",
                "2.7.0-1ubuntu0.4",
                "2.7.0-1ubuntu0.5",
                true,
                false
            ),
            linux_item(
                "linux-image-6.8.0-151-generic",
                "",
                "6.8.0-151.151",
                false,
                true
            ),
        ]
    );
}

/// Proves a simulation that installs only packages not installed yet, a security update and a
/// kernel image, has nothing due under the scope `security`, and both under `all`.
///
/// Catches: a host with nothing the security install would install reported as having updates
/// due, so that an update of it halts on `update_due`.
#[test]
fn only_new_packages_are_nothing_due_under_scope_security() {
    let (_, apt) = host(ONLY_NEW);
    assert_eq!(
        collect(&apt, LinuxScope::Security).expect("collects"),
        Section::default()
    );
    let all = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(
        labels(&all),
        ["libdrm-amdgpu1", "linux-image-6.8.0-151-generic"]
    );
}

/// Proves the rule is one for the look and the install: for each update of each listing,
/// [`AptUpdate::is_due`] is true exactly when the package is not hostkeeper's own and, under
/// `security`, is installed and a security update or a kernel image; under `all` whatever it is.
#[test]
fn is_due_follows_the_scope_the_installed_state_and_the_own_packages() {
    let own = own();
    let security = Look {
        scope: LinuxScope::Security,
        own_packages: &own,
    };
    let all = Look {
        scope: LinuxScope::All,
        own_packages: &own,
    };
    let update = |package: &str, from: &str, security: bool, kernel: bool| AptUpdate {
        package: package.to_owned(),
        from: from.to_owned(),
        to: "2".to_owned(),
        security,
        kernel,
    };
    let cases = [
        // (update, due under security, due under all)
        (update("a", "1", true, false), true, true),
        (update("a", "1", false, true), true, true),
        (update("a", "1", false, false), false, true),
        (update("a", "", true, false), false, true),
        (update("a", "", false, true), false, true),
        (update("a", "", false, false), false, true),
        (update("hostkeeper", "1", true, true), false, false),
        (update("hostkeeper:amd64", "", true, true), false, false),
    ];
    for (update, under_security, under_all) in cases {
        assert_eq!(update.is_due(&security), under_security, "{update:?}");
        assert_eq!(update.is_due(&all), under_all, "{update:?}");
    }
}

/// Proves a Debian 12 listing reports the installed `Debian-Security` updates as security, and a
/// plain `stable-updates` update and a kernel image not installed yet only under scope `all`.
#[test]
fn debian_security_origins_are_security() {
    let (_, apt) = host(DEBIAN);
    let section = collect(&apt, LinuxScope::Security).expect("collects");
    assert_eq!(labels(&section), ["libssl3", "openssl", "curl"]);
    assert!(section.due.iter().all(|item| item.security));
    let all = collect(&apt, LinuxScope::All).expect("collects");
    assert_eq!(
        labels(&all),
        [
            "libssl3",
            "openssl",
            "curl",
            "linux-image-6.1.0-26-amd64",
            "tzdata"
        ]
    );
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
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()));
    assert_eq!(
        collect(&apt, LinuxScope::All).err(),
        Some(
            "apt-get update failed with exit code 100: E: Failed to fetch http://deb.example/dists/stable/InRelease"
                .to_owned()
        )
    );
    assert_eq!(fake.calls(), vec![args(&["update", "-qq"])]);
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
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()));
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
    let apt = Apt::new(Box::new(fake.clone()), Box::new(fake.clone()));
    assert_eq!(
        collect(&apt, LinuxScope::All).err(),
        Some("apt-get dist-upgrade simulation failed with exit code 100: E: Unable to correct problems".to_owned())
    );
}
