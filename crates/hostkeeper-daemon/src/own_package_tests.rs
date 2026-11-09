//! The rules of hostkeeper's own package that need no host.

use hostkeeper_placed::{InstalledEntry, PlacedRecord};
use hostkeeper_proto::hostkeeper::release::v1::Target;

use super::{at_version, deferred, has_version};

/// The version the package manager shows is the release that runs with or without a Debian
/// revision, and not a release that merely starts with its text.
#[test]
fn a_package_version_is_the_release_with_or_without_a_revision() {
    assert!(at_version("1.2.3", "1.2.3"));
    assert!(at_version("1.2.3-1", "1.2.3"));
    assert!(at_version("1.2.3-0ubuntu2", "1.2.3"));
    for other in ["1.2.30", "1.2.3+b1", "1.2", "11.2.3", ""] {
        assert!(!at_version(other, "1.2.3"), "{other}");
    }
}

/// The switch of an update of hostkeeper is left to its new binary for a native version, and on
/// a host where hostkeeper runs from its native package whatever the version; an archive version
/// on a host that runs hostkeeper from archives keeps the step runner's switch.
#[test]
fn the_switch_is_left_to_the_new_binary_for_a_native_version_or_host() {
    let archive = PlacedRecord::default();
    let mut moved = PlacedRecord::default();
    moved
        .installed
        .insert("hostkeeper".to_owned(), InstalledEntry::new("apt"));
    let native = Target {
        installer: "apt".to_owned(),
        ..Target::default()
    };
    assert!(!deferred(None, &archive));
    assert!(deferred(Some(&native), &archive));
    assert!(deferred(None, &moved));
    assert!(deferred(Some(&native), &moved));
}

/// A service directory holds a version when any entry is not a link, a seal or a stage's
/// leftover; a missing directory holds none, and one that cannot be read is an error.
#[test]
fn a_version_is_any_entry_but_the_links_seals_and_leftovers() {
    // A unit test has no `CARGO_TARGET_TMPDIR`: the binary is in `<target>/<profile>/deps`.
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    let scratch =
        hostkeeper_testkit::Scratch::under(&target.join("tmp"), "own-has-version").unwrap();
    let dir = scratch.path().join("hostkeeper");
    assert!(!has_version(&dir).unwrap());
    std::fs::create_dir(&dir).unwrap();
    for name in ["current", "previous", "1.0.0.seal", ".stage-x"] {
        std::fs::write(dir.join(name), "").unwrap();
    }
    assert!(!has_version(&dir).unwrap());
    std::fs::create_dir(dir.join("1.0.0")).unwrap();
    assert!(has_version(&dir).unwrap());
    let file = scratch.path().join("a-file");
    std::fs::write(&file, "").unwrap();
    assert!(has_version(&file).is_err());
}
