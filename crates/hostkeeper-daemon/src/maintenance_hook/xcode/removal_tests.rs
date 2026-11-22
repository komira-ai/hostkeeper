//! The tests of the removal of the old Xcodes by the apply of the `xcode` kind: which
//! directories it takes and which it leaves, and what a removal that fails or was cut off leaves.
//! The fakes are those of the apply's tests.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::sync::Arc;

use hostkeeper_launchd::LaunchctlOutput;

use super::*;

/// Proves: the removal takes exactly the `Xcode-<version>.app` directories of the versions the
/// order names whose own version list names that version; a directory named for a version it
/// does not hold, one with no or an unreadable version list, a link, a file, an Xcode the order
/// does not name (older or not), a foreign `Xcode.app` and a version not installed are all left.
///
/// Catches: a removal by a glob over `/Applications`, an Xcode a person put there under another
/// name removed, a link followed into what it names, a version not installed failing the run.
#[test]
fn the_removal_takes_only_the_named_xcodes_of_their_own_version() {
    let mac = Mac::new("xcode-apply-removal");
    let new = entry("27.2", "27B52");
    mac.staged(&new);
    mac.installed("25.4", "25E5");
    mac.installed("25.1", "25A1");
    mac.app("/Applications/Xcode-25.3.app", "25.3.1", "25C9");
    mac.app("/Applications/Xcode.app", "24.1", "24B2");
    fs::create_dir_all(mac.path("/Applications/Xcode-22.app/Contents")).expect("no version list");
    mac.file("/Applications/Xcode-21.app/Contents/version.plist", GARBAGE);
    mac.installed("20.1", "20A1");
    symlink(
        mac.path("/Applications/Xcode-20.1.app"),
        mac.path("/Applications/Xcode-20.app"),
    )
    .expect("a link");
    mac.file("/Applications/Xcode-19.app", b"a file");
    let remove = ["25.4", "25.3", "24.1", "22", "21", "20", "19", "18"];
    let ended = mac
        .apply()
        .cutover(&params(std::slice::from_ref(&new), &remove, "27.2"));
    assert_eq!(ended, applied("27.2"));
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-19.app",
            "/Applications/Xcode-20.1.app",
            "/Applications/Xcode-20.app",
            "/Applications/Xcode-21.app",
            "/Applications/Xcode-22.app",
            "/Applications/Xcode-25.1.app",
            "/Applications/Xcode-25.3.app",
            "/Applications/Xcode-27.2.app",
            "/Applications/Xcode.app",
        ]
    );
    assert_eq!(
        mac.version_list("/Applications/Xcode-20.1.app"),
        plist("20.1", "20A1")
    );
}

/// Proves: an Xcode to remove leaves its path whole, renamed into the staging directory, before
/// it is deleted there: a deletion the volume refuses fails `xcode_remove_failed` naming what is
/// left in staging, with `/Applications/Xcode-<version>.app` already gone; a path that cannot be
/// looked at, or a staging directory others can write, fails it before anything is moved. Each
/// comes after the select.
///
/// Catches: a removal cut off part way that leaves a half-deleted Xcode at its path, which no
/// later run can tell is ours; an Xcode moved into a directory an admin of the Mac controls.
#[test]
fn a_removal_that_fails_names_what_it_left() {
    let removing = format!("{STAGING}/removing-25.4");
    let cases = [
        (
            "contents",
            "/Applications/Xcode-25.4.app/Contents".to_owned(),
            0o500,
            format!("{removing}/Contents"),
            format!("cannot remove {removing}: Permission denied (os error 13)"),
            vec![
                "/Applications/Xcode-26.3.app",
                "/Applications/Xcode-27.2.app",
            ],
        ),
        (
            "applications",
            "/Applications".to_owned(),
            0o000,
            "/Applications".to_owned(),
            "cannot read /Applications/Xcode-25.4.app: Permission denied (os error 13)".to_owned(),
            vec![
                "/Applications/Xcode-25.4.app",
                "/Applications/Xcode-26.3.app",
                "/Applications/Xcode-27.2.app",
            ],
        ),
        (
            "staging",
            STAGING.to_owned(),
            0o755,
            STAGING.to_owned(),
            format!(
                "cannot remove /Applications/Xcode-25.4.app: {STAGING} is not a directory only \
                 this process's user can write"
            ),
            vec![
                "/Applications/Xcode-25.4.app",
                "/Applications/Xcode-26.3.app",
                "/Applications/Xcode-27.2.app",
            ],
        ),
    ];
    for (name, closed, mode, reopen, detail, left) in cases {
        let (mac, xcodes) = usual_mac(&format!("xcode-apply-remove-{name}"));
        let shut = closed.clone();
        mac.answers(
            "/usr/bin/xcode-select -s /Applications/Xcode-27.2.app",
            Arc::new(move |root: &Path| {
                let path = root.join(shut.trim_start_matches('/'));
                fs::set_permissions(path, fs::Permissions::from_mode(mode))
                    .expect("the mode the test sets");
                Ok(LaunchctlOutput::success(""))
            }),
        );
        let ended = mac.apply().cutover(&params(&xcodes, &["25.4"], "27.2"));
        mac.mode(&reopen, 0o755);
        assert_eq!(ended, failure(REASON_REMOVE_FAILED, &detail), "{name}");
        let mut names = vec!["/Applications/.hostkeeper-staging"];
        names.extend(left);
        assert_eq!(mac.apps(), names, "{name}");
    }
}

/// Proves: what an earlier removal cut off left in the staging directory is removed before the
/// next removal of that version, and a staging directory that is not there is made, mode 0700,
/// for the removal: the Mac ends with no `/Applications/Xcode-25.4.app` and an empty staging
/// directory.
///
/// Catches: a leftover of a cut-off removal that blocks the next removal for ever; a removal
/// that fails on a Mac whose stage left no staging directory.
#[test]
fn a_removal_finishes_what_an_earlier_one_left() {
    let mac = Mac::new("xcode-apply-remove-again");
    mac.installed("27.2", "27B52");
    mac.installed("25.4", "25E5");
    let order = params(&[entry("27.2", "27B52")], &["25.4"], "27.2");
    assert_eq!(mac.apply().cutover(&order), applied("27.2"));
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-27.2.app"
        ]
    );
    let mode = fs::metadata(mac.path(STAGING))
        .expect("the staging directory")
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    mac.installed("25.4", "25E5");
    mac.file(
        &format!("{STAGING}/removing-25.4/Contents/left"),
        b"cut off",
    );
    assert_eq!(mac.apply().cutover(&order), applied("27.2"));
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-27.2.app"
        ]
    );
    assert_eq!(mac.names(STAGING), Vec::<String>::new());
}
