//! The rules of the entry's installer (`manifest::check_for`): each field an installer requires
//! missing, and each it refuses present. The coordinator's check (`manifest::check`) applies
//! none of them.

use hostkeeper_proto::hostkeeper::release::v1::Manifest;
use hostkeeper_release::installer::UnknownInstaller;
use hostkeeper_release::manifest::{self, ManifestError};
use hostkeeper_release::{ARCHIVE_INSTALLER, Installer};

use crate::support::{container, manifest, place, platform_of, web};

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

/// Catches: an entry of either installer passing without a hook or installer bounds, or the
/// refusal of a missing field reading otherwise than the rule it breaks.
#[test]
fn every_installer_needs_a_hook_and_installer_bounds() {
    for installer in Installer::ALL {
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
/// identity.
#[test]
fn every_installer_needs_the_packages_name() {
    for installer in Installer::ALL {
        let mut m = manifest(installer);
        m.package = None;
        missing(installer, &m, "package");
    }
}

/// Catches: `place` passing for a format whose own tool places its files, or `containers`
/// passing for an installer that runs none, whose units the package ships.
#[test]
fn place_and_containers_are_refused_for_every_installer() {
    for installer in Installer::ALL {
        let mut m = manifest(installer);
        m.place = vec![place("file", "files/a", "/etc/web/a")];
        refused(installer, &m, "place");
    }
    // A container's unit is a systemd unit.
    let mut m = manifest(Installer::Apt);
    m.run.push("web-api.service".to_owned());
    m.containers = vec![container()];
    refused(Installer::Apt, &m, "containers");
    assert_eq!(
        ManifestError::Refused {
            installer: Installer::Apt,
            field: "place"
        }
        .to_string(),
        "the manifest of the apt entry may not have place"
    );
}

/// Catches: an installer's spelling that does not round trip, a withdrawn installer (`deb`,
/// `rpm`, `arch`, `apk`, `hostkeeper-archive`, `tarball`, `oci-image`) still read as one a
/// daemon may list, or an unknown or differently cased name read as an installer.
#[test]
fn the_installers_read_their_own_spellings_only() {
    let spellings: Vec<&str> = Installer::ALL.iter().map(|i| i.as_str()).collect();
    assert_eq!(spellings, ["apt", "macos-pkg"]);
    assert_eq!(Installer::ALL, [Installer::Apt, Installer::MacosPkg]);
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
        "deb",
        "rpm",
        "arch",
        "apk",
        "dnf",
        "snap",
        "APT",
        "",
        "flatpak",
        "hostkeeper-archive",
        "tarball",
        "oci-image",
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
/// have the daemon look for a repository on a `macos-pkg` artifact entry.
#[test]
fn apt_alone_is_a_backend_of_the_repository_installer() {
    let repository: Vec<Installer> = Installer::ALL
        .into_iter()
        .filter(|installer| installer.is_repository())
        .collect();
    assert_eq!(repository, [Installer::Apt]);
}

/// Catches: the archive installer's name changed, which the coordinator and the daemon read on
/// old data, or an installer of this crate answering to it.
#[test]
fn the_archive_installers_name_is_a_constant_and_no_installer() {
    assert_eq!(ARCHIVE_INSTALLER, "hostkeeper-archive");
    assert!(
        Installer::ALL
            .iter()
            .all(|installer| installer.as_str() != ARCHIVE_INSTALLER)
    );
    assert_eq!(
        ARCHIVE_INSTALLER.parse::<Installer>(),
        Err(UnknownInstaller(ARCHIVE_INSTALLER.to_owned()))
    );
}
