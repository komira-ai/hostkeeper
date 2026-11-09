//! The rules of the entry's installer (`manifest::check_for`): each field an installer requires
//! missing, and each it refuses present. The coordinator's check (`manifest::check`) applies
//! none of them.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::release::v1::{Hook, Manifest};
use hostkeeper_release::Installer;
use hostkeeper_release::installer::UnknownInstaller;
use hostkeeper_release::manifest::{self, ManifestError};

use crate::support::{bounds, container, hook, manifest, place, platform_of, text, web};

/// Sets one field of a manifest.
type SetField = fn(&mut Manifest);

fn check_for(installer: Installer, manifest: &Manifest) -> Result<(), ManifestError> {
    manifest::check_for(&web(), platform_of(installer), installer, manifest)
}

/// Asserts `installer` refuses `manifest` for `field`, and the coordinator's check takes it.
fn refused(installer: Installer, manifest: &Manifest, field: &'static str) {
    assert_eq!(
        check_for(installer, manifest),
        Err(ManifestError::Refused { installer, field }),
        "{installer} {field}"
    );
    assert_eq!(
        manifest::check(&web(), platform_of(installer), manifest),
        Ok(()),
        "{installer} {field}"
    );
}

/// Asserts `installer` needs `field` in `manifest`, and the coordinator's check takes it.
fn missing(installer: Installer, manifest: &Manifest, field: &'static str) {
    assert_eq!(
        check_for(installer, manifest),
        Err(ManifestError::Missing { installer, field }),
        "{installer} {field}"
    );
    assert_eq!(
        manifest::check(&web(), platform_of(installer), manifest),
        Ok(()),
        "{installer} {field}"
    );
}

/// Catches: an archive entry's manifest carrying anything but `min_hostkeeper` passing, which
/// would let an unattested manifest override the attested `manifest.toml` inside the archive.
#[test]
fn a_hostkeeper_archive_manifest_holds_only_min_hostkeeper() {
    let archive = Installer::HostkeeperArchive;
    let only = manifest(archive);
    assert_eq!(check_for(archive, &only), Ok(()));
    let fields: [(&'static str, SetField); 8] = [
        ("package", |m| m.package = Some("web".to_owned())),
        ("hook", |m| m.hook = Some(hook())),
        ("run", |m| m.run = vec!["web.service".to_owned()]),
        ("health_url", |m| {
            m.health_url = Some("http://127.0.0.1:8080/".to_owned());
        }),
        ("place", |m| {
            m.place = vec![place("file", "files/a", "/etc/web/a")];
        }),
        ("params", |m| {
            m.params = BTreeMap::from([("a".to_owned(), text("b"))]);
        }),
        ("containers", |m| {
            m.run = vec!["web-api.service".to_owned()];
            m.containers = vec![container()];
        }),
        ("installer_bounds", |m| m.installer_bounds = Some(bounds())),
    ];
    for (field, set) in fields {
        let mut m = only.clone();
        set(&mut m);
        // `containers` needs its unit in `run`, so `run` is the first field present.
        let first = if field == "containers" { "run" } else { field };
        refused(archive, &m, first);
    }
    assert_eq!(
        ManifestError::Refused {
            installer: archive,
            field: "hook"
        }
        .to_string(),
        "the manifest of the hostkeeper-archive entry may not have hook"
    );
}

/// Catches: an entry of any installer but the archive passing without a hook or installer
/// bounds.
#[test]
fn every_installer_but_the_archive_needs_a_hook_and_installer_bounds() {
    for installer in Installer::ALL {
        if installer == Installer::HostkeeperArchive {
            continue;
        }
        let mut no_hook = manifest(installer);
        no_hook.hook = None;
        missing(installer, &no_hook, "hook");
        let mut no_bounds = manifest(installer);
        no_bounds.installer_bounds = None;
        missing(installer, &no_bounds, "installer_bounds");
    }
    assert_eq!(
        ManifestError::Missing {
            installer: Installer::MacosPkg,
            field: "package"
        }
        .to_string(),
        "the manifest of the macos-pkg entry needs package"
    );
}

/// Catches: a native package's manifest (`apt`'s, `macos-pkg`'s) passing without the package's
/// identity, or a manifest of a format with no package database passing with one.
#[test]
fn package_is_required_for_native_formats_and_refused_for_the_others() {
    for installer in [Installer::Apt, Installer::MacosPkg] {
        assert!(installer.names_a_package(), "{installer}");
        let mut m = manifest(installer);
        m.package = None;
        missing(installer, &m, "package");
    }
    for installer in [Installer::Tarball, Installer::OciImage] {
        assert!(!installer.names_a_package(), "{installer}");
        let mut m = manifest(installer);
        m.package = Some("web".to_owned());
        refused(installer, &m, "package");
    }
    assert!(!Installer::HostkeeperArchive.names_a_package());
}

/// Catches: `place` passing for a format whose own tool places its files, or `containers`
/// passing for anything but an image, or an image passing without one.
#[test]
fn place_is_for_a_tarball_and_containers_for_an_image_only() {
    for installer in [Installer::Apt, Installer::MacosPkg, Installer::OciImage] {
        let mut m = manifest(installer);
        m.place = vec![place("file", "files/a", "/etc/web/a")];
        refused(installer, &m, "place");
    }
    // An image is Linux only; a container's unit is a systemd unit.
    for installer in [Installer::Apt, Installer::Tarball] {
        let mut m = manifest(installer);
        m.run.push("web-api.service".to_owned());
        m.containers = vec![container()];
        refused(installer, &m, "containers");
    }
    let mut image = manifest(Installer::OciImage);
    image.containers.clear();
    missing(Installer::OciImage, &image, "containers");
}

/// Catches: an image entry passing with a hook program, which no staged tree could hold.
#[test]
fn a_hook_command_for_an_image_is_refused() {
    let mut m = manifest(Installer::OciImage);
    m.hook = Some(Hook {
        boot: None,
        ..hook()
    });
    refused(Installer::OciImage, &m, "hook.command");
}

/// Catches: an installer's spelling that does not round trip, a withdrawn installer (`deb`,
/// `rpm`, `arch`, `apk`) still read as one a daemon may list, or an unknown or differently
/// cased name read as an installer.
#[test]
fn the_installers_read_their_own_spellings_only() {
    let spellings: Vec<&str> = Installer::ALL.iter().map(|i| i.as_str()).collect();
    assert_eq!(
        spellings,
        [
            "apt",
            "hostkeeper-archive",
            "macos-pkg",
            "oci-image",
            "tarball"
        ]
    );
    let mut sorted = Installer::ALL;
    sorted.sort();
    assert_eq!(
        sorted,
        Installer::ALL,
        "declared in the order of the spellings"
    );
    for installer in Installer::ALL {
        assert_eq!(installer.as_str().parse::<Installer>(), Ok(installer));
        assert_eq!(installer.to_string(), installer.as_str());
    }
    for unknown in [
        "deb", "rpm", "arch", "apk", "dnf", "snap", "APT", "", "flatpak",
    ] {
        assert_eq!(
            unknown.parse::<Installer>(),
            Err(UnknownInstaller(unknown.to_owned()))
        );
    }
    assert_eq!(
        UnknownInstaller("snap".to_owned()).to_string(),
        "\"snap\" is not an installer this daemon knows"
    );
}

/// Catches: an artifact installer taken for a repository backend or the other way, which would
/// have the daemon look for a repository on an archive's entry, or a repository backend that
/// needs no package name.
#[test]
fn apt_alone_is_a_backend_of_the_repository_installer() {
    let repository: Vec<Installer> = Installer::ALL
        .into_iter()
        .filter(|installer| installer.is_repository())
        .collect();
    assert_eq!(repository, [Installer::Apt]);
    for installer in repository {
        assert!(installer.names_a_package(), "{installer}");
    }
}
