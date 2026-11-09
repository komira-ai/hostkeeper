#![cfg(feature = "privileged")]
//! The `apt` backend of the repository installer (`hostkeeper_install::repository`,
//! packages-repositories.md section 4) as root, against the real apt, dpkg and systemd of the
//! runner and a signed fixture repository the test builds on disk (`install_apt/repo.rs`): the
//! installer's own steps (`stage`, `activate`, `remove`, the heartbeat's `drift`) over the
//! production launcher, each tool a transient unit, with its record in memory. Only the core's
//! part is left out: each version is staged into a fresh staging directory the test makes, and
//! activated from it.
//!
//! Each test makes its own host (`install_apt/rig.rs`): a key, a repository, the entry of the
//! host's configuration for it, and a guard that purges the package, releases its hold and takes
//! away its enablement link when the test ends, however it ends. The tests take turns, as the host
//! has one dpkg database. What a test reads is what the host says: `dpkg-query`, `apt-mark
//! showhold`, `systemctl is-enabled`, the files of the package and the link.
//!
//! One test runs the daemon's own order path over the same host (`install_apt/daemon.rs`): the
//! orders a coordinator sends, from the announcement of `apt` to the uninstall.
//!
//! The last test runs the backend's `apt-get update` and simulation in a container of each deb
//! release of `linux-releases.toml` (`install_apt/containers.rs`), so that what the readers of
//! `parse` rely on is checked against the apt of the oldest and the newest Debian and Ubuntu
//! the project lists. No test asserts how long anything took.

#[path = "install_apt/containers.rs"]
mod containers;
#[path = "install_apt/daemon.rs"]
mod daemon;
#[path = "install_apt/repo.rs"]
mod repo;
#[path = "install_apt/rig.rs"]
mod rig;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use containers::{Case, Repos, failed_with, simulate, simulate_args, update};
use hostkeeper_install::repository::{Class, Drift, Outcome, PackageRecord, Reason};
use hostkeeper_privileged::linux_packages::{Format, RELEASES_TEXT, parse_releases};
use repo::{DEFAULT, Flavour, PACKAGE, Spec};
use rig::{FILES, LINK, REPOSITORY, Rig, Trust, UNIT, dpkg_version, held, print, program_says};

/// The record of the package at `version` that `activate` leaves: done, with the one link of the
/// `run` unit.
fn recorded(version: &str) -> PackageRecord {
    PackageRecord {
        installer: "apt".to_owned(),
        package: PACKAGE.to_owned(),
        version: version.to_owned(),
        repository: REPOSITORY.to_owned(),
        links: BTreeSet::from([LINK.to_owned()]),
        pending: false,
    }
}

/// What the host shows of a package installed from the repository at `version` and put there by
/// `activate`: the version in dpkg's database, the hold, the program, and the one enablement link
/// of the `run` unit (the timer, which nothing runs, is not enabled).
fn assert_installed(version: &str) {
    assert_eq!(dpkg_version().as_deref(), Some(version));
    assert!(held().iter().any(|p| p == PACKAGE), "the package is held");
    assert_eq!(program_says(), format!("{PACKAGE} {version}"));
    assert_eq!(
        fs::read_link(LINK).expect("the enablement link"),
        Path::new(UNIT)
    );
    assert_eq!(
        print("systemctl", &["is-enabled", "hkaptfix.service"])
            .text
            .trim(),
        "enabled"
    );
    assert!(
        fs::symlink_metadata("/etc/systemd/system/timers.target.wants/hkaptfix-extra.timer")
            .is_err(),
        "a unit that is not a run unit is not enabled"
    );
}

/// Proves: a service's package installs from a signed repository, updates to a newer version
/// keeping its link and its hold, rolls back to the kept previous version with the repository
/// removed from the disk (nothing is fetched), and is taken off the host entirely: dpkg knows no
/// package, no file of it remains, the link and the hold are gone and the record is dropped.
///
/// Catches: an `activate` that fetches, a rollback that needs the index, an update that loses the
/// hold or the link, a purge that leaves a configuration file, a directory or the link behind.
#[test]
fn a_two_version_package_installs_updates_rolls_back_offline_and_uninstalls() {
    let rig = Rig::new("apt-lifecycle", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    assert_eq!(dpkg_version(), None, "the host starts without the package");

    let (v1, v2) = (rig.order("1.0-1"), rig.order("2.0-1"));
    let (dir1, dir2) = (rig.staging("v1"), rig.staging("v2"));
    let staged = installer.stage(&v1, &dir1).expect("stage 1.0-1");
    assert!(!staged.hook, "the fixture package has no hook");
    assert!(dir1.join("tree/usr/bin/hkaptfix").is_file());
    assert!(
        dir1.join("apt").is_dir(),
        "the version keeps its copy of the index"
    );
    assert_eq!(dpkg_version(), None, "stage installs nothing");

    let activated = installer.activate(&v1, &dir1).expect("activate 1.0-1");
    assert_eq!(activated.drift, None);
    assert_installed("1.0-1");
    assert_eq!(rig.records.get(&rig.service), Some(recorded("1.0-1")));

    installer.stage(&v2, &dir2).expect("stage 2.0-1");
    installer.activate(&v2, &dir2).expect("activate 2.0-1");
    assert_installed("2.0-1");
    assert_eq!(rig.records.get(&rig.service), Some(recorded("2.0-1")));

    fs::remove_dir_all(&rig.repo).expect("remove the repository from the disk");
    assert!(!rig.repo.exists());
    let rolled = installer
        .activate(&v1, &dir1)
        .expect("roll back to 1.0-1 offline");
    assert_eq!(rolled.drift, None);
    assert_installed("1.0-1");
    assert_eq!(rig.records.get(&rig.service), Some(recorded("1.0-1")));

    let removed = installer
        .remove(&rig.service, 120)
        .expect("remove the service");
    assert_eq!(removed.drift, None);
    assert_eq!(dpkg_version(), None);
    assert_ne!(
        print("dpkg-query", &["-W", PACKAGE]).code,
        Some(0),
        "dpkg knows no package"
    );
    for file in FILES {
        assert!(
            fs::symlink_metadata(file).is_err(),
            "{file} remains after the purge"
        );
    }
    assert!(
        fs::symlink_metadata(LINK).is_err(),
        "the enablement link remains"
    );
    assert!(!held().iter().any(|p| p == PACKAGE), "the hold remains");
    assert_eq!(rig.records.get(&rig.service), None);
}

/// Proves, through the daemon's own order path: a host with apt and the rig's repository entry
/// announces `apt`; an install order of the apt entry stages, installs, holds and enables the
/// package and starts its unit (the built-in `cutover`); an update moves it; a `force_rollback`
/// installs the kept previous version with the repository removed from the disk; an uninstall
/// order stops the unit and purges the package, leaving no file, link, hold or version
/// directory of it.
///
/// Catches: a daemon that does not announce or dispatch apt on a host that can run it, or whose
/// rollback or uninstall leaves the host other than the order says.
#[test]
fn the_daemon_installs_updates_rolls_back_and_uninstalls_an_apt_service() {
    let rig = Rig::new("apt-daemon", Trust::Entry);
    let (daemon, announced) = daemon::daemon(&rig);
    assert_eq!(announced, ["hostkeeper-archive", "apt"]);
    let finished = |reply: hostkeeper_daemon::Reply| match reply {
        hostkeeper_daemon::Reply::Finished {
            verdict, reason, ..
        } => (verdict, reason),
        other => panic!("the order did not finish: {other:?}"),
    };
    let succeeded = (
        hostkeeper_hook::Verdict::Succeeded {
            promotion_pending: false,
        },
        None,
    );
    let (one, two) = (rig.pin("1.0-1"), rig.pin("2.0-1"));
    let active = || {
        print("systemctl", &["is-active", "hkaptfix.service"])
            .text
            .trim()
            .to_owned()
    };

    assert_eq!(
        finished(daemon.order(&daemon::install("1.0-1", &one, 1))),
        succeeded
    );
    assert_installed("1.0-1");
    assert_eq!(active(), "active");
    assert_eq!(
        finished(daemon.order(&daemon::update("2.0-1", &two, 2))),
        succeeded
    );
    assert_installed("2.0-1");
    assert_eq!(active(), "active");

    fs::remove_dir_all(&rig.repo).expect("remove the repository from the disk");
    let mut back = daemon::update("1.0-1", &one, 3);
    if let Some(hostkeeper_proto::hostkeeper::agent::v1::order::Verb::Update(update)) =
        back.verb.take()
    {
        back.verb = Some(
            hostkeeper_proto::hostkeeper::agent::v1::order::Verb::ForceRollback(
                hostkeeper_proto::hostkeeper::agent::v1::ForceRollback {
                    target: update.target,
                    service: update.service,
                    version: update.version,
                    package_digest: update.package_digest,
                    index_digest: update.index_digest,
                },
            ),
        );
    }
    assert_eq!(finished(daemon.order(&back)), succeeded);
    assert_installed("1.0-1");
    assert_eq!(active(), "active");

    let packages = rig.scratch().join("packages").join(PACKAGE);
    let reply = finished(daemon.order(&daemon::uninstall(4)));
    assert_eq!(reply, succeeded);
    assert_eq!(dpkg_version(), None);
    for file in FILES {
        assert!(fs::symlink_metadata(file).is_err(), "{file} remains");
    }
    assert!(
        fs::symlink_metadata(LINK).is_err(),
        "the enablement link remains"
    );
    assert!(!held().iter().any(|p| p == PACKAGE), "the hold remains");
    assert!(!packages.exists(), "the version directories remain");
    assert_ne!(
        active(),
        "active",
        "the unit still runs after the uninstall"
    );
}

/// Proves: the version hostkeeper installed is held, so an upgrade of the host that sees a newer
/// version in the repository (`apt-get upgrade -y`) leaves it as it is; the same upgrade
/// simulated once the hold is released would take the newer one.
///
/// Catches: a hold that is not set or not kept, and an upgrade that proves nothing because the
/// repository holds no newer version.
#[test]
fn an_unattended_upgrade_leaves_the_held_version() {
    let rig = Rig::new("apt-upgrade", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let (v1, dir1) = (rig.order("1.0-1"), rig.staging("v1"));
    installer.stage(&v1, &dir1).expect("stage 1.0-1");
    installer.activate(&v1, &dir1).expect("activate 1.0-1");

    assert_eq!(rig.apt_by_hand(&["update"]).code, Some(0));
    let upgrade = rig.apt_by_hand(&["upgrade", "-y", "-o", "Dpkg::Options::=--force-confold"]);
    assert_eq!(upgrade.code, Some(0), "{}", upgrade.text);
    assert_eq!(dpkg_version().as_deref(), Some("1.0-1"), "{}", upgrade.text);
    assert!(
        held().iter().any(|p| p == PACKAGE),
        "the package is still held"
    );

    assert_eq!(print("apt-mark", &["unhold", PACKAGE]).code, Some(0));
    let simulated = rig.apt_by_hand(&["upgrade", "--simulate"]);
    assert!(
        simulated.text.contains("Inst hkaptfix [1.0-1] (3.0-1"),
        "without the hold the upgrade takes 3.0-1:\n{}",
        simulated.text
    );
}

/// Proves: a package moved by hand with `apt-get install` is reported as drift (the version found
/// and the version recorded) by the heartbeat's check, and `activate` of the recorded version
/// puts it back and reports the drift it repaired.
///
/// Catches: drift read from the wrong source, a repair that leaves the hand-installed version, and
/// an `activate` that reports nothing of what it found.
#[test]
fn a_package_moved_by_hand_is_reported_as_drift_and_put_back() {
    let rig = Rig::new("apt-drift", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let (v1, dir1) = (rig.order("1.0-1"), rig.staging("v1"));
    installer.stage(&v1, &dir1).expect("stage 1.0-1");
    installer.activate(&v1, &dir1).expect("activate 1.0-1");
    assert_eq!(
        installer.drift(&rig.service, PACKAGE, 60).expect("drift"),
        None
    );

    assert_eq!(rig.apt_by_hand(&["update"]).code, Some(0));
    let by_hand = rig.apt_by_hand(&[
        "install",
        "-y",
        "--allow-change-held-packages",
        "-o",
        "Dpkg::Options::=--force-confold",
        "hkaptfix=2.0-1",
    ]);
    assert_eq!(by_hand.code, Some(0), "{}", by_hand.text);
    assert_eq!(dpkg_version().as_deref(), Some("2.0-1"));

    let drift = Drift {
        found: Some("2.0-1".to_owned()),
        recorded: Some("1.0-1".to_owned()),
    };
    assert_eq!(
        installer.drift(&rig.service, PACKAGE, 60).expect("drift"),
        Some(drift.clone())
    );
    let repaired = installer
        .activate(&v1, &dir1)
        .expect("activate 1.0-1 again");
    assert_eq!(repaired.drift, Some(drift));
    assert_installed("1.0-1");
    assert_eq!(
        installer.drift(&rig.service, PACKAGE, 60).expect("drift"),
        None
    );
}

/// Proves: a suite signed by another key than the entry's keyring is refused at `stage` with the
/// reason `signature` (class environment, failed), before any package is read, and nothing is
/// installed.
///
/// Catches: an index trusted without its signature checked against the entry's own keyring.
#[test]
fn a_suite_signed_by_another_key_is_signature() {
    let rig = Rig::new("apt-signature", Trust::Other);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let failure = installer
        .stage(&rig.order("1.0-1"), &rig.staging("v1"))
        .expect_err("a suite signed by another key");
    assert_eq!(failure.reason(), Reason::Signature, "{failure}");
    assert_eq!(failure.reason().class(), Class::Environment);
    assert_eq!(failure.reason().outcome(), Outcome::Failed);
    assert_eq!(dpkg_version(), None);
}

/// Proves: a package re-uploaded under the same version (the index lists other bytes) is refused
/// at `stage` with `package_digest_mismatch` against the order's pin, and a version the index
/// does not list is `not_in_repository`.
///
/// Catches: a pin compared with anything but the signed index's digest, and a missing version
/// reported as a failure of the host's files.
#[test]
fn a_reuploaded_package_is_a_digest_mismatch_and_a_missing_version_is_not_in_the_repository() {
    let mut rig = Rig::new("apt-reupload", Trust::Entry);
    let original = rig.order("2.0-1").pin;
    let specs: Vec<Spec> = DEFAULT
        .iter()
        .map(|spec| match spec.version {
            "2.0-1" => Spec {
                version: spec.version,
                flavour: Flavour::Reuploaded,
            },
            _ => *spec,
        })
        .collect();
    rig.republish(&specs);
    let reuploaded = rig.order("2.0-1").pin;
    assert_ne!(original, reuploaded, "the re-upload has other bytes");

    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let order = rig.order_pinned("2.0-1", &original);
    let failure = installer
        .stage(&order, &rig.staging("reuploaded"))
        .expect_err("other bytes under the version");
    assert_eq!(failure.reason(), Reason::PackageDigestMismatch, "{failure}");
    assert_eq!(failure.reason().class(), Class::Release);

    let missing = rig.order_pinned("9.9-1", &original);
    let failure = installer
        .stage(&missing, &rig.staging("missing"))
        .expect_err("a version the index does not list");
    assert_eq!(failure.reason(), Reason::NotInRepository, "{failure}");
    assert_eq!(failure.reason().outcome(), Outcome::Deferred);
    assert_eq!(dpkg_version(), None);
}

/// Proves: `stage` refuses, with the reason the rule gives, a package whose control members hold a
/// maintainer script (`package_scripts`), whose tree has a drop-in directory or a unit of another
/// name in the unit directory (`manifest_invalid`), and whose dependency no repository has
/// (`dependency_missing`); none of them installs anything.
///
/// Catches: a path rule that lets a drop-in or a unit of another name through, and a script that
/// would run on the host.
#[test]
fn a_script_a_drop_in_a_foreign_unit_and_a_missing_dependency_are_refused_at_stage() {
    let rig = Rig::new("apt-refused", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    for (version, reason) in [
        ("0.2-script", Reason::PackageScripts),
        ("0.1-dropin", Reason::ManifestInvalid),
        ("0.3-unit", Reason::ManifestInvalid),
        ("0.9-dep", Reason::DependencyMissing),
    ] {
        let failure = installer
            .stage(&rig.order(version), &rig.staging(version))
            .expect_err(version);
        assert_eq!(failure.reason(), reason, "{version}: {failure}");
    }
    assert_eq!(dpkg_version(), None);
}

/// Proves: on the apt of each deb release the project lists (Debian 12 and 13, Ubuntu 22.04 and
/// 26.04), `apt-get update` with the backend's options reads as `Ok` for a good suite, and as the
/// reason `signature` for a suite signed by another key, for one whose `Valid-Until` passed and
/// for one that is not signed; the simulated install of a version reads as `Ok`, and the one of a
/// version with a dependency no repository has reads as `dependency_missing`.
///
/// The forms differ by release where it matters, and `parse::SIGNATURE` and `parse::UNMET` hold
/// each. Another key: Debian 12 and Ubuntu 22.04 (apt 2.x, gpgv) print `W: GPG error: ...
/// NO_PUBKEY <id>`, Ubuntu 26.04 (apt 3.1, gpgv) `W: OpenPGP signature verification failed: ...
/// NO_PUBKEY <id>`, and Debian 13 (apt 3.0, sqv) `... Missing key <fingerprint>, which is needed
/// to verify signature`; all four end in `E: The repository '...' is not signed.`. An expired
/// suite is `E: Release file for ... is expired` on every release, and an unsigned one `E: The
/// repository '...' is not signed.`. A dependency no repository has: `Depends: ... but it is not
/// installable` on every release, closed by `E: Unable to correct problems` on Debian 12, Debian
/// 13 (which adds `E: The following information from --solver 3.0 may provide additional
/// context:`) and Ubuntu 22.04, and by `E: Unable to satisfy dependencies. Reached two conflicting
/// assignments` on Ubuntu 26.04.
///
/// Catches: a release whose apt words a failure in a form the readers do not know, so that a
/// signature failure would be reported as a fetch failure.
#[test]
fn every_deb_release_gives_the_forms_the_backend_reads() {
    let docker = print("docker", &["--version"]);
    assert_eq!(
        docker.code,
        Some(0),
        "docker is missing or does not run: the job's runner has it, and this test does not skip"
    );
    let releases = parse_releases(RELEASES_TEXT).unwrap_or_else(|e| panic!("{e}"));
    let repos = Repos::build("apt-releases");
    let mut failures = Vec::new();
    let mut checked = 0;
    for release in releases.iter().filter(|r| r.format == Format::Deb) {
        checked += 1;
        let image = release.image.as_str();
        for (case, expected) in [
            (Case::Good, None),
            (Case::OtherKey, Some(Reason::Signature)),
            (Case::Expired, Some(Reason::Signature)),
            (Case::Unsigned, Some(Reason::Signature)),
        ] {
            let work = repos.work(&format!("work-{checked}-{case:?}"));
            let output = repos.apt(image, case, &work, ["update", "--error-on=any"]);
            println!(
                "{} {case:?}: update exited {:?}\n{}",
                release.name,
                output.code,
                output.text()
            );
            let result = update(&output);
            let reads = match expected {
                None => result.is_ok(),
                Some(reason) => failed_with(&result, reason),
            };
            if !reads {
                failures.push(format!(
                    "{} {case:?}: update read as {result:?}",
                    release.name
                ));
            }
            if case != Case::Good {
                continue;
            }
            let good = repos.apt(image, case, &work, simulate_args("1.0-1"));
            let result = simulate(&good);
            if result.is_err() {
                failures.push(format!(
                    "{}: simulate 1.0-1 read as {result:?}",
                    release.name
                ));
            }
            let unmet = repos.apt(image, case, &work, simulate_args("0.9-dep"));
            println!(
                "{} simulate 0.9-dep: {:?}\n{}",
                release.name,
                unmet.code,
                unmet.text()
            );
            let result = simulate(&unmet);
            if !failed_with(&result, Reason::DependencyMissing) {
                failures.push(format!(
                    "{}: simulate 0.9-dep read as {result:?}",
                    release.name
                ));
            }
        }
    }
    assert!(
        checked >= 4,
        "the list names {checked} deb releases, not the four proved"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
