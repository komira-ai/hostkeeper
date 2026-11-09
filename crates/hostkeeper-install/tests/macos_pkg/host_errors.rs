//! Tests of the steps when the host's files or a path cannot be read: a path that is not UTF-8,
//! a host whose directory is swapped for a file between two steps (every later look at it fails
//! with `ENOTDIR`, not with "not found"), and a directory no one can write.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::*;

fn bytes_path(raw: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(raw))
}

/// A 2.0 that lists no file and runs no job, so a step that reads the host's files finds none
/// of 2.0's to read.
fn v2_bare() -> FakePkg {
    let pkg = FakePkg::standard(SERVICE, IDENTIFIER, "2.0");
    let paths = [
        pkg.path(SERVICE),
        pkg.path("VERSION"),
        "/Library/LaunchDaemons/svc.plist".into(),
    ];
    paths.iter().fold(pkg, |pkg, path| pkg.without(path))
}

/// A 1.0 with two empty directories one in the other, which 2.0 does not list.
fn v1_with_groups() -> FakePkg {
    FakePkg::standard(SERVICE, IDENTIFIER, "1.0")
        .dir("grp")
        .dir("grp/inner")
}

fn swap_root(rig: &Rig) -> Fault {
    Fault::root_becomes_a_file(&rig.root)
}

/// Whether a directory without write permission denies the test user: not when it is root.
fn permissions_deny(rig: &Rig) -> bool {
    rig.euid != 0
}

fn lock_dir(rig: &Rig, logical: &str) {
    fs::set_permissions(rig.real(logical), fs::Permissions::from_mode(0o555)).unwrap();
}

fn unlock_dir(rig: &Rig, logical: &str) {
    fs::set_permissions(rig.real(logical), fs::Permissions::from_mode(0o755)).unwrap();
}

/// Catches: a path that is not UTF-8 reaching a tool, or being reported as anything but
/// `io_error`.
#[test]
fn stage_reports_io_error_for_an_artifact_or_staging_path_that_is_not_utf8() {
    let rig = Rig::new("stage-utf8");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let err = rig
        .pkg()
        .stage(&order, bytes_path(b"/x/\xff.pkg"), &rig.packages)
        .unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(rig.mac.calls().is_empty());

    let staging = rig.packages.join(OsStr::from_bytes(b"st\xffg"));
    let err = rig
        .pkg()
        .stage(&order, &rig.artifact("svc-1.0"), &staging)
        .unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert_eq!(
        only_verbs(&rig, Tool::Pkgutil),
        ["--check-signature"],
        "nothing was expanded"
    );
}

/// Catches: an expanded payload that cannot be walked (a name that is not UTF-8) being staged.
#[test]
fn stage_reports_io_error_for_a_payload_that_cannot_be_walked() {
    let rig = Rig::new("stage-walk");
    rig.mac.add(v1());
    let odd = rig
        .packages
        .join("svc/.stage-0/expanded/Payload/Library")
        .join(OsStr::from_bytes(b"\xff"));
    rig.mac.fault(
        Tool::Lsbom,
        &[],
        Fault::hook(move || fs::write(&odd, b"x").unwrap()),
    );
    let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
    let err = result.unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(
        err.detail().starts_with("the expanded payload"),
        "{}",
        err.detail()
    );
}

/// Catches: a component file that vanishes before it is copied into the staging directory
/// being reported as a success.
#[test]
fn stage_reports_io_error_for_a_component_file_that_cannot_be_copied() {
    let rig = Rig::new("stage-copy");
    rig.mac.add(v1());
    let gone = rig.packages.join("svc/.stage-0/expanded/PackageInfo");
    rig.mac.fault(
        Tool::Lsbom,
        &[],
        Fault::hook(move || fs::remove_file(&gone).unwrap()),
    );
    let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
    let err = result.unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(err.detail().starts_with("PackageInfo"), "{}", err.detail());
}

/// Catches: a host that cannot be read being treated as a host with nothing on it, at each
/// look `activate` takes at the host.
#[test]
fn activate_reports_io_error_at_each_look_at_a_host_it_cannot_read() {
    // The parents of the payload paths, before anything else.
    let rig = Rig::new("act-host-parents");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.mac
        .fault_nth(Tool::Pkgutil, &["--pkg-info-plist"], 0, 1, swap_root(&rig));
    let err = rig.activate(&order, &dir, None).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(err.detail().starts_with("/Library: "), "{}", err.detail());
    assert!(rig.mac.calls_of(Tool::Installer).is_empty());

    // A payload file already on the host, looked at before its owners are asked for.
    let i = installed("act-host-owners");
    let (dir2, order2) = i.stage_v2();
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--file-info-plist"],
        0,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(err.detail().starts_with(&home("lib/x")), "{}", err.detail());

    // Each payload entry, looked at for the directories to record.
    let i = installed("act-host-created");
    let (dir2, order2) = i.stage_v2();
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--file-info-plist"],
        2,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(err.detail().starts_with("/Library: "), "{}", err.detail());
    assert_eq!(
        i.rig.mac.calls_of(Tool::Installer).len(),
        1,
        "2.0 was not installed"
    );
}

/// Catches: an installed file that cannot be read being taken as one that matches its copy, in
/// the proof after `installer` and in the check for drift before it.
#[test]
fn activate_reports_io_error_when_an_installed_file_cannot_be_read() {
    let i = installed("act-host-prove");
    let (dir2, order2) = i.stage_v2();
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--pkg-info-plist"],
        1,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(
        err.detail().starts_with(&home("VERSION")),
        "{}",
        err.detail()
    );
    assert_eq!(
        i.rig.mac.calls_of(Tool::Installer).len(),
        2,
        "2.0 was installed"
    );

    let i = installed("act-host-drift");
    let (dir2, order2) = i.stage_v2();
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--pkg-info-plist"],
        0,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert_eq!(
        i.rig.mac.calls_of(Tool::Installer).len(),
        1,
        "2.0 was not installed"
    );
}

/// Catches: a host that cannot be read after `installer` being taken as one where nothing of
/// the previous version is left to delete.
#[test]
fn activate_reports_io_error_when_the_files_to_delete_cannot_be_looked_at() {
    let i = installed("act-host-delete");
    i.rig.mac.add(v2_bare());
    let order2 = i.rig.order_run("2.0", &[]);
    let dir2 = i.rig.stage(&order2, "svc-2.0");
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--pkg-info-plist"],
        1,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(
        err.detail().starts_with(&home("VERSION")),
        "{}",
        err.detail()
    );
    assert!(
        i.rig.mac.calls_of(Tool::Launchctl).len() == 2,
        "only 1.0's enables ran"
    );
}

/// Catches: a file of the previous version that cannot be deleted, or a recorded directory that
/// cannot be, being reported as a success.
#[test]
fn activate_reports_io_error_when_a_directory_cannot_be_written() {
    let i = installed("act-denied-file");
    lock_dir(&i.rig, &home("old"));
    let (dir2, order2) = i.stage_v2();
    let result = i.rig.activate(&order2, &dir2, Some(&i.dir1));
    match permissions_deny(&i.rig) {
        true => assert!(
            result
                .unwrap_err()
                .detail()
                .starts_with(&home("old/legacy"))
        ),
        false => assert!(!i.rig.exists(&home("old/legacy"))),
    }
    unlock_dir(&i.rig, &home("old"));

    let rig = Rig::new("act-denied-dir");
    rig.mac.add(v1_with_groups());
    let order1 = rig.order("1.0");
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    rig.mac.add(v2());
    let order2 = rig.order("2.0");
    let dir2 = rig.stage(&order2, "svc-2.0");
    lock_dir(&rig, &home("grp"));
    let result = rig.activate(&order2, &dir2, Some(&dir1));
    match permissions_deny(&rig) {
        true => assert!(result.unwrap_err().detail().starts_with(&home("grp/inner"))),
        false => assert!(!rig.exists(&home("grp"))),
    }
    unlock_dir(&rig, &home("grp"));
}

/// Catches: a failing installer lock, a host that cannot be read and a directory no one can
/// write ending `remove` as a success.
#[test]
fn remove_reports_io_error_at_each_failure_of_the_host() {
    let mut i = installed("rm-host-lock");
    i.rig.lock = hostkeeper_install::lock::InstallerLock::new(&i.rig.root.join("missing/dir/lock"));
    assert_eq!(
        reason(i.rig.pkg().remove(&i.order1, &[&i.dir1])),
        Reason::IoError
    );

    // A file the receipt lists, looked at.
    let i = installed("rm-host-meta");
    i.rig
        .mac
        .fault_nth(Tool::Pkgutil, &["--only-files"], 0, 1, swap_root(&i.rig));
    let err = i.rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(
        err.detail().starts_with(&home("VERSION")),
        "{}",
        err.detail()
    );

    // The deletion of a file after its owners were asked for.
    let i = installed("rm-host-delete");
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--file-info-plist"],
        0,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(
        err.detail().starts_with(&home("VERSION")),
        "{}",
        err.detail()
    );

    // Every file shared with another receipt, so nothing is deleted, and then a directory looked at.
    let i = installed("rm-host-dirs");
    let shared: Vec<String> = v1().receipt_files();
    let shared: Vec<&str> = shared.iter().map(String::as_str).collect();
    i.rig.mac.set_receipt("com.other.pkg", "1", &shared);
    i.rig.mac.fault_nth(
        Tool::Pkgutil,
        &["--file-info-plist"],
        shared.len() - 1,
        1,
        swap_root(&i.rig),
    );
    let err = i.rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
    assert!(
        err.detail().starts_with("/Library/LaunchDaemons: "),
        "{}",
        err.detail()
    );

    // A recorded directory no one can write.
    let rig = Rig::new("rm-denied-dir");
    rig.mac.add(v1_with_groups());
    let order = rig.order("1.0");
    let dir = rig.stage(&order, "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    lock_dir(&rig, &home("grp"));
    let result = rig.pkg().remove(&order, &[&dir]);
    match permissions_deny(&rig) {
        true => assert!(result.unwrap_err().detail().starts_with(&home("grp/inner"))),
        false => result.unwrap(),
    }
    unlock_dir(&rig, &home("grp"));
}

/// Catches: a version directory that cannot be looked at being retired without a word.
#[test]
fn retire_reports_io_error_for_a_directory_below_a_file() {
    let rig = Rig::new("retire-io");
    let file = rig.packages.join("not-a-directory");
    fs::write(&file, b"x").unwrap();
    assert_eq!(reason(rig.pkg().retire(&file.join("1.0"))), Reason::IoError);
}

/// Catches: an observer that takes a payload it cannot look at for one that is there.
#[test]
fn the_observer_reports_an_error_for_a_payload_file_it_cannot_look_at() {
    let i = installed("drift-host-io");
    fs::remove_file(i.rig.real(&home("old/legacy"))).unwrap();
    fs::remove_dir(i.rig.real(&home("old"))).unwrap();
    i.rig
        .write(&home("old"), b"a file where a directory was", 0o644);
    let record = i.rig.records.get(SERVICE);
    let result = observe(
        &i.rig.host,
        IDENTIFIER,
        record.as_ref(),
        Some(&i.dir1),
        &mut Seen::default(),
    );
    assert!(result.is_err());
}

/// Catches: a listed file that became a directory being deleted as a file, or its contents.
#[test]
fn remove_leaves_a_directory_where_a_listed_file_was() {
    let i = installed("rm-became-dir");
    fs::remove_file(i.rig.real(&home("svc"))).unwrap();
    fs::create_dir(i.rig.real(&home("svc"))).unwrap();
    i.rig.write(&home("svc/inside"), b"mine", 0o644);
    i.rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    assert_eq!(i.rig.read_text(&home("svc/inside")), "mine");
}
