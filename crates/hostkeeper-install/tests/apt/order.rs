//! What an order asks of the repository installer: the entry's shape against its installer's
//! kind (packages-repositories.md section 2.3) and the manifest rules of a repository entry
//! (section 3.4, packages-native.md section 10.2).

use std::collections::BTreeMap;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::repository::apt::Apt;
use hostkeeper_install::repository::{Class, Failure, Order, Outcome, Reason, judge_kind};
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Container, Hook, InstallerBounds, Location, Manifest, Param, Place, Target,
};
use hostkeeper_release::Installer;

use crate::harness::assert_failure;

const PIN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn entry() -> Target {
    Target {
        installer: "apt".to_owned(),
        locations: Vec::new(),
        digest: PIN.to_owned(),
        bytes: 0,
        attestation: None,
        manifest: Some(Manifest {
            package: Some("mdm".to_owned()),
            hook: Some(Hook::default()),
            run: vec!["mdm.service".to_owned()],
            installer_bounds: Some(InstallerBounds {
                stage_secs: 60,
                activate_secs: 120,
                remove_secs: 30,
            }),
            ..Manifest::default()
        }),
        repository: Some("main".to_owned()),
        package_version: Some("1.4.0-1".to_owned()),
    }
}

fn manifest(target: &mut Target) -> &mut Manifest {
    target.manifest.as_mut().expect("a manifest")
}

fn order(target: &Target) -> Result<Order, Failure> {
    let service = ServiceName::new("mdm").expect("a name");
    let version = Version::new("1.4.0").expect("a version");
    Order::from_entry(&Apt, &service, &version, target, 4096)
}

fn refused(target: &Target, reason: Reason) -> Failure {
    let failure = order(target).expect_err("the entry is refused");
    assert_eq!(failure.reason(), reason, "{failure}");
    failure
}

/// Proves a good entry is accepted, and the order carries the entry's repository, package,
/// version, pin, run units, boot step, params flag, bounds and the daemon's package limit.
#[test]
fn a_good_entry_is_an_order() {
    let mut target = entry();
    manifest(&mut target).hook = Some(Hook {
        boot: Some("migrate".to_owned()),
        ..Hook::default()
    });
    manifest(&mut target).params = BTreeMap::from([("level".to_owned(), Param::default())]);
    let order = order(&target).expect("accepted");
    assert_eq!(order.repository, "main");
    assert_eq!(
        (order.package.as_str(), order.package_version.as_str()),
        ("mdm", "1.4.0-1")
    );
    assert_eq!(order.pin, PIN);
    assert_eq!(order.run, ["mdm.service"]);
    assert_eq!(order.boot.as_deref(), Some("migrate"));
    assert!(order.params);
    assert_eq!(
        (
            order.bounds.stage,
            order.bounds.activate,
            order.bounds.remove
        ),
        (60, 120, 30)
    );
    assert_eq!(order.max_package_bytes, 4096);
    assert_eq!(order.service.as_str(), "mdm");
    assert_eq!(order.version.as_str(), "1.4.0");
    let plain = self::order(&entry()).expect("accepted");
    assert!(!plain.params & plain.boot.is_none());
}

/// Proves an entry for another installer is `order_invalid`, and so is one that names an artifact
/// (locations, bytes or an attestation), failed, release.
#[test]
fn the_entry_must_be_the_backends_and_name_no_artifact() {
    let mut other = entry();
    other.installer = "tarball".to_owned();
    refused(&other, Reason::OrderInvalid);
    let located = Target {
        locations: vec![Location {
            store: "s".to_owned(),
            locator: "l".to_owned(),
        }],
        ..entry()
    };
    let sized = Target {
        bytes: 1,
        ..entry()
    };
    let attested = Target {
        attestation: Some(Blob {
            digest: PIN.to_owned(),
            bytes: 1,
            locations: Vec::new(),
        }),
        ..entry()
    };
    for target in [located, sized, attested] {
        let failure = refused(&target, Reason::OrderInvalid);
        assert_failure(
            &failure,
            Reason::OrderInvalid,
            Class::Release,
            Outcome::Failed,
        );
    }
}

/// Proves an entry with no repository is `coordinator_too_old`, deferred, environment.
#[test]
fn an_entry_without_a_repository_is_from_an_old_coordinator() {
    let target = Target {
        repository: None,
        ..entry()
    };
    let failure = refused(&target, Reason::CoordinatorTooOld);
    assert_failure(
        &failure,
        Reason::CoordinatorTooOld,
        Class::Environment,
        Outcome::Deferred,
    );
}

/// Proves a pin that is not 64 lower-case hex digits is `order_invalid`.
#[test]
fn a_bad_pin_is_refused() {
    for pin in [
        &PIN[1..],
        &PIN.to_uppercase(),
        &format!("{}g", &PIN[1..]),
        "",
    ] {
        let target = Target {
            digest: pin.to_owned(),
            ..entry()
        };
        refused(&target, Reason::OrderInvalid);
    }
}

/// Proves each manifest rule of a repository entry is `manifest_invalid`: no manifest, no
/// package, a package or a version outside dpkg's grammar, a hook command, `place` entries,
/// containers, no run unit and 65 of them, and missing or zero installer bounds; 1 and 64 run
/// units are accepted.
#[test]
fn each_manifest_rule_is_manifest_invalid() {
    type Edit = Box<dyn Fn(&mut Target)>;
    let edits: Vec<(&str, Edit)> = vec![
        ("no manifest", Box::new(|t| t.manifest = None)),
        ("no package", Box::new(|t| manifest(t).package = None)),
        (
            "a package in capitals",
            Box::new(|t| manifest(t).package = Some("Mdm".to_owned())),
        ),
        (
            "a one-letter package",
            Box::new(|t| manifest(t).package = Some("m".to_owned())),
        ),
        ("no package version", Box::new(|t| t.package_version = None)),
        (
            "a version of a letter first",
            Box::new(|t| t.package_version = Some("v1".to_owned())),
        ),
        (
            "a version with a space",
            Box::new(|t| t.package_version = Some("1 0".to_owned())),
        ),
        (
            "a hook command",
            Box::new(|t| {
                manifest(t).hook = Some(Hook {
                    command: vec!["bin/x".to_owned()],
                    ..Hook::default()
                });
            }),
        ),
        (
            "a place entry",
            Box::new(|t| {
                manifest(t).place = vec![Place {
                    kind: "file".to_owned(),
                    source: "a".to_owned(),
                    path: "/b".to_owned(),
                }];
            }),
        ),
        (
            "a container",
            Box::new(|t| manifest(t).containers = vec![Container::default()]),
        ),
        ("no run unit", Box::new(|t| manifest(t).run.clear())),
        (
            "65 run units",
            Box::new(|t| {
                manifest(t).run = (0..65).map(|n| format!("mdm-{n}.service")).collect();
            }),
        ),
        (
            "no bounds",
            Box::new(|t| manifest(t).installer_bounds = None),
        ),
        (
            "a stage bound of 0",
            Box::new(|t| {
                manifest(t).installer_bounds = Some(InstallerBounds {
                    stage_secs: 0,
                    activate_secs: 1,
                    remove_secs: 1,
                });
            }),
        ),
        (
            "an activate bound of 0",
            Box::new(|t| {
                manifest(t).installer_bounds = Some(InstallerBounds {
                    stage_secs: 1,
                    activate_secs: 0,
                    remove_secs: 1,
                });
            }),
        ),
        (
            "a remove bound of 0",
            Box::new(|t| {
                manifest(t).installer_bounds = Some(InstallerBounds {
                    stage_secs: 1,
                    activate_secs: 1,
                    remove_secs: 0,
                });
            }),
        ),
    ];
    for (what, edit) in edits {
        let mut target = entry();
        edit(&mut target);
        let failure = order(&target).expect_err(what);
        assert_failure(
            &failure,
            Reason::ManifestInvalid,
            Class::Release,
            Outcome::Failed,
        );
    }
    for count in [1, 64] {
        let mut target = entry();
        manifest(&mut target).run = (0..count).map(|n| format!("mdm-{n}.service")).collect();
        assert_eq!(order(&target).expect("accepted").run.len(), count);
    }
}

/// Proves an artifact installer's entry naming a repository or a package version is
/// `order_invalid`, and one naming neither is accepted.
#[test]
fn an_artifact_entry_names_no_repository() {
    let artifact = Target {
        repository: None,
        package_version: None,
        ..entry()
    };
    judge_kind(Installer::MacosPkg, &artifact).expect("an artifact entry");
    for target in [
        Target {
            repository: Some("main".to_owned()),
            ..artifact.clone()
        },
        Target {
            package_version: Some("1-1".to_owned()),
            ..artifact.clone()
        },
    ] {
        let failure = judge_kind(Installer::MacosPkg, &target).expect_err("refused");
        assert_failure(
            &failure,
            Reason::OrderInvalid,
            Class::Release,
            Outcome::Failed,
        );
    }
    judge_kind(Installer::Apt, &entry()).expect("a repository entry");
}
