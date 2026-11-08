//! The daemon's check of an order's entry on receipt: the version first, then the installer it
//! lists, then the manifest.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_release::manifest::ManifestError;
use hostkeeper_release::receipt::{self, Daemon, ReceiptError};

use crate::support::{entry, version, web};

const LISTED: [Installer; 3] = [
    Installer::HostkeeperArchive,
    Installer::Tarball,
    Installer::Deb,
];

fn daemon(running: &str) -> Daemon<'static> {
    Daemon {
        running: version(running),
        installers: &LISTED,
    }
}

fn check(target: &Target, running: &str) -> Result<Installer, ReceiptError> {
    receipt::check(&web(), target, daemon(running))
}

fn with_min(mut target: Target, min: &str) -> Target {
    target.manifest.as_mut().expect("a manifest").min_hostkeeper = min.to_owned();
    target
}

/// Catches: an entry the daemon can take refused, or answered with another installer.
#[test]
fn an_entry_of_a_listed_installer_from_a_new_enough_daemon_is_taken() {
    for installer in LISTED {
        assert_eq!(
            check(&entry(installer), "0.4.0"),
            Ok(installer),
            "{installer}"
        );
    }
    // The floor is inclusive: a daemon of exactly `min_hostkeeper` takes it.
    assert_eq!(
        check(&with_min(entry(Installer::Deb), "1.2.3"), "1.2.3"),
        Ok(Installer::Deb)
    );
}

/// Catches: a manifest a newer daemon wrote judged by this one's rules (`manifest_invalid`
/// instead of `daemon_too_old`), or the installer judged before the version.
#[test]
fn a_manifest_above_the_daemon_is_daemon_too_old_whatever_else_it_holds() {
    let mut wrong = with_min(entry(Installer::Deb), "0.5.0");
    let manifest = wrong.manifest.as_mut().expect("a manifest");
    manifest.package = None;
    manifest.run = vec!["api.service".to_owned()];
    wrong.installer = "flatpak".to_owned();
    let err = check(&wrong, "0.4.9").expect_err("too old");
    assert_eq!(
        err,
        ReceiptError::DaemonTooOld {
            needs: version("0.5.0"),
            running: version("0.4.9")
        }
    );
    assert_eq!(err.reason(), "daemon_too_old");
    assert!(err.is_deferred());
    assert_eq!(
        err.to_string(),
        "the manifest needs hostkeeper 0.5.0; this daemon is 0.4.9"
    );
    // Compared as numbers, not text.
    assert!(matches!(
        check(&with_min(entry(Installer::Deb), "0.10.0"), "0.9.0"),
        Err(ReceiptError::DaemonTooOld { .. })
    ));
}

/// Catches: an installer the daemon does not list reaching the manifest's rules
/// (`manifest_invalid`), or failing the order instead of deferring it.
#[test]
fn an_installer_the_daemon_does_not_list_is_installer_not_listed() {
    for name in ["rpm", "oci-image", "flatpak", "Deb", ""] {
        let mut target = entry(Installer::Rpm);
        target.installer = name.to_owned();
        // The manifest is wrong for rpm too; the installer is the answer.
        target.manifest.as_mut().expect("a manifest").package = None;
        let err = check(&target, "0.4.0").expect_err(name);
        assert_eq!(
            err,
            ReceiptError::InstallerNotListed(name.to_owned()),
            "{name}"
        );
        assert_eq!(err.reason(), "installer_not_listed");
        assert!(err.is_deferred());
    }
    assert_eq!(
        ReceiptError::InstallerNotListed("rpm".to_owned()).to_string(),
        "the entry's installer \"rpm\" is not one this daemon lists"
    );
}

/// Catches: a manifest that breaks a rule, or one with no version, taken; or its refusal
/// deferred instead of failed.
#[test]
fn a_manifest_that_breaks_a_rule_or_has_no_version_is_manifest_invalid() {
    let mut no_package = entry(Installer::Deb);
    no_package.manifest.as_mut().expect("a manifest").package = None;
    let err = check(&no_package, "0.4.0").expect_err("refused");
    assert_eq!(
        err,
        ReceiptError::Invalid(ManifestError::Missing {
            installer: Installer::Deb,
            field: "package"
        })
    );
    assert_eq!(err.reason(), "manifest_invalid");
    assert!(!err.is_deferred());
    assert_eq!(err.to_string(), "the manifest of a deb entry needs package");

    let bad_version = with_min(entry(Installer::Deb), "next");
    let err = check(&bad_version, "0.4.0").expect_err("refused");
    assert!(matches!(
        err,
        ReceiptError::Invalid(ManifestError::MinHostkeeper(_))
    ));
    assert_eq!(err.reason(), "manifest_invalid");

    let mut none = entry(Installer::Deb);
    none.manifest = None;
    let err = check(&none, "0.4.0").expect_err("refused");
    assert_eq!(err, ReceiptError::NoManifest);
    assert_eq!(err.reason(), "manifest_invalid");
    assert!(!err.is_deferred());
    assert_eq!(err.to_string(), "the entry has no manifest");
}

/// Catches: a daemon with no list taking any entry.
#[test]
fn a_daemon_that_lists_no_installer_takes_no_entry() {
    let daemon = Daemon {
        running: HostkeeperVersion::parse("9.9.9").expect("a version"),
        installers: &[],
    };
    assert_eq!(
        receipt::check(&web(), &entry(Installer::HostkeeperArchive), daemon),
        Err(ReceiptError::InstallerNotListed(
            "hostkeeper-archive".to_owned()
        ))
    );
}
