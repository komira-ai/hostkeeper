//! The daemon's check of an order's entry on receipt: the version first, then the installer it
//! lists, then the manifest.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{Manifest, Target};
use hostkeeper_release::manifest::ManifestError;
use hostkeeper_release::receipt::{self, Daemon, ReceiptError};
use hostkeeper_release::{ARCHIVE_INSTALLER, Installer};

use crate::support::{LINUX, entry, pad_manifest_to, text, version, web};

const LISTED: [Installer; 1] = [Installer::Apt];

fn daemon(running: &str) -> Daemon<'static> {
    Daemon {
        platform: LINUX,
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
        check(&with_min(entry(Installer::Apt), "1.2.3"), "1.2.3"),
        Ok(Installer::Apt)
    );
}

/// Catches: a manifest a newer daemon wrote judged by this one's rules (`manifest_invalid`
/// instead of `daemon_too_old`), or the installer judged before the version.
#[test]
fn a_manifest_above_the_daemon_is_daemon_too_old_whatever_else_it_holds() {
    let mut wrong = with_min(entry(Installer::Apt), "0.5.0");
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
        check(&with_min(entry(Installer::Apt), "0.10.0"), "0.9.0"),
        Err(ReceiptError::DaemonTooOld { .. })
    ));
}

/// Catches: an installer the daemon does not list reaching the manifest's rules
/// (`manifest_invalid`), or failing the order instead of deferring it.
#[test]
fn an_installer_the_daemon_does_not_list_is_installer_not_listed() {
    for name in [
        "rpm",
        "deb",
        "oci-image",
        "tarball",
        ARCHIVE_INSTALLER,
        "flatpak",
        "Apt",
        "",
    ] {
        let mut target = entry(Installer::Apt);
        target.installer = name.to_owned();
        // The manifest is wrong for any of them too; the installer is the answer.
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
    let mut no_package = entry(Installer::Apt);
    no_package.manifest.as_mut().expect("a manifest").package = None;
    let err = check(&no_package, "0.4.0").expect_err("refused");
    assert_eq!(
        err,
        ReceiptError::Invalid(ManifestError::Missing {
            installer: Installer::Apt,
            field: "package"
        })
    );
    assert_eq!(err.reason(), "manifest_invalid");
    assert!(!err.is_deferred());
    assert_eq!(
        err.to_string(),
        "the manifest of the apt entry needs package"
    );

    let bad_version = with_min(entry(Installer::Apt), "next");
    let err = check(&bad_version, "0.4.0").expect_err("refused");
    assert!(matches!(
        err,
        ReceiptError::Invalid(ManifestError::MinHostkeeper(_))
    ));
    assert_eq!(err.reason(), "manifest_invalid");

    let mut none = entry(Installer::Apt);
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
        platform: LINUX,
        running: HostkeeperVersion::parse("9.9.9").expect("a version"),
        installers: &[],
    };
    assert_eq!(
        receipt::check(&web(), &entry(Installer::Apt), daemon),
        Err(ReceiptError::InstallerNotListed("apt".to_owned()))
    );
}

/// Catches: the daemon taking a manifest the coordinator's rules refuse: the daemon trusts no
/// check the coordinator made, so every rule every manifest keeps is applied on receipt too, and
/// before the installer's own rules.
#[test]
fn on_receipt_the_daemon_applies_every_rule_every_manifest_keeps_before_its_installers() {
    type Break = fn(&mut Manifest);
    type Is = fn(&ManifestError) -> bool;
    let cases: [(Break, Is); 6] = [
        (
            |m| m.run = vec!["api.service".to_owned()],
            |e| matches!(e, ManifestError::RunUnit { .. }),
        ),
        (
            |m| m.health_url = Some("http://localhost:8080/".to_owned()),
            |e| matches!(e, ManifestError::HealthUrl { .. }),
        ),
        (
            |m| m.hook.as_mut().expect("a hook").boot = Some("prepare".to_owned()),
            |e| matches!(e, ManifestError::Boot(_)),
        ),
        (
            |m| {
                m.params.insert("updater.x".to_owned(), text("y"));
            },
            |e| matches!(e, ManifestError::ReservedParam(_)),
        ),
        (
            |m| m.installer_bounds.as_mut().expect("bounds").stage_secs = 0,
            |e| matches!(e, ManifestError::InstallerBound { .. }),
        ),
        (
            |m| pad_manifest_to(m, 16 * 1024 + 1),
            |e| matches!(e, ManifestError::TooLarge { .. }),
        ),
    ];
    for (at, (broken, is)) in cases.into_iter().enumerate() {
        let mut target = entry(Installer::Apt);
        let manifest = target.manifest.as_mut().expect("a manifest");
        // The installer's rule is broken too: an apt manifest names its package.
        manifest.package = None;
        broken(manifest);
        let err = check(&target, "0.4.0").expect_err("refused");
        assert_eq!(err.reason(), "manifest_invalid", "case {at}");
        let ReceiptError::Invalid(why) = &err else {
            panic!("case {at}: {err:?}");
        };
        assert!(is(why), "case {at}: {why:?}");
    }
}

/// Catches: the daemon judging unit names by another platform than its own.
#[test]
fn on_receipt_the_unit_names_are_those_of_the_daemons_platform() {
    let mac = Daemon {
        platform: Platform::MacosArm64,
        running: version("0.4.0"),
        installers: &[Installer::MacosPkg, Installer::Apt],
    };
    assert_eq!(
        receipt::check(&web(), &entry(Installer::MacosPkg), mac),
        Ok(Installer::MacosPkg)
    );
    // A Linux entry's systemd units are not launchd labels of the service on a Mac.
    let mut linux = entry(Installer::Apt);
    linux.manifest.as_mut().expect("a manifest").run = vec!["web-cache.timer".to_owned()];
    assert!(matches!(
        receipt::check(&web(), &linux, mac),
        Err(ReceiptError::Invalid(ManifestError::RunUnit { .. }))
    ));
    // And a Mac's labels are not systemd units on Linux.
    let mut on_linux = entry(Installer::MacosPkg);
    on_linux.installer = "apt".to_owned();
    assert!(matches!(
        check(&on_linux, "0.4.0"),
        Err(ReceiptError::Invalid(ManifestError::RunUnit { .. }))
    ));
}
