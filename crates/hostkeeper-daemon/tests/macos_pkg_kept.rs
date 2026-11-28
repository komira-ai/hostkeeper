//! A `.pkg`'s first install over a file an operator placed by hand at one of its payload paths
//! (packages-lifecycle.md section 6.5, "A file nobody owns is kept at a first install"), on the
//! fake Mac the `macos-pkg` tests use: the daemon keeps the file at the first install, puts it
//! back once its removal (a failed first install's rollback, or an uninstall) has deleted every
//! file the receipt lists, and drops the copy; a file another receipt lists is refused as before;
//! a copy that cannot be put back is an alert naming the path.

mod support;
mod support_macos_pkg;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use hostkeeper_daemon::Reply;
use hostkeeper_hook::{Class, Verdict};
use support::{alerting, lines, uninstall};
use support_macos_pkg::{IDENTIFIER, MacRig, SERVICE, install_of, package};

/// The payload file an operator placed by hand.
const FILE: &str = "/Library/Application Support/hello/VERSION";
/// What the operator wrote there.
const HAND: &[u8] = b"placed by hand\n";

/// A rig with the operator's file at [`FILE`], mode 0640, its parents as a Mac has them.
fn rig(name: &str) -> MacRig {
    let mac = MacRig::new(name);
    let mut dir = mac.root.clone();
    for part in ["Library", "Application Support", "hello"] {
        dir = dir.join(part);
        fs::create_dir(&dir).expect("a parent");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    }
    fs::write(mac.real(FILE), HAND).expect("the operator's file");
    fs::set_permissions(mac.real(FILE), fs::Permissions::from_mode(0o640)).expect("its mode");
    mac
}

/// The bytes, mode, owner and group at [`FILE`].
fn seen(mac: &MacRig) -> (Vec<u8>, u32, u32, u32) {
    let meta = fs::symlink_metadata(mac.real(FILE)).expect("a file at the path");
    (
        fs::read(mac.real(FILE)).expect("its bytes"),
        meta.mode() & 0o7777,
        meta.uid(),
        meta.gid(),
    )
}

/// The kept files' set of the service.
fn set_dir(mac: &MacRig) -> PathBuf {
    mac.rig.scratch.path().join("kept").join(SERVICE)
}

/// Proves (a) on a Mac: a first install that fails after `installer` replaced the file (a
/// payload file that is not the sealed copy) is rolled back by removing every file the receipt
/// lists, and the operator's file is then back with its bytes, mode, owner and group.
#[test]
fn a_hand_placed_file_survives_a_failed_first_install_of_a_pkg() {
    let mac = rig("pkg-kept-failed-first");
    let before = seen(&mac);
    let daemon = mac.daemon();
    let one = mac.publish(
        "1.0.0",
        package("1.0.0").with_install(support_macos_pkg::pkg::Install::OtherBytes {
            path: "/Library/Application Support/hello/hello".to_owned(),
            bytes: b"not the sealed copy".to_vec(),
        }),
    );
    let ended = daemon.order(&install_of("1.0.0", &one, 1));
    assert!(
        matches!(
            &ended,
            Reply::Finished { verdict: Verdict::Failed { class: Class::Release, .. }, .. }
        ),
        "{ended:?}"
    );
    assert_eq!(mac.mac.receipt(IDENTIFIER), None, "the package is removed");
    assert!(!mac.real("/Library/Application Support/hello/hello").exists());
    assert_eq!(seen(&mac), before);
    assert!(!set_dir(&mac).exists(), "the kept copy is dropped");
}

/// Proves (b) and (e) on a Mac: an install replaces the file and keeps it in a set only root
/// can read; the uninstall puts it back and drops the set.
#[test]
fn a_hand_placed_file_survives_the_install_and_uninstall_of_a_pkg() {
    let mac = rig("pkg-kept-uninstall");
    let before = seen(&mac);
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    let succeeded = Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    };
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded);
    assert_eq!(fs::read(mac.real(FILE)).expect("the package's"), b"1.0.0");
    let mode = |path: PathBuf| fs::symlink_metadata(path).expect("there").mode() & 0o7777;
    assert_eq!(mode(set_dir(&mac)), 0o700);
    assert_eq!(mode(set_dir(&mac).join("0")), 0o600);
    assert_eq!(fs::read(set_dir(&mac).join("0")).expect("the copy"), HAND);
    support_macos_pkg::open_up(&mac.rig.packages);

    assert_eq!(daemon.order(&uninstall(SERVICE, 2)), succeeded);
    assert_eq!(mac.mac.receipt(IDENTIFIER), None);
    assert_eq!(seen(&mac), before);
    assert!(!set_dir(&mac).exists());
}

/// Proves (c) on a Mac: a file another receipt lists is not kept, and the first install is
/// refused `package_conflict` as before, with the file untouched.
#[test]
fn a_file_another_receipt_lists_is_not_kept_and_the_pkg_is_refused() {
    let mac = rig("pkg-kept-owned");
    mac.mac.set_receipt(
        "com.example.other",
        "3",
        &["Library/Application Support/hello/VERSION"],
    );
    let before = seen(&mac);
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    let ended = daemon.order(&install_of("1.0.0", &one, 1));
    assert!(
        matches!(&ended, Reply::Finished { reason: Some(reason), .. } if reason == "package_conflict"),
        "{ended:?}"
    );
    assert_eq!(seen(&mac), before);
    assert!(!mac.rig.scratch.path().join("kept").exists());
}

/// Proves a kept copy missing at the uninstall of a `.pkg` leaves the path empty, completes the
/// uninstall, and raises the alert naming the path.
#[test]
fn a_kept_copy_missing_at_the_uninstall_of_a_pkg_raises_an_alert() {
    let mac = rig("pkg-kept-lost");
    let (daemon, log) = alerting(mac.daemon());
    let one = mac.publish("1.0.0", package("1.0.0"));
    let ended = daemon.order(&install_of("1.0.0", &one, 1));
    assert!(matches!(&ended, Reply::Finished { reason: None, .. }), "{ended:?}");
    fs::remove_file(set_dir(&mac).join("0")).expect("the copy goes");
    support_macos_pkg::open_up(&mac.rig.packages);

    let ended = daemon.order(&uninstall(SERVICE, 2));
    assert!(matches!(&ended, Reply::Finished { reason: None, .. }), "{ended:?}");
    assert!(!mac.real(FILE).exists(), "the path stays empty");
    assert!(!set_dir(&mac).exists());
    assert_eq!(
        lost_lines(&log),
        [format!(
            "hostkeeper daemon: level=error alert=kept_file_lost service={SERVICE} path={FILE}: \
             the file kept at its first install was not put back: the kept copy: No such file \
             or directory (os error 2); no copy of it is left"
        )]
    );
}

/// The log's lines of the alert of a kept file not put back.
fn lost_lines(log: &hostkeeper_daemon::testing::Log) -> Vec<String> {
    lines(log)
        .into_iter()
        .filter(|line| line.contains("alert=kept_file_lost"))
        .collect()
}
