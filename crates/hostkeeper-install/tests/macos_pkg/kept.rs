//! The payload files a first install lands on that no receipt lists (packages-lifecycle.md
//! section 6.5): kept by the first install's `activate`, and put back from the store once
//! `remove` has deleted every file the receipt lists, as the daemon runs the two; never kept for
//! a file a receipt lists, for an update, or when nothing was there.

use std::os::unix::fs::MetadataExt;

use hostkeeper_install::kept::KeptSet;

use super::*;

fn service() -> ServiceName {
    ServiceName::new(SERVICE).unwrap()
}

/// The bytes, mode, owner and group at the logical `path`.
fn seen(rig: &Rig, path: &str) -> (Vec<u8>, u32, u32, u32) {
    let meta = fs::symlink_metadata(rig.real(path)).unwrap();
    (
        rig.read(path).unwrap(),
        meta.mode() & 0o7777,
        meta.uid(),
        meta.gid(),
    )
}

/// The kept files' logical paths.
fn kept_paths(rig: &Rig) -> Option<Vec<String>> {
    rig.kept
        .set_of(&service())
        .unwrap()
        .map(|set| set.files.into_iter().map(|file| file.path).collect())
}

/// Catches: a first install whose removal deletes the operator's file at a payload path.
#[test]
fn a_file_no_receipt_lists_is_kept_at_a_first_install_and_put_back_after_its_removal() {
    let rig = Rig::new("pkg-kept-roundtrip");
    rig.write(&home("VERSION"), b"hand-placed", 0o640);
    let before = seen(&rig, &home("VERSION"));
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    assert_eq!(rig.read_text(&home("VERSION")), "1.0");
    assert_eq!(kept_paths(&rig), Some(vec![home("VERSION")]));
    let set = rig.kept.set_of(&service()).unwrap().unwrap();
    assert_eq!(set.package, IDENTIFIER);

    rig.pkg().remove(&order, &[&dir]).unwrap();
    assert!(!rig.exists(&home("VERSION")), "remove deletes what the receipt lists");
    assert_eq!(rig.kept.put_back(&service()).unwrap(), Vec::new());
    assert_eq!(seen(&rig, &home("VERSION")), before);
    rig.kept.drop_set(&service()).unwrap();
    assert_eq!(rig.kept.set_of(&service()).unwrap(), None);
}

/// Catches (c): a file another receipt lists being kept; the install is refused as today
/// (`package_conflict`), with the file untouched.
#[test]
fn a_file_another_receipt_lists_is_not_kept_and_the_install_is_refused_as_today() {
    let rig = Rig::new("pkg-kept-other");
    rig.write(&home("VERSION"), b"the other package's", 0o644);
    rig.mac.set_receipt(
        "com.other.pkg",
        "3",
        &["Library/Application Support/svc/VERSION"],
    );
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    assert_eq!(
        reason(rig.activate(&order, &dir, None)),
        Reason::PackageConflict
    );
    assert_eq!(rig.read_text(&home("VERSION")), "the other package's");
    assert_eq!(kept_paths(&rig), None);
    no_install_ran(&rig);
}

/// Catches: a file the package's own receipt lists (a package installed by hand, drift) taken
/// for the operator's.
#[test]
fn a_file_the_packages_own_receipt_lists_is_not_kept() {
    let rig = Rig::new("pkg-kept-own");
    rig.write(&home("VERSION"), b"0.9", 0o644);
    rig.mac
        .set_receipt(IDENTIFIER, "0.9", &["Library/Application Support/svc/VERSION"]);
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    assert_eq!(kept_paths(&rig), None);
}

/// Catches (d): a first install over no file at any payload path keeping something.
#[test]
fn a_first_install_over_no_file_keeps_nothing_and_its_removal_leaves_the_paths_empty() {
    let i = installed("pkg-kept-none");
    assert!(!i.rig.kept.dir_of(&service()).exists());
    i.rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    assert_eq!(i.rig.kept.put_back(&service()).unwrap(), Vec::new());
    assert!(!i.rig.exists(&home("VERSION")));
}

/// Catches: an update keeping a hand-placed file at a path it adds, or dropping the first
/// install's set.
#[test]
fn an_update_keeps_the_first_installs_set_and_keeps_nothing_new() {
    let rig = Rig::new("pkg-kept-update");
    rig.write(&home("VERSION"), b"hand-placed", 0o600);
    rig.mac.add(v1());
    let order1 = rig.order_run("1.0", &BOTH);
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    let set: Option<KeptSet> = rig.kept.set_of(&service()).unwrap();

    rig.write(&home("lib/x"), b"hand-placed too", 0o600);
    rig.mac.add(v2());
    let order2 = rig.order("2.0");
    let dir2 = rig.stage(&order2, "svc-2.0");
    rig.activate(&order2, &dir2, replaces(&dir1, &order1))
        .unwrap();
    assert_eq!(rig.kept.set_of(&service()).unwrap(), set);
}

/// Catches: a first install that goes on when its copy cannot be kept.
#[test]
fn a_first_install_whose_copy_cannot_be_kept_installs_nothing() {
    let rig = Rig::new("pkg-kept-fails");
    rig.write(&home("VERSION"), b"hand-placed", 0o600);
    fs::write(rig.scratch.path().join("kept"), "a file where the store goes").unwrap();
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    let failure = rig.activate(&order, &dir, None).unwrap_err();
    assert_eq!(failure.reason(), Reason::IoError, "{failure}");
    assert!(
        failure.to_string().contains("the kept files of svc: "),
        "{failure}"
    );
    assert_eq!(rig.read_text(&home("VERSION")), "hand-placed");
    no_install_ran(&rig);
    assert_eq!(rig.records.get(SERVICE), None);
}
