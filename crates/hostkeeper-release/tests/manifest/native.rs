//! The rules of a native entry (packages-native.md sections 10.2 to 10.4, build item A18): which
//! installers are native, the hook of a native entry (no `hook.command`, and `hook.boot` without
//! one), a `macos-pkg` entry with no attestation, and the list of the service `hostkeeper`, one
//! native entry with at most one `hostkeeper-archive` entry after it. Each is judged by the
//! coordinator (`target::check`, `target::check_entry`) and, where it is a manifest's, by the
//! daemon (`manifest::check_for`) the same way.

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Hook, Manifest, Target, TargetList};
use hostkeeper_release::manifest::{self, ManifestError};
use hostkeeper_release::target::{self, EntryError, TargetError, is_native_entry};
use hostkeeper_release::{Installer, MAX_LOCATOR_BYTES_PER_ENTRY};

use crate::support::{
    LINUX, digest, entry, hook, location, manifest, named, platform_of, repository_entry, targets,
    web,
};

/// The installers whose entries are native: the platform's own package format.
const NATIVE: [Installer; 2] = [Installer::Apt, Installer::MacosPkg];

/// The installers whose entries are not.
const OTHER: [Installer; 3] = [
    Installer::HostkeeperArchive,
    Installer::OciImage,
    Installer::Tarball,
];

fn check_entry(entry: &Target) -> Result<(), EntryError> {
    let installer = entry.installer.parse().unwrap_or(Installer::Tarball);
    target::check_entry(&web(), platform_of(installer), entry)
}

fn check_for(installer: Installer, manifest: &Manifest) -> Result<(), ManifestError> {
    manifest::check_for(&web(), platform_of(installer), installer, manifest)
}

/// `entry` with its hook changed by `change`.
fn with_hook(mut entry: Target, change: impl FnOnce(&mut Hook)) -> Target {
    let manifest = entry.manifest.as_mut().expect("a manifest");
    change(manifest.hook.as_mut().expect("a hook"));
    entry
}

/// Catches: an installer of another format counted native (its entries would be picked for a
/// move, or held to the native hook), a native one not counted, or a later repository backend
/// needing a change to count: a repository entry is native whatever its installer, an artifact
/// entry by its installer's exact spelling alone.
#[test]
fn the_native_installers_are_apt_and_macos_pkg() {
    let native: Vec<Installer> = Installer::ALL
        .into_iter()
        .filter(|i| i.is_native())
        .collect();
    assert_eq!(native, NATIVE);
    for installer in NATIVE {
        assert!(is_native_entry(&entry(installer)), "{installer}");
    }
    for installer in OTHER {
        assert!(!is_native_entry(&entry(installer)), "{installer}");
    }
    // An artifact entry of a name no daemon lists, or of a native one spelled otherwise, is not
    // native; a repository entry of any installer is.
    for name in ["deb", "rpm", "Apt", "macos_pkg", "dnf"] {
        assert!(!is_native_entry(&named(name)), "{name}");
        let repository = Target {
            installer: name.to_owned(),
            ..repository_entry()
        };
        assert!(is_native_entry(&repository), "{name}");
    }
    let artifact_apt = Target {
        installer: "apt".to_owned(),
        ..named("deb")
    };
    assert!(is_native_entry(&artifact_apt));
}

/// Catches: a native entry passing with a hook program, which would let the caller of
/// `SetDesired` choose a program of the package to run as root, at the coordinator or at the
/// daemon; or an empty command refused.
#[test]
fn a_native_entry_names_no_hook_command() {
    for installer in NATIVE {
        assert_eq!(check_entry(&entry(installer)), Ok(()), "{installer}");
        let commanded = with_hook(entry(installer), |hook| {
            hook.command = vec!["usr/lib/web/hostkeeper-hook".to_owned()];
        });
        let refused = ManifestError::NativeHookCommand;
        assert_eq!(
            check_entry(&commanded),
            Err(EntryError::Manifest(refused.clone())),
            "{installer}"
        );
        let manifest = commanded.manifest.expect("a manifest");
        assert_eq!(check_for(installer, &manifest), Err(refused), "{installer}");
        // The rules every manifest keeps do not know the installer, and take it.
        assert_eq!(
            manifest::check(&web(), platform_of(installer), &manifest),
            Ok(()),
            "{installer}"
        );
    }
    // A later repository backend's entry is held to it too.
    let later = with_hook(
        Target {
            installer: "dnf".to_owned(),
            ..repository_entry()
        },
        |hook| hook.command = vec!["usr/lib/web/hostkeeper-hook".to_owned()],
    );
    assert_eq!(
        target::check_entry(&web(), LINUX, &later),
        Err(EntryError::Manifest(ManifestError::NativeHookCommand))
    );
    assert_eq!(
        ManifestError::NativeHookCommand.to_string(),
        "hook.command is set on a native entry; a native package's hook is its program at its fixed path"
    );
}

/// Catches: `hook.boot` refused on a native entry, whose hook is the package's program at its
/// fixed path; or allowed with no program on any other entry, whose built-in steps have no
/// boot step, at the coordinator or at the daemon.
#[test]
fn hook_boot_without_a_command_passes_for_a_native_entry_only() {
    for installer in NATIVE {
        let booted = entry(installer);
        let hook = booted
            .manifest
            .as_ref()
            .and_then(|m| m.hook.as_ref())
            .expect("a hook");
        assert!(
            hook.command.is_empty() && hook.boot.is_some(),
            "{installer}"
        );
        assert_eq!(check_entry(&booted), Ok(()), "{installer}");
        let manifest = booted.manifest.expect("a manifest");
        assert_eq!(check_for(installer, &manifest), Ok(()), "{installer}");
    }
    let without = |entry: Target| {
        with_hook(entry, |hook| {
            hook.command.clear();
            hook.boot = Some("restore".to_owned());
        })
    };
    let refused = ManifestError::BootWithoutCommand("restore".to_owned());
    for other in [entry(Installer::Tarball), named("deb")] {
        let name = other.installer.clone();
        assert_eq!(
            check_entry(&without(other.clone())),
            Err(EntryError::Manifest(refused.clone())),
            "{name}"
        );
        // With a program the boot step is the hook's, as before.
        assert_eq!(check_entry(&other), Ok(()), "{name}");
    }
    for installer in [Installer::Tarball, Installer::OciImage] {
        let mut m = manifest(installer);
        m.hook = Some(Hook {
            command: Vec::new(),
            boot: Some("restore".to_owned()),
            ..hook()
        });
        assert_eq!(
            check_for(installer, &m),
            Err(refused.clone()),
            "{installer}"
        );
        assert_eq!(manifest::check(&web(), LINUX, &m), Ok(()), "{installer}");
    }
    assert_eq!(
        refused.to_string(),
        "hook.boot \"restore\" needs a hook.command; the built-in steps have no boot step"
    );
}

/// Catches: a `macos-pkg` entry passing with an attestation, which no daemon checks (the
/// package's signature and the pin are its trust) and which must not look checked; one refused
/// without; or its locators measured with an attestation it has not. Every other artifact entry
/// still needs one.
#[test]
fn a_macos_pkg_entry_carries_no_attestation() {
    let pkg = entry(Installer::MacosPkg);
    assert_eq!(pkg.attestation, None);
    assert_eq!(check_entry(&pkg), Ok(()));
    let attested = Target {
        attestation: Some(Blob {
            digest: digest('b'),
            bytes: 7311,
            locations: vec![location("github", "example-org/web/web.pkg.sigstore.json")],
        }),
        ..pkg.clone()
    };
    assert_eq!(check_entry(&attested), Err(EntryError::MacosPkgAttestation));
    assert_eq!(
        EntryError::MacosPkgAttestation.to_string(),
        "the macos-pkg entry has an attestation; a package's signature and the pin are its trust, and no daemon checks one"
    );
    // The artifact's own locators alone count toward the 2 KiB of the entry.
    let over = Target {
        locations: vec![
            location("lan", &"a".repeat(1000)),
            location("lan", &"b".repeat(1000)),
            location("lan", &"c".repeat(49)),
        ],
        ..pkg.clone()
    };
    assert_eq!(check_entry(&over), Err(EntryError::LocatorBytes(2049)));
    let at = Target {
        locations: vec![
            location("lan", &"a".repeat(1000)),
            location("lan", &"b".repeat(1000)),
            location("lan", &"c".repeat(48)),
        ],
        ..pkg
    };
    assert_eq!(check_entry(&at), Ok(()));
    for installer in [Installer::Tarball, Installer::OciImage] {
        let bare = Target {
            attestation: None,
            ..entry(installer)
        };
        assert_eq!(
            check_entry(&bare),
            Err(EntryError::NoAttestation),
            "{installer}"
        );
    }
}

/// hostkeeper's own manifest for `installer`: its unit or label, no health URL, and the package
/// `hostkeeper` for a native installer.
fn own(installer: Installer) -> Target {
    let mut own = entry(installer);
    let manifest = own.manifest.as_mut().expect("a manifest");
    manifest.health_url = None;
    manifest.params.clear();
    manifest.run = match platform_of(installer) {
        Platform::MacosArm64 => vec!["hostkeeper".to_owned()],
        _ => vec!["hostkeeper.service".to_owned()],
    };
    if installer.is_native() {
        manifest.package = Some("hostkeeper".to_owned());
    }
    own
}

/// The targets of hostkeeper's release with one list, of `entries`, for `platform`.
fn one_list(
    platform: &str,
    entries: Vec<Target>,
) -> std::collections::BTreeMap<String, TargetList> {
    targets(&[(platform, TargetList { entries })])
}

/// Catches: hostkeeper's release refused in a shape of the move (its native entry, with the
/// archive entry after it during the move, or the archive entry alone until hostkeeper's own
/// release lists its native one), or passing in one no daemon could update itself from (the
/// archive entry first, two native entries, any other installer); and another service held to
/// the rule.
#[test]
fn hostkeepers_list_is_one_native_entry_and_at_most_one_archive_entry_after_it() {
    let hostkeeper = ServiceName::new("hostkeeper").expect("a name");
    let archive = || entry(Installer::HostkeeperArchive);
    for (platform, native) in [
        ("linux-x86_64", Installer::Apt),
        ("linux-arm64", Installer::Apt),
        ("macos-arm64", Installer::MacosPkg),
    ] {
        for good in [
            vec![own(native)],
            vec![own(native), archive()],
            vec![archive()],
        ] {
            let installers: Vec<String> = good.iter().map(|e| e.installer.clone()).collect();
            assert_eq!(
                target::check(&hostkeeper, &one_list(platform, good)),
                Ok(()),
                "{platform} {installers:?}"
            );
        }
    }
    let at = Platform::LinuxArm64;
    let tarball = || {
        let mut tarball = own(Installer::Tarball);
        tarball.manifest.as_mut().expect("a manifest").place.clear();
        tarball
    };
    let macos_on_linux = || Target {
        manifest: own(Installer::Apt).manifest,
        ..own(Installer::MacosPkg)
    };
    for bad in [
        vec![archive(), own(Installer::Apt)],
        vec![own(Installer::Apt), macos_on_linux()],
        vec![own(Installer::Apt), archive(), tarball()],
        vec![own(Installer::Apt), tarball()],
        vec![tarball()],
        vec![tarball(), archive()],
    ] {
        let installers: Vec<String> = bad.iter().map(|e| e.installer.clone()).collect();
        assert_eq!(
            target::check(&hostkeeper, &one_list("linux-arm64", bad)),
            Err(TargetError::SelfList { platform: at }),
            "{installers:?}"
        );
    }
    // Two entries of one installer are refused as in any list.
    for twice in [Installer::HostkeeperArchive, Installer::Apt] {
        assert_eq!(
            target::check(
                &hostkeeper,
                &one_list("linux-arm64", vec![own(twice), own(twice)])
            ),
            Err(TargetError::DuplicateInstaller {
                platform: at,
                installer: twice.as_str().to_owned()
            }),
            "{twice}"
        );
    }
    assert_eq!(
        TargetError::SelfList {
            platform: Platform::MacosArm64
        }
        .to_string(),
        "the list of macos-arm64 for the service hostkeeper is not one native entry with at most one hostkeeper-archive entry after it, nor one hostkeeper-archive entry"
    );
    // Any other service may list them in any order.
    let web_list = vec![archive(), entry(Installer::Apt), entry(Installer::Tarball)];
    assert_eq!(
        target::check(&web(), &one_list("linux-arm64", web_list)),
        Ok(())
    );
}
