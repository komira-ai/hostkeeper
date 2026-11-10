//! The daemon runs the `macos-pkg` installer on a Mac that announces it (packages-repositories.md
//! section 4.7): an order whose entry is a `macos-pkg` entry has its `.pkg` fetched by its pin
//! alone, staged by the installer's `stage` (the signature checked against the host's signers
//! first) into a sealed version directory with its `target.json`, activated before `cutover`,
//! rolled back from the kept previous version, uninstalled by the installer's `remove`, and
//! observed for drift at each heartbeat. A Mac that holds no `macos-pkg` installer (no signers
//! file, or no `installer` or `pkgutil`) announces none, and its order is deferred
//! `installer_not_listed`. The Mac is the installer's fake, over a scratch root.

mod support;
mod support_macos_pkg;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::link_adapters::DaemonObserver;
use hostkeeper_daemon::target::{self, TARGET_FILE};
use hostkeeper_daemon::{Daemon, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_install::Tool;
use hostkeeper_link::{Drift, ServiceObserver};
use hostkeeper_release::Installer;
use support::uninstall;
use support_macos_pkg::{
    IDENTIFIER, LABEL, MacRig, SERVICE, fp, install_of, package, signed_output, update_of,
};

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

fn hello() -> ServiceName {
    ServiceName::new(SERVICE).expect("a name")
}

/// The version `link` (`current` or `previous`) of the service names.
fn linked(mac: &MacRig, link: &str) -> Option<String> {
    fs::read_link(mac.rig.packages.join(SERVICE).join(link))
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

/// The names in the service's package directory, sorted.
fn staged(mac: &MacRig) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(mac.rig.packages.join(SERVICE))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The drift the heartbeat reports of the service.
fn drift(daemon: Arc<Daemon>) -> Option<Drift> {
    DaemonObserver::new(daemon).observe(&hello()).drift
}

/// Catches: a Mac that announces `macos-pkg` and is ordered the `.pkg` but defers it, fetches
/// it through an attestation, installs it without the record, or leaves it unsealed. The
/// heartbeat lists the installer; the install fetches the `.pkg` by its pin alone, stages it
/// (sealed, `target.json`, the kept `.pkg` and the tree), runs `installer` before `cutover`, and
/// records the package's identifier and version in `placed.json`; the built-in `cutover`
/// restarts the job; no hook step runs.
#[test]
fn a_mac_that_announces_macos_pkg_installs_the_pkg_it_is_ordered() {
    let mac = MacRig::new("pkg-install");
    let daemon = mac.daemon();
    assert_eq!(daemon.ctx().announcement.names(), ["macos-pkg"]);
    let one = mac.publish("1.0.0", package("1.0.0"));

    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());

    assert_eq!(
        mac.rig.pins.asked(),
        [(SERVICE.to_owned(), one.digest.clone())],
        "the .pkg was fetched once, by its pin"
    );
    assert_eq!(mac.rig.pins.entries(), [Some(one.clone())]);
    assert_eq!(mac.rig.launcher.steps(SERVICE), Vec::<Step>::new());
    let dir = mac.version_dir("1.0.0");
    assert_eq!(target::read(&dir).expect("it reads"), Some(one.clone()));
    assert!(dir.join(TARGET_FILE).is_file());
    assert!(dir.join("artifact").is_file());
    assert!(dir.join("macos-pkg/artifact.pkg").is_file());
    assert!(
        dir.join("tree/Library/Application Support/hello/hello")
            .is_file()
    );
    assert!(mac.rig.packages.join(SERVICE).join("1.0.0.seal").is_file());
    assert_eq!(linked(&mac, "current").as_deref(), Some("1.0.0"));
    assert_eq!(
        mac.mac.receipt(IDENTIFIER).map(|receipt| receipt.version),
        Some("1.0.0".to_owned())
    );
    assert!(
        mac.real("/Library/Application Support/hello/hello")
            .is_file()
    );
    assert_eq!(mac.mac.enabled(LABEL), Some(true));
    let record = recorded(&daemon).expect("an installed entry");
    assert_eq!(record.installer, "macos-pkg");
    assert_eq!(record.package.as_deref(), Some(IDENTIFIER));
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert_eq!(record.repository, None);
    assert!(!record.pending);
    assert!(
        record
            .created_dirs
            .contains("/Library/Application Support/hello"),
        "{:?}",
        record.created_dirs
    );
    assert!(
        staged(&mac).iter().all(|name| !name.starts_with(".fetch")),
        "the fetched .pkg is gone: {:?}",
        staged(&mac)
    );
    assert_eq!(drift(Arc::new(daemon)), None);
}

/// Catches: an unsigned `.pkg`, or one signed by a chain whose leaf is none of the host's
/// signers (an intermediate that is a signer does not count), expanded, installed or staged.
/// Each is refused at `stage`, `Failed` before the commit, `environment`, `signature`: nothing
/// is left in the packages directory, no `installer` ran, nothing is recorded, and the order of
/// a signed package for the same version after it installs.
#[test]
fn an_unsigned_or_wrongly_signed_pkg_is_refused_at_stage() {
    let trusted = "signed by a certificate trusted by macOS";
    let leaf = support_macos_pkg::leaf();
    // Each case: its name, the host's signer, and what `pkgutil --check-signature` answers.
    let cases = [
        (
            "unsigned",
            leaf.clone(),
            (
                1,
                "Package \"hello.pkg\":\n   Status: no signature\n".to_owned(),
            ),
        ),
        (
            "another leaf",
            leaf.clone(),
            (0, signed_output(&fp("7e"), trusted)),
        ),
        (
            "the signer an intermediate",
            fp("11"),
            (0, signed_output(&leaf, trusted)),
        ),
        (
            "an untrusted chain",
            leaf.clone(),
            (
                1,
                signed_output(&leaf, "signed by a certificate that is not trusted"),
            ),
        ),
    ];
    for (name, signer, (code, check)) in cases {
        let mut mac = MacRig::new(&format!("pkg-signature-{}", name.replace(' ', "-")));
        mac.signers = hostkeeper_install::macos_pkg::signers::Signers::new(vec![
            hostkeeper_install::macos_pkg::signers::Fingerprint::parse(&signer).expect("hex"),
        ])
        .expect("one signer");
        let daemon = mac.daemon();
        let bad = mac.publish("1.0.0", package("1.0.0").with_check(code, &check));

        assert_eq!(
            daemon.order(&install_of("1.0.0", &bad, 1)),
            failed(Class::Environment, "signature"),
            "{name}"
        );
        assert!(
            staged(&mac).iter().all(|n| n.starts_with('.')),
            "{name}: nothing staged: {:?}",
            staged(&mac)
        );
        assert!(!mac.version_dir("1.0.0").exists(), "{name}");
        assert_eq!(mac.mac.runs_of(Tool::Pkgutil, "--expand-full"), 0, "{name}");
        assert!(mac.mac.calls_of(Tool::Installer).is_empty(), "{name}");
        assert_eq!(mac.mac.receipt(IDENTIFIER), None, "{name}");
        assert_eq!(recorded(&daemon), None, "{name}");
        assert_eq!(linked(&mac, "current"), None, "{name}");
    }
}

/// Catches: a Mac without the signers file, or without `installer` or `pkgutil`, that lists
/// `macos-pkg` or runs a `macos-pkg` order. It announces `hostkeeper-archive` alone, holds no
/// installer, and its order of a `.pkg` is deferred `installer_not_listed`, with nothing
/// fetched.
#[test]
fn a_mac_without_the_signers_file_or_its_tools_announces_no_macos_pkg() {
    let mac = MacRig::new("pkg-not-announced");
    let tools: Arc<dyn hostkeeper_install::Tools> = mac.mac.clone();
    let without_signers =
        hostkeeper_daemon::macos_pkg::ports(true, mac.host.clone(), None, Some(Arc::clone(&tools)));
    assert!(without_signers.is_none(), "no signers file");
    let not_a_mac = hostkeeper_daemon::macos_pkg::ports(
        false,
        mac.host.clone(),
        Some(mac.signers.clone()),
        Some(Arc::clone(&tools)),
    );
    assert!(not_a_mac.is_none(), "not a Mac");
    let no_tools = hostkeeper_daemon::macos_pkg::ports(
        true,
        mac.host.clone(),
        Some(mac.signers.clone()),
        None,
    );
    assert!(no_tools.is_none(), "a build with no Mac tools");
    for tool in ["usr/sbin/installer", "usr/sbin/pkgutil"] {
        let path = mac.root.join(tool);
        fs::rename(&path, path.with_extension("gone")).expect("the tool gone");
        assert!(mac.pkg_ports().is_none(), "no {tool}");
        fs::rename(path.with_extension("gone"), &path).expect("the tool back");
    }
    assert!(mac.pkg_ports().is_some());

    let announcement = hostkeeper_daemon::macos_pkg::announced(Default::default(), None);
    assert_eq!(announcement.installers, Vec::<Installer>::new());
    let daemon = Daemon::new(
        support::config(),
        mac.rig.dirs(),
        hostkeeper_daemon::Ports {
            announcement,
            ..mac.rig.ports()
        },
        support::OWN,
    );
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", &one, 1)),
        Reply::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(hostkeeper_daemon::entry::NOT_LISTED_RETRY_SECS),
            },
            reason: Some("installer_not_listed".to_owned()),
            resent: false,
        }
    );
    assert!(mac.rig.pins.asked().is_empty(), "nothing fetched");
    assert!(mac.mac.calls().is_empty(), "no tool ran");
}

/// Catches: an update of a `.pkg` that does not replace the files of the version before it, a
/// failed install that leaves the new version in place, or a rollback that fetches the old
/// `.pkg` again. An update installs 1.1.0 over 1.0.0 (a file 1.0.0 had and 1.1.0 does not is
/// deleted) and keeps 1.0.0 as `previous`; an update whose `installer` leaves a file that is not
/// the sealed copy is rolled back by the `activate` of the kept 1.1.0, with no fetch, and ends
/// `Failed`, `release`, `installer_error`.
#[test]
fn a_pkg_updates_and_a_failed_install_rolls_back_to_the_kept_version() {
    let mac = MacRig::new("pkg-update");
    let daemon = mac.daemon();
    let one = mac.publish(
        "1.0.0",
        package("1.0.0").file("old-only", "gone in 1.1.0", 0o644),
    );
    let two = mac.publish("1.1.0", package("1.1.0"));
    // 1.2.0's `installer` puts other bytes in one file: the install is not proved.
    let three = mac.publish(
        "1.2.0",
        package("1.2.0").with_install(support_macos_pkg::pkg::Install::OtherBytes {
            path: "/Library/Application Support/hello/hello".to_owned(),
            bytes: b"not the sealed copy".to_vec(),
        }),
    );
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());
    assert!(
        mac.real("/Library/Application Support/hello/old-only")
            .is_file()
    );

    assert_eq!(daemon.order(&update_of("1.1.0", &two, 2)), succeeded());
    assert_eq!(linked(&mac, "current").as_deref(), Some("1.1.0"));
    assert_eq!(linked(&mac, "previous").as_deref(), Some("1.0.0"));
    assert_eq!(
        mac.mac.receipt(IDENTIFIER).map(|receipt| receipt.version),
        Some("1.1.0".to_owned())
    );
    assert!(
        !mac.real("/Library/Application Support/hello/old-only")
            .exists(),
        "the file 1.1.0 does not list is deleted"
    );
    assert_eq!(
        recorded(&daemon).and_then(|record| record.version),
        Some("1.1.0".to_owned())
    );

    let fetched = mac.rig.pins.asked().len();
    let ended = daemon.order(&update_of("1.2.0", &three, 3));
    assert!(
        matches!(
            &ended,
            Reply::Finished {
                verdict: Verdict::Failed { class: Class::Release, .. },
                reason: Some(reason),
                ..
            } if reason == "installer_error"
        ),
        "{ended:?}"
    );
    assert_eq!(
        fs::read(mac.real("/Library/Application Support/hello/hello")).expect("the program"),
        b"program hello 1.1.0",
        "the kept version's file is back"
    );
    assert_eq!(linked(&mac, "current").as_deref(), Some("1.1.0"));
    assert_eq!(
        mac.mac.receipt(IDENTIFIER).map(|receipt| receipt.version),
        Some("1.1.0".to_owned()),
        "the kept 1.1.0 installed again"
    );
    assert_eq!(
        recorded(&daemon).and_then(|record| record.version),
        Some("1.1.0".to_owned())
    );
    assert_eq!(
        mac.rig.pins.asked().len(),
        fetched + 1,
        "only 1.2.0 was fetched; the rollback took the kept .pkg"
    );
}

/// Catches: an uninstall of a `.pkg` service that leaves its files, its receipt or its record.
/// The uninstall removes every file of the package, forgets its receipt, and drops the record
/// and the service's directory.
#[test]
fn a_pkg_service_is_uninstalled_by_its_receipt() {
    let mac = MacRig::new("pkg-uninstall");
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());
    support_macos_pkg::open_up(&mac.rig.packages);

    assert_eq!(daemon.order(&uninstall(SERVICE, 2)), succeeded());

    assert_eq!(mac.mac.receipt(IDENTIFIER), None);
    assert!(
        !mac.real("/Library/Application Support/hello/hello")
            .exists()
    );
    assert!(!mac.real("/Library/Application Support/hello").exists());
    assert_eq!(recorded(&daemon), None);
    assert!(!mac.rig.packages.join(SERVICE).exists());
}

/// Catches: a heartbeat that reports no drift of a `.pkg` installed by hand at another version,
/// or one whose payload file was changed, or that reports drift of an intact install. Each is
/// drift, with the receipt's version against the record's.
#[test]
fn the_heartbeat_reports_the_drift_of_a_pkg() {
    let mac = MacRig::new("pkg-drift");
    let daemon = Arc::new(mac.daemon());
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());
    assert_eq!(drift(Arc::clone(&daemon)), None);

    fs::write(
        mac.real("/Library/Application Support/hello/hello"),
        b"changed",
    )
    .expect("a file changed");
    assert_eq!(
        drift(Arc::clone(&daemon)),
        Some(Drift {
            found: Some("1.0.0".to_owned()),
            recorded: Some("1.0.0".to_owned()),
        })
    );

    mac.mac.set_receipt(IDENTIFIER, "0.9.0", &[]);
    assert_eq!(
        drift(daemon),
        Some(Drift {
            found: Some("0.9.0".to_owned()),
            recorded: Some("1.0.0".to_owned()),
        })
    );
}

/// Catches: a first install whose `installer` fails that leaves the package's files, its
/// receipt, its record or `current` behind. Its rollback, with no version before it, stops the
/// job and removes the package by its receipt, and the order ends `Failed`, `release`,
/// `installer_error`.
#[test]
fn a_failed_first_install_of_a_pkg_is_taken_off_the_host() {
    let mac = MacRig::new("pkg-first-fails");
    let daemon = mac.daemon();
    let one = mac.publish(
        "1.0.0",
        package("1.0.0").with_install(support_macos_pkg::pkg::Install::OtherBytes {
            path: "/Library/Application Support/hello/hello".to_owned(),
            bytes: b"not the sealed copy".to_vec(),
        }),
    );
    let ended = daemon.order(&install_of("1.0.0", &one, 1));
    assert!(
        matches!(
            &ended,
            Reply::Finished {
                verdict: Verdict::Failed { class: Class::Release, .. },
                reason: Some(reason),
                ..
            } if reason == "installer_error"
        ),
        "{ended:?}"
    );
    assert_eq!(
        mac.mac.receipt(IDENTIFIER),
        None,
        "the receipt is forgotten"
    );
    assert!(
        !mac.real("/Library/Application Support/hello/hello")
            .exists()
    );
    assert_eq!(recorded(&daemon), None);
    assert_eq!(linked(&mac, "current"), None);
    assert!(
        mac.rig
            .placed_log
            .events()
            .contains(&hostkeeper_placed::testing::Event::Unit(
                hostkeeper_placed::testing::UnitCall::Stop(LABEL.to_owned())
            )),
        "the job is stopped"
    );
}

/// Catches: a `.pkg` staged for a service when another service's record names its identifier.
/// The stage is refused `Failed`, `release`, `package_conflict`, before any tool runs.
#[test]
fn a_pkg_whose_identifier_another_service_records_is_a_conflict() {
    let mac = MacRig::new("pkg-conflict");
    let daemon = mac.daemon();
    {
        let placer = daemon.ctx().placer();
        let lock = placer.lock().expect("the placed lock");
        let mut record = placer.store().load().expect("placed.json reads");
        let mut other = hostkeeper_placed::InstalledEntry::new("macos-pkg");
        other.package = Some(IDENTIFIER.to_owned());
        other.version = Some("3.0.0".to_owned());
        record.installed.insert("other".to_owned(), other);
        placer.store().save(&record, &lock).expect("saved");
    }
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", &one, 1)),
        failed(Class::Release, "package_conflict")
    );
    assert!(mac.mac.calls().is_empty(), "no tool ran");
    assert_eq!(recorded(&daemon), None);
}

/// Catches: an install over a receipt hostkeeper did not write (the package put there by hand)
/// that reports nothing of it. The install corrects it and ends `Succeeded`, `drift_repaired`.
#[test]
fn an_install_over_a_pkg_put_there_by_hand_repairs_the_drift() {
    let mac = MacRig::new("pkg-repaired");
    let daemon = mac.daemon();
    mac.mac.set_receipt(IDENTIFIER, "0.9.0", &[]);
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", &one, 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: Some("drift_repaired".to_owned()),
            resent: false,
        }
    );
    assert_eq!(
        mac.mac.receipt(IDENTIFIER).map(|receipt| receipt.version),
        Some("1.0.0".to_owned())
    );
}

/// Catches: a `.pkg` the store could not fetch that is staged or reported as another failure.
/// The store's refusal is the order's (`no_space`, deferred), and nothing is staged.
#[test]
fn a_pkg_the_store_cannot_fetch_is_its_refusal() {
    let mac = MacRig::new("pkg-fetch-fails");
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    mac.rig.pins.fail(
        SERVICE,
        &one.digest,
        hostkeeper_daemon::packages::PackageError::NoSpace("the disk is full".to_owned()),
    );
    let ended = daemon.order(&install_of("1.0.0", &one, 1));
    assert!(
        matches!(&ended, Reply::Refused { refusal, .. } if refusal.token() == "no_space"),
        "{ended:?}"
    );
    assert!(!mac.version_dir("1.0.0").exists());
    assert!(mac.mac.calls().is_empty(), "no tool ran");
}

/// Catches: a `.pkg` version whose running executable the observer cannot match (its version
/// reported `version_unconfirmed` for ever), or matches against another file of the tree. The
/// digest of a `.pkg` version is the SHA-256 of the program its job's plist in the tree names,
/// under `tree/`; a version the service does not have has none.
#[test]
fn the_sealed_digest_of_a_pkg_version_is_that_of_its_jobs_program_in_the_tree() {
    use hostkeeper_daemon::observer::SealedExe;
    use hostkeeper_observe::ExpectedExe;
    use sha2::{Digest, Sha256};
    let mac = MacRig::new("pkg-sealed-exe");
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());
    let program = fs::read(
        mac.version_dir("1.0.0")
            .join("tree/Library/Application Support/hello/hello"),
    )
    .expect("it reads");
    let want: [u8; 32] = Sha256::digest(&program).into();
    let exe = SealedExe::new(&mac.rig.packages, support::OWN);
    assert_eq!(exe.digest(SERVICE, "1.0.0"), Some(want));
    assert_eq!(exe.digest(SERVICE, "9.9.9"), None);
}

/// Catches: a forced install of a `.pkg` whose bytes are not the entry's pin blamed on the
/// host. The store's refusal keeps its class: `Failed` before the commit, `release`.
#[test]
fn a_force_of_a_pkg_the_store_refuses_keeps_the_refusals_class() {
    let mac = MacRig::new("pkg-force-refused");
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());
    let two = mac.publish("1.1.0", package("1.1.0"));
    mac.rig.pins.fail(
        SERVICE,
        &two.digest,
        hostkeeper_daemon::packages::PackageError::DigestMismatch("other bytes".to_owned()),
    );
    let force = support_macos_pkg::force_of("1.1.0", &two, 2);
    let ended = daemon.order(&force);
    assert!(
        matches!(
            &ended,
            Reply::Finished {
                verdict: Verdict::Failed {
                    class: Class::Release,
                    ..
                },
                ..
            }
        ),
        "{ended:?}"
    );
    assert_eq!(linked(&mac, "current").as_deref(), Some("1.0.0"));
}
