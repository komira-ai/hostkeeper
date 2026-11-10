//! The coordinator's checks of a repository entry (packages-repositories.md section 2.2, build
//! item A16): one case per row of the section's table, under and over each limit, on an entry
//! that passes every other rule (`support::repository_entry`), every refusal `target_invalid`.
//! An entry's kind is told by `repository` alone, never by its installer's name.

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Target, TargetList};
use hostkeeper_release::grammar::{is_package_name, is_package_version, is_repository_name};
use hostkeeper_release::manifest::ManifestError;
use hostkeeper_release::target::{
    self, EntryError, TargetError, is_native_entry, is_repository_entry,
};
use hostkeeper_release::{ARCHIVE_INSTALLER, Installer};
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};
use prost_types::FileDescriptorSet;

use crate::support::{
    LINUX, attested_artifact, bare_manifest, digest, entry, location, manifest, repository_entry,
    targets, unlisted, web,
};

fn check_entry(entry: &Target) -> Result<(), EntryError> {
    target::check_entry(&web(), LINUX, entry)
}

/// The repository entry with `repository` set to `name`.
fn from(name: &str) -> Target {
    Target {
        repository: Some(name.to_owned()),
        ..repository_entry()
    }
}

/// The repository entry at the package version `version`.
fn at(version: &str) -> Target {
    Target {
        package_version: Some(version.to_owned()),
        ..repository_entry()
    }
}

/// The repository entry whose manifest names the package `package`.
fn of_package(package: Option<&str>) -> Target {
    let mut entry = repository_entry();
    entry.manifest.as_mut().expect("a manifest").package = package.map(str::to_owned);
    entry
}

/// Catches: the entry CI sends refused, or told from an artifact entry by anything but its
/// `repository`: an unknown installer's repository entry is one too, an `apt` entry without a
/// repository is an artifact entry and is checked as one, and an empty repository is set.
#[test]
fn a_repository_entry_is_one_whose_repository_is_set_whatever_its_installer() {
    let entry = repository_entry();
    assert!(is_repository_entry(&entry));
    assert_eq!(check_entry(&entry), Ok(()));
    let later = Target {
        installer: "dnf".to_owned(),
        ..repository_entry()
    };
    assert_eq!(
        check_entry(&later),
        Ok(()),
        "a later backend needs no change"
    );
    let artifact = Target {
        repository: None,
        package_version: None,
        ..repository_entry()
    };
    assert!(!is_repository_entry(&artifact));
    assert_eq!(
        check_entry(&artifact),
        Err(EntryError::ZeroBytes),
        "checked as an artifact entry"
    );
    assert!(is_repository_entry(&from("")));
    assert!(!is_repository_entry(&unlisted("tarball")));
}

/// Catches: a repository name outside `[a-z0-9-]` or over 32 bytes taken, one of 1 or 32 bytes
/// refused, or an empty one (set, but empty) read as no repository.
#[test]
fn a_repository_is_1_to_32_bytes_of_lower_case_letters_digits_and_hyphens() {
    let longest = "a".repeat(32);
    for good in [
        "komira-testing",
        "komira-stable",
        "a",
        "-",
        "0",
        longest.as_str(),
    ] {
        assert!(is_repository_name(good), "{good}");
        assert_eq!(check_entry(&from(good)), Ok(()), "{good}");
    }
    let long = "a".repeat(33);
    for bad in [
        "",
        long.as_str(),
        "Komira",
        "komira_testing",
        "komira testing",
        "komira.stable",
        "k/s",
    ] {
        assert!(!is_repository_name(bad), "{bad}");
        assert_eq!(
            check_entry(&from(bad)),
            Err(EntryError::Repository(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        EntryError::Repository("Komira".to_owned()).to_string(),
        "the repository \"Komira\" is not 1 to 32 bytes of a-z, 0-9 and `-`"
    );
}

/// Catches: a repository entry with no package version taken, a version outside the union of
/// dpkg's and rpm's grammars (over 128 bytes, a letter first, a space) taken, or one of them
/// (an epoch, a revision, a tilde, an underscore, 1 byte, 128 bytes) refused.
#[test]
fn a_package_version_is_required_and_is_1_to_128_bytes_starting_with_a_digit() {
    let none = Target {
        package_version: None,
        ..repository_entry()
    };
    assert_eq!(check_entry(&none), Err(EntryError::NoPackageVersion));
    assert_eq!(
        EntryError::NoPackageVersion.to_string(),
        "the repository entry has no package_version"
    );
    let longest = format!("1{}", "a".repeat(127));
    for good in [
        "1.6.0-1",
        "1:1.6.0-1",
        "2.0~rc1-0ubuntu1",
        "1.6.0+dfsg-2",
        "1.6.0_1",
        "0",
        longest.as_str(),
    ] {
        assert!(is_package_version(good), "{good}");
        assert_eq!(check_entry(&at(good)), Ok(()), "{good}");
    }
    let long = format!("{longest}0");
    for bad in [
        "",
        long.as_str(),
        "v1.6.0",
        "-1",
        ":1",
        "1.6.0 1",
        "1.6.0/1",
        "1.6.0@1",
        "1.6.0é",
    ] {
        assert!(!is_package_version(bad), "{bad}");
        assert_eq!(
            check_entry(&at(bad)),
            Err(EntryError::PackageVersion(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        EntryError::PackageVersion("v1".to_owned()).to_string(),
        "the package_version \"v1\" is not 1 to 128 bytes of A-Z, a-z, 0-9 and `.+~:_-` with a digit first"
    );
}

/// Catches: an artifact entry carrying a `package_version`, which a daemon would read as
/// half a repository entry; refused whatever its installer, and even when empty.
#[test]
fn a_package_version_on_an_entry_without_repository_is_refused() {
    for installer in ["tarball", "apt", "macos-pkg"] {
        for version in ["1.6.0-1", ""] {
            let half = Target {
                installer: installer.to_owned(),
                package_version: Some(version.to_owned()),
                ..unlisted("tarball")
            };
            assert!(!is_repository_entry(&half));
            assert_eq!(
                check_entry(&half),
                Err(EntryError::VersionWithoutRepository),
                "{installer} {version:?}"
            );
        }
    }
    assert_eq!(
        EntryError::VersionWithoutRepository.to_string(),
        "the entry has a package_version and no repository"
    );
}

/// Catches: a repository entry that names an artifact taken: a location, a size of even one
/// byte, or an attestation, which no one would check and which must not look checked.
#[test]
fn a_repository_entry_has_no_location_no_size_and_no_attestation() {
    let one_location = Target {
        locations: vec![location("lan", "releases/web/1.6.0/web.deb")],
        ..repository_entry()
    };
    let one_byte = Target {
        bytes: 1,
        ..repository_entry()
    };
    let attested = Target {
        attestation: Some(Blob {
            digest: digest('b'),
            bytes: 7311,
            locations: vec![location("github", "o/r/web.sigstore.json")],
        }),
        ..repository_entry()
    };
    let empty_bundle = Target {
        attestation: Some(Blob::default()),
        ..repository_entry()
    };
    for (entry, field) in [
        (one_location, "locations"),
        (one_byte, "bytes"),
        (attested, "attestation"),
        (empty_bundle, "attestation"),
    ] {
        assert_eq!(
            check_entry(&entry),
            Err(EntryError::ArtifactField(field)),
            "{field}"
        );
    }
    assert_eq!(
        EntryError::ArtifactField("attestation").to_string(),
        "the repository entry sets attestation, a field of an artifact; it names no artifact"
    );
}

/// Catches: a repository entry without its pin, or with one that is not 64 lower-case hex
/// characters (63 digits, 65, an upper-case letter, a letter past `f`), taken.
#[test]
fn a_repository_entry_needs_a_pin_of_64_lower_case_hex_characters() {
    let pinned = |pin: &str| Target {
        digest: pin.to_owned(),
        ..repository_entry()
    };
    assert_eq!(check_entry(&pinned(&"0123456789abcdef".repeat(4))), Ok(()));
    let upper = format!("A{}", "a".repeat(63));
    let not_hex = format!("g{}", "a".repeat(63));
    for bad in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        upper,
        not_hex,
    ] {
        assert_eq!(
            check_entry(&pinned(&bad)),
            Err(EntryError::Digest {
                of: "package",
                digest: bad.clone()
            }),
            "{bad:?}"
        );
    }
    assert_eq!(
        EntryError::Digest {
            of: "package",
            digest: String::new()
        }
        .to_string(),
        "the package digest \"\" is not 64 lower-case hex characters"
    );
}

/// Catches: a repository entry whose manifest does not name the package, or names one outside
/// the union of dpkg's and rpm's names (over 128 bytes, `-` or `.` first, a space or a `/`),
/// taken; or one of those names (a `+`, an `@`, a capital, 1 byte, 128 bytes) refused; or an
/// entry with no manifest at all taken.
#[test]
fn a_repository_entrys_manifest_names_the_package_by_its_grammar() {
    assert_eq!(check_entry(&of_package(None)), Err(EntryError::NoPackage));
    assert_eq!(
        EntryError::NoPackage.to_string(),
        "the manifest of a repository entry needs package"
    );
    let longest = format!("t{}", "x".repeat(127));
    for good in [
        "tierstore",
        "libstdc++6",
        "python3.11",
        "perl-Data-Dumper",
        "node@20",
        "0ad",
        "T",
        longest.as_str(),
    ] {
        assert!(is_package_name(good), "{good}");
        assert_eq!(check_entry(&of_package(Some(good))), Ok(()), "{good}");
    }
    let long = format!("{longest}x");
    for bad in [
        "",
        long.as_str(),
        "-tierstore",
        ".tierstore",
        "tier store",
        "tier/store",
        "tierstore:amd64",
    ] {
        assert!(!is_package_name(bad), "{bad}");
        assert_eq!(
            check_entry(&of_package(Some(bad))),
            Err(EntryError::Package(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        EntryError::Package("-x".to_owned()).to_string(),
        "the package \"-x\" is not 1 to 128 bytes of A-Z, a-z, 0-9 and `+._@-` with a letter or digit first"
    );
    let bare = Target {
        manifest: None,
        ..repository_entry()
    };
    assert_eq!(check_entry(&bare), Err(EntryError::NoManifest));
}

/// Catches: a repository entry's manifest let off the rules every manifest keeps (packages.md
/// section 4.4), or held to a rule of its installer, which is the daemon's.
#[test]
fn a_repository_entrys_manifest_keeps_the_rules_of_every_manifest() {
    let mut other_units = repository_entry();
    other_units.manifest.as_mut().expect("a manifest").run = vec!["api.service".to_owned()];
    assert!(matches!(
        check_entry(&other_units),
        Err(EntryError::Manifest(ManifestError::RunUnit { .. }))
    ));
    // `apt` refuses `place` and needs a hook (A7), on the daemon only.
    let mut daemons_rules = repository_entry();
    let manifest = daemons_rules.manifest.as_mut().expect("a manifest");
    manifest.hook = None;
    manifest.place = vec![crate::support::place("file", "files/a", "/etc/web/a")];
    assert_eq!(check_entry(&daemons_rules), Ok(()));
}

/// Catches: a repository entry over the size of every entry taken, or an installer name outside
/// the grammar judged after the repository's rules: those two are every entry's, and first.
#[test]
fn every_entrys_size_and_installer_name_are_checked_first() {
    let bad_name = Target {
        installer: "APT".to_owned(),
        repository: Some("Bad".to_owned()),
        ..repository_entry()
    };
    assert_eq!(
        check_entry(&bad_name),
        Err(EntryError::Installer("APT".to_owned()))
    );
    let mut large = repository_entry();
    large.manifest.as_mut().expect("a manifest").params.insert(
        "pad".to_owned(),
        crate::support::text(&"x".repeat(25 * 1024)),
    );
    large.repository = Some("Bad".to_owned());
    assert!(matches!(check_entry(&large), Err(EntryError::TooLarge(_))));
}

/// Catches: an entry that breaks several of the repository rules refused for another rule from
/// run to run: the repository, the version, the artifact fields, the pin, the manifest's package
/// and then its other rules, in that order.
#[test]
fn of_several_broken_rules_the_first_in_the_sections_order_is_the_answer() {
    let mut entry = Target {
        repository: Some("Bad".to_owned()),
        package_version: None,
        bytes: 1,
        digest: String::new(),
        ..repository_entry()
    };
    entry.manifest.as_mut().expect("a manifest").package = None;
    let mut answers = Vec::new();
    loop {
        match check_entry(&entry) {
            Ok(()) => break,
            Err(why) => {
                answers.push(why.clone());
                match why {
                    EntryError::Repository(_) => entry.repository = Some("komira-stable".into()),
                    EntryError::NoPackageVersion => entry.package_version = Some("1.0-1".into()),
                    EntryError::ArtifactField(_) => entry.bytes = 0,
                    EntryError::Digest { .. } => entry.digest = digest('e'),
                    EntryError::NoPackage => {
                        entry.manifest.as_mut().expect("a manifest").package = Some("web".into());
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
    }
    assert_eq!(
        answers,
        [
            EntryError::Repository("Bad".to_owned()),
            EntryError::NoPackageVersion,
            EntryError::ArtifactField("bytes"),
            EntryError::Digest {
                of: "package",
                digest: String::new()
            },
            EntryError::NoPackage,
        ]
    );
}

/// Catches: the service `hostkeeper` set with a repository entry refused, or with an archive
/// entry after it or alone passing, since its release is one native entry per platform; or a
/// repository entry that names `hostkeeper-archive` refused, which is an unknown installer's
/// name as any other is.
#[test]
fn the_service_hostkeeper_takes_a_repository_entry_alone() {
    let named_archive = Target {
        installer: ARCHIVE_INSTALLER.to_owned(),
        ..repository_entry()
    };
    assert_eq!(check_entry(&named_archive), Ok(()));
    assert!(is_native_entry(&named_archive));

    let hostkeeper = ServiceName::new("hostkeeper").expect("a name");
    // An archive entry as an earlier release's CI sent it: the manifest holds `min_hostkeeper`.
    let archive_entry = || attested_artifact(ARCHIVE_INSTALLER, bare_manifest());
    let mut own = repository_entry();
    let manifest = own.manifest.as_mut().expect("a manifest");
    manifest.run = vec!["hostkeeper.service".to_owned()];
    manifest.health_url = None;
    manifest.package = Some("hostkeeper".to_owned());
    assert_eq!(target::check_entry(&hostkeeper, LINUX, &own), Ok(()));
    // Its repository entry is its native one, alone (`native.rs` holds every shape).
    let alone = TargetList {
        entries: vec![own.clone()],
    };
    assert_eq!(
        target::check(&hostkeeper, &targets(&[("linux-arm64", alone)])),
        Ok(())
    );
    let after = TargetList {
        entries: vec![archive_entry(), own],
    };
    assert_eq!(
        target::check(&hostkeeper, &targets(&[("linux-arm64", after)])),
        Err(TargetError::SelfList {
            platform: Platform::LinuxArm64
        })
    );
    let archive_list = TargetList {
        entries: vec![archive_entry()],
    };
    assert_eq!(
        target::check(&hostkeeper, &targets(&[("linux-arm64", archive_list)])),
        Err(TargetError::SelfList {
            platform: Platform::LinuxArm64
        })
    );
}

/// Catches: a list of `apt` from a repository, then an artifact entry of a name no daemon lists,
/// refused, or two `apt` entries passing because one is a repository entry and one is not.
#[test]
fn a_list_of_a_repository_entry_and_an_unlisted_artifact_passes_and_no_installer_is_named_twice() {
    let sent = TargetList {
        entries: vec![repository_entry(), unlisted("tarball")],
    };
    assert_eq!(
        target::check(&web(), &targets(&[("linux-x86_64", sent)])),
        Ok(())
    );
    let artifact_apt = Target {
        installer: "apt".to_owned(),
        manifest: Some(manifest(Installer::Apt)),
        ..unlisted("tarball")
    };
    let twice = TargetList {
        entries: vec![repository_entry(), artifact_apt],
    };
    assert_eq!(
        target::check(&web(), &targets(&[("linux-x86_64", twice)])),
        Err(TargetError::DuplicateInstaller {
            platform: Platform::LinuxX86_64,
            installer: "apt".to_owned()
        })
    );
}

/// The `Target` of the release before A16: this build's without fields 7 and 8.
fn target_before() -> prost_reflect::MessageDescriptor {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    for file in &mut set.file {
        for message in &mut file.message_type {
            if file.name.as_deref() == Some("hostkeeper/release/v1/release.proto")
                && message.name() == "Target"
            {
                message.field.retain(|f| f.number() < 7);
                message.oneof_decl.clear();
            }
        }
    }
    DescriptorPool::from_file_descriptor_set(set)
        .expect("builds")
        .get_message_by_name(&format!("hostkeeper.release.v1.{}", "Target"))
        .expect("Target")
}

/// Catches: an entry that a coordinator or CI of the release before wrote (it has no field 7
/// or 8) judged as a repository entry, or refused, by this build: it is an artifact entry, and
/// passes as it did.
#[test]
fn an_entry_written_by_the_release_before_is_an_artifact_entry_and_passes_as_before() {
    for (written, platform) in [
        (unlisted("tarball"), LINUX),
        (entry(Installer::MacosPkg), Platform::MacosArm64),
        (attested_artifact(ARCHIVE_INSTALLER, bare_manifest()), LINUX),
    ] {
        let installer = written.installer.clone();
        let mut before = DynamicMessage::new(target_before());
        before
            .transcode_from(&written)
            .expect("the release before writes it");
        let read = Target::decode(before.encode_to_vec().as_slice()).expect("decodes");
        assert!(!is_repository_entry(&read), "{installer}");
        assert_eq!(read.package_version, None, "{installer}");
        assert_eq!(
            target::check_entry(&web(), platform, &read),
            Ok(()),
            "{installer}"
        );
    }
}
