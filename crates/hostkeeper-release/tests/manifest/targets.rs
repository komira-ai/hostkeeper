//! The coordinator's checks of a set's targets (section 3.4): the platforms, the lists and each
//! entry, every one `target_invalid`.

use std::collections::BTreeMap;

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::{Platform, UnknownPlatform};
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Target, TargetList};
use hostkeeper_release::grammar::LocatorError;
use hostkeeper_release::manifest::ManifestError;
use hostkeeper_release::target::{self, EntryError, TargetError};
use hostkeeper_release::{
    ARCHIVE_INSTALLER, Installer, MAX_ATTESTATION_BYTES, MAX_ENTRY_BYTES,
    MAX_LOCATOR_BYTES_PER_ENTRY,
};
use prost::Message as _;

use crate::support::{
    LINUX, list, location, named, named_list, pad_manifest_to, targets, unlisted, web,
};

fn check(targets: &BTreeMap<String, TargetList>) -> Result<(), TargetError> {
    target::check(&web(), targets)
}

fn check_entry(entry: &Target) -> Result<(), EntryError> {
    target::check_entry(&web(), LINUX, entry)
}

/// Artifact entries of installers no daemon lists any more (`deb`, `rpm`) beside a `tarball`: a
/// set naming withdrawn installers is still accepted, and no host is ever picked for them.
const NATIVE: [&str; 3] = ["deb", "rpm", "tarball"];

/// Catches: the requested shape refused: three platforms of one list each, or a null platform
/// left out.
#[test]
fn a_release_of_lists_for_some_platforms_passes() {
    let all = targets(&[
        ("linux-x86_64", named_list(&NATIVE)),
        ("linux-arm64", named_list(&["tarball"])),
        ("macos-arm64", list(&[Installer::MacosPkg])),
    ]);
    assert_eq!(check(&all), Ok(()));
    let one = targets(&[("macos-arm64", list(&[Installer::MacosPkg]))]);
    assert_eq!(check(&one), Ok(()));
}

/// Catches: a set with no platform, or a key that is not one of the three, passing.
#[test]
fn the_targets_name_one_to_three_of_the_platforms_and_no_other_key() {
    assert_eq!(check(&BTreeMap::new()), Err(TargetError::NoPlatform));
    assert_eq!(
        TargetError::NoPlatform.to_string(),
        "the targets name no platform"
    );
    for key in ["linux-riscv64", "Linux-x86_64", "linux_x86_64", ""] {
        let set = targets(&[
            ("linux-x86_64", named_list(&NATIVE)),
            (key, named_list(&NATIVE)),
        ]);
        assert_eq!(
            check(&set),
            Err(TargetError::UnknownPlatform(UnknownPlatform(
                key.to_owned()
            ))),
            "{key}"
        );
    }
    assert_eq!(
        TargetError::UnknownPlatform(UnknownPlatform("linux-riscv64".to_owned())).to_string(),
        "the targets key \"linux-riscv64\" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64"
    );
}

/// `count` entries, each of its own installer name.
fn entries(count: usize) -> TargetList {
    TargetList {
        entries: (0..count)
            .map(|i| Target {
                installer: format!("format-{i}"),
                ..unlisted("tarball")
            })
            .collect(),
    }
}

/// Catches: an empty list or one of 9 entries passing, or one of 8 refused.
#[test]
fn a_list_holds_1_to_8_entries() {
    assert_eq!(check(&targets(&[("linux-arm64", entries(8))])), Ok(()));
    for count in [0, 9] {
        assert_eq!(
            check(&targets(&[("linux-arm64", entries(count))])),
            Err(TargetError::Entries {
                platform: Platform::LinuxArm64,
                count
            }),
            "{count}"
        );
    }
    assert_eq!(
        TargetError::Entries {
            platform: Platform::LinuxArm64,
            count: 9
        }
        .to_string(),
        "the list of linux-arm64 has 9 entries, not 1 to 8"
    );
}

/// Catches: two entries of one installer in a list passing, though the second could never be
/// picked; or one installer in two platforms' lists refused.
#[test]
fn a_list_names_no_installer_twice() {
    let twice = targets(&[("linux-x86_64", named_list(&["deb", "tarball", "deb"]))]);
    assert_eq!(
        check(&twice),
        Err(TargetError::DuplicateInstaller {
            platform: Platform::LinuxX86_64,
            installer: "deb".to_owned()
        })
    );
    assert_eq!(
        check(&twice).expect_err("refused").to_string(),
        "the list of linux-x86_64 has two entries of the installer \"deb\""
    );
    let across = targets(&[
        ("linux-x86_64", named_list(&["tarball"])),
        ("linux-arm64", named_list(&["tarball"])),
    ]);
    assert_eq!(check(&across), Ok(()));
}

/// Catches: the service `hostkeeper` set with an archive entry alone, or with one of any other
/// format, which no daemon could install; or another service held to it. Its native entries are
/// `native.rs`'s.
#[test]
fn hostkeeper_is_set_with_no_installer_but_its_native_one() {
    let hostkeeper = ServiceName::new("hostkeeper").expect("a name");
    // Manifests of hostkeeper's own: no unit or placed file of the service `web`.
    let own = |names: &[&str]| {
        let mut list = named_list(names);
        for entry in &mut list.entries {
            let manifest = entry.manifest.as_mut().expect("a manifest");
            manifest.run.clear();
            manifest.place.clear();
        }
        list
    };
    for bad in [
        own(&[ARCHIVE_INSTALLER]),
        own(&["tarball"]),
        own(&[ARCHIVE_INSTALLER, "tarball"]),
        own(&["deb", ARCHIVE_INSTALLER]),
    ] {
        let set = targets(&[("linux-arm64", bad)]);
        assert_eq!(
            target::check(&hostkeeper, &set),
            Err(TargetError::SelfList {
                platform: Platform::LinuxArm64
            })
        );
    }
    let other = targets(&[("linux-arm64", named_list(&["tarball", ARCHIVE_INSTALLER]))]);
    assert_eq!(check(&other), Ok(()));
}

/// Catches: an entry naming one of the withdrawn installers (`hostkeeper-archive`, `tarball`,
/// `oci-image`) refused, though a set that names one is accepted as one that names any unknown
/// installer is, whatever manifest it carries.
#[test]
fn an_entry_naming_a_withdrawn_installer_is_accepted_as_any_unknown_name_is() {
    for name in [ARCHIVE_INSTALLER, "tarball", "oci-image"] {
        assert_eq!(check_entry(&named(name)), Ok(()), "{name}");
        assert_eq!(check_entry(&unlisted(name)), Ok(()), "{name}");
        let set = targets(&[("linux-x86_64", named_list(&[name, "deb"]))]);
        assert_eq!(check(&set), Ok(()), "{name}");
    }
}

/// Catches: two broken platforms refused for a different one from run to run.
#[test]
fn of_several_broken_platforms_the_first_spelling_is_the_answer() {
    let set = targets(&[
        ("macos-arm64", entries(0)),
        ("linux-x86_64", entries(9)),
        ("linux-arm64", entries(0)),
    ]);
    assert_eq!(
        check(&set),
        Err(TargetError::Entries {
            platform: Platform::LinuxArm64,
            count: 0
        })
    );
}

/// Catches: the answer depending on the order the caller's map yields its keys (a map that
/// hashes them yields another order on each run): two unknown keys, and an unknown key beside
/// a broken list, given in both orders, are refused for the same one.
#[test]
fn the_answer_does_not_depend_on_the_order_the_keys_are_given_in() {
    let good = named_list(&NATIVE);
    let empty = entries(0);
    let keys = [
        ("windows-x86_64".to_owned(), good.clone()),
        ("linux-riscv64".to_owned(), good.clone()),
        ("macos-arm64".to_owned(), empty.clone()),
    ];
    let forward: Vec<(&String, &TargetList)> = keys.iter().map(|(k, l)| (k, l)).collect();
    let backward: Vec<(&String, &TargetList)> = forward.iter().rev().copied().collect();
    for order in [forward, backward] {
        assert_eq!(
            target::check(&web(), order),
            Err(TargetError::UnknownPlatform(UnknownPlatform(
                "linux-riscv64".to_owned()
            )))
        );
    }
    let lists = [
        ("macos-arm64".to_owned(), empty.clone()),
        ("linux-x86_64".to_owned(), good),
        ("linux-arm64".to_owned(), empty),
    ];
    let forward: Vec<(&String, &TargetList)> = lists.iter().map(|(k, l)| (k, l)).collect();
    let backward: Vec<(&String, &TargetList)> = forward.iter().rev().copied().collect();
    for order in [forward, backward] {
        assert_eq!(
            target::check(&web(), order),
            Err(TargetError::Entries {
                platform: Platform::LinuxArm64,
                count: 0
            })
        );
    }
}

/// Catches: the coordinator judging a list's unit names by another platform than the list's.
#[test]
fn each_lists_manifests_are_judged_by_its_own_platform() {
    let set = targets(&[
        ("macos-arm64", list(&[Installer::MacosPkg])),
        ("linux-arm64", named_list(&["deb"])),
    ]);
    assert_eq!(check(&set), Ok(()));
    let crossed = targets(&[("macos-arm64", named_list(&["deb"]))]);
    assert!(matches!(
        check(&crossed),
        Err(TargetError::Entry {
            platform: Platform::MacosArm64,
            at: 0,
            why: EntryError::Manifest(ManifestError::RunUnit { .. })
        })
    ));
}

/// Catches: an entry's error reported without its platform and place, or not at all.
#[test]
fn an_entry_that_breaks_a_rule_is_named_by_platform_and_place() {
    let mut bad = named_list(&NATIVE);
    bad.entries[1].bytes = 0;
    let set = targets(&[("linux-x86_64", bad)]);
    let err = check(&set).expect_err("refused");
    assert_eq!(
        err,
        TargetError::Entry {
            platform: Platform::LinuxX86_64,
            at: 1,
            why: EntryError::ZeroBytes
        }
    );
    assert_eq!(
        err.to_string(),
        "entry 1 of linux-x86_64: the artifact's bytes is 0"
    );
}

/// Catches: an installer name outside the grammar passing, one at its limit refused, or an
/// unknown installer of the grammar refused (no daemon lists it, so it is never picked).
#[test]
fn an_installer_is_1_to_32_bytes_of_lower_case_letters_digits_and_hyphens() {
    let named = |name: &str| Target {
        installer: name.to_owned(),
        ..unlisted("tarball")
    };
    let longest = "a".repeat(32);
    for good in ["deb", "flatpak", "snap-2", longest.as_str(), "-"] {
        assert_eq!(check_entry(&named(good)), Ok(()), "{good}");
    }
    let long = "a".repeat(33);
    for bad in ["", long.as_str(), "Deb", "deb_x", "deb x", "débian"] {
        assert_eq!(
            check_entry(&named(bad)),
            Err(EntryError::Installer(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        EntryError::Installer("Deb".to_owned()).to_string(),
        "the installer \"Deb\" is not 1 to 32 bytes of a-z, 0-9 and `-`"
    );
}

/// Catches: a digest of 63 hex digits, with an upper-case letter or a non-hex letter passing,
/// for the artifact or the attestation.
#[test]
fn every_digest_is_64_lower_case_hex_characters() {
    let upper = format!("A{}", "a".repeat(63));
    let short = "a".repeat(63);
    let long = "a".repeat(65);
    let letter = format!("g{}", "a".repeat(63));
    for bad in [
        upper.as_str(),
        short.as_str(),
        long.as_str(),
        letter.as_str(),
        "",
    ] {
        let artifact = Target {
            digest: bad.to_owned(),
            ..named("deb")
        };
        assert_eq!(
            check_entry(&artifact),
            Err(EntryError::Digest {
                of: "artifact",
                digest: bad.to_owned()
            }),
            "{bad}"
        );
        let mut bundle = named("deb");
        bundle.attestation.as_mut().expect("a bundle").digest = bad.to_owned();
        assert_eq!(
            check_entry(&bundle),
            Err(EntryError::Digest {
                of: "attestation",
                digest: bad.to_owned()
            }),
            "{bad}"
        );
    }
    let digits = Target {
        digest: "0123456789abcdef".repeat(4),
        ..named("deb")
    };
    assert_eq!(check_entry(&digits), Ok(()));
    assert_eq!(
        EntryError::Digest {
            of: "artifact",
            digest: short.clone()
        }
        .to_string(),
        format!("the artifact digest {short:?} is not 64 lower-case hex characters")
    );
}

/// Catches: an artifact of 0 bytes passing, or one of 1 byte refused.
#[test]
fn an_artifact_is_1_byte_or_more() {
    let sized = |bytes| Target {
        bytes,
        ..named("deb")
    };
    assert_eq!(check_entry(&sized(0)), Err(EntryError::ZeroBytes));
    assert_eq!(check_entry(&sized(1)), Ok(()));
    assert_eq!(check_entry(&sized(u64::MAX)), Ok(()));
}

/// Catches: an entry with no attestation, or one of 0 bytes or over 1 MiB, passing; one of 1
/// byte or of exactly 1 MiB refused.
#[test]
fn an_attestation_is_required_and_1_byte_to_1_mib() {
    let none = Target {
        attestation: None,
        ..named("deb")
    };
    assert_eq!(check_entry(&none), Err(EntryError::NoAttestation));
    assert_eq!(
        EntryError::NoAttestation.to_string(),
        "the entry has no attestation"
    );
    for (bytes, ok) in [
        (0, false),
        (1, true),
        (MAX_ATTESTATION_BYTES, true),
        (MAX_ATTESTATION_BYTES + 1, false),
    ] {
        let mut e = named("deb");
        e.attestation.as_mut().expect("a bundle").bytes = bytes;
        let want = if ok {
            Ok(())
        } else {
            Err(EntryError::AttestationBytes(bytes))
        };
        assert_eq!(check_entry(&e), want, "{bytes}");
    }
    assert_eq!(
        EntryError::AttestationBytes(0).to_string(),
        "the attestation's bytes is 0, not 1 to 1048576"
    );
}

/// Catches: an artifact or an attestation with no location or with 9 passing, or one with 8
/// refused.
#[test]
fn an_artifact_and_its_attestation_each_have_1_to_8_locations() {
    let at = |count: usize| {
        (0..count)
            .map(|i| location("github", &format!("r/{i}")))
            .collect::<Vec<_>>()
    };
    for (count, ok) in [(0, false), (1, true), (8, true), (9, false)] {
        let artifact = Target {
            locations: at(count),
            ..named("deb")
        };
        let want = |of| {
            if ok {
                Ok(())
            } else {
                Err(EntryError::Locations { of, count })
            }
        };
        assert_eq!(check_entry(&artifact), want("artifact"), "{count}");
        let bundle = Target {
            attestation: Some(Blob {
                locations: at(count),
                ..named("deb").attestation.expect("a bundle")
            }),
            ..named("deb")
        };
        assert_eq!(check_entry(&bundle), want("attestation"), "{count}");
    }
    assert_eq!(
        EntryError::Locations {
            of: "artifact",
            count: 9
        }
        .to_string(),
        "the artifact has 9 locations, not 1 to 8"
    );
}

/// Catches: a location whose store is not a store's name, or whose locator breaks the grammar,
/// passing, for the artifact or the attestation.
#[test]
fn every_location_names_a_store_and_a_locator_of_the_grammar() {
    let long = "s".repeat(33);
    for store in ["", "GitHub", "lan_mirror", long.as_str()] {
        let e = Target {
            locations: vec![location(store, "a/b")],
            ..named("deb")
        };
        assert_eq!(
            check_entry(&e),
            Err(EntryError::Store(store.to_owned())),
            "{store}"
        );
    }
    assert_eq!(
        check_entry(&Target {
            locations: vec![location(&"s".repeat(32), "a/b")],
            ..named("deb")
        }),
        Ok(())
    );
    let mut bundle = named("deb");
    bundle.attestation.as_mut().expect("a bundle").locations = vec![location("github", "../x")];
    assert_eq!(
        check_entry(&bundle),
        Err(EntryError::Locator {
            locator: "../x".to_owned(),
            why: LocatorError::DotSegment
        })
    );
    assert_eq!(
        check_entry(&bundle).expect_err("refused").to_string(),
        "the locator \"../x\" has a `.` or `..` segment"
    );
    assert_eq!(
        EntryError::Store("GitHub".to_owned()).to_string(),
        "the store \"GitHub\" is not 1 to 32 bytes of a-z, 0-9 and `-`"
    );
}

/// An entry whose locators, the artifact's and the attestation's, are `total` bytes in all.
fn locators_of(total: usize) -> Target {
    let mut e = named("deb");
    // Three locators of the artifact and one of the bundle, the last taking the remainder.
    let part = total / 4;
    e.locations = vec![
        location("github", &"a".repeat(part)),
        location("lan", &"b".repeat(part)),
        location("lan", &"c".repeat(part)),
    ];
    e.attestation.as_mut().expect("a bundle").locations =
        vec![location("github", &"d".repeat(total - 3 * part))];
    e
}

/// Catches: an entry over 2 KiB of locators passing, the count taken over the artifact's alone,
/// or an entry of exactly 2 KiB refused.
#[test]
fn an_entry_holds_at_most_2_kib_of_locators_in_all() {
    assert_eq!(
        check_entry(&locators_of(MAX_LOCATOR_BYTES_PER_ENTRY)),
        Ok(())
    );
    assert_eq!(
        check_entry(&locators_of(MAX_LOCATOR_BYTES_PER_ENTRY + 1)),
        Err(EntryError::LocatorBytes(2049))
    );
    assert_eq!(
        EntryError::LocatorBytes(2049).to_string(),
        "the entry's locators are 2049 bytes in all, more than 2048"
    );
}

/// An entry that encodes to exactly `bytes`, by padding its manifest.
fn entry_of(bytes: usize) -> Target {
    let mut e = unlisted("tarball");
    let rest = e.encoded_len() - e.manifest.as_ref().expect("a manifest").encoded_len();
    // The manifest's length prefix grows with it; aim a little short, then settle.
    let mut manifest = e.manifest.take().expect("a manifest");
    pad_manifest_to(&mut manifest, bytes - rest - 3);
    e.manifest = Some(manifest);
    while e.encoded_len() < bytes {
        let m = e.manifest.as_mut().expect("a manifest");
        let len = m.encoded_len();
        pad_manifest_to(m, len + 1);
    }
    assert_eq!(e.encoded_len(), bytes);
    e
}

/// Catches: an entry over 24 KiB passing, or the size measured on something other than its
/// encoding. An entry can reach 24 KiB only with a manifest over 16 KiB, so one of exactly
/// 24 KiB passes the entry's limit and is refused for its manifest.
#[test]
fn an_entry_of_24_kib_passes_its_limit_and_one_byte_more_is_refused() {
    let over = entry_of(MAX_ENTRY_BYTES + 1);
    assert_eq!(check_entry(&over), Err(EntryError::TooLarge(24_577)));
    assert_eq!(
        EntryError::TooLarge(24_577).to_string(),
        "the entry is 24577 bytes encoded, more than 24576"
    );
    let at = entry_of(MAX_ENTRY_BYTES);
    assert!(matches!(
        check_entry(&at),
        Err(EntryError::Manifest(ManifestError::TooLarge { .. }))
    ));
}

/// Catches: an entry with no manifest, or one whose manifest breaks a rule, passing; or the
/// coordinator applying a rule of the entry's installer.
#[test]
fn every_entry_has_a_manifest_passing_the_rules_of_every_manifest() {
    let none = Target {
        manifest: None,
        ..named("deb")
    };
    assert_eq!(check_entry(&none), Err(EntryError::NoManifest));
    assert_eq!(
        EntryError::NoManifest.to_string(),
        "the entry has no manifest"
    );
    let mut bad = named("deb");
    bad.manifest.as_mut().expect("a manifest").run = vec!["api.service".to_owned()];
    let err = check_entry(&bad).expect_err("refused");
    assert!(matches!(
        err,
        EntryError::Manifest(ManifestError::RunUnit { .. })
    ));
    assert_eq!(
        err.to_string(),
        "the run entry \"api.service\" does not begin with the service name, alone or followed by `-`"
    );
    // A deb manifest without its package: the installer's rule, the daemon's alone.
    let mut native = named("deb");
    native.manifest.as_mut().expect("a manifest").package = None;
    assert_eq!(check_entry(&native), Ok(()));
}
