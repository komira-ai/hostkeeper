//! A file an operator placed by hand at a payload path before the package's first install
//! (packages-lifecycle.md section 6.5, "A file nobody owns is kept at a first install"), as root
//! against the host's real dpkg and systemd, through the daemon's own order path: it survives a
//! failed first install and an install followed by the uninstall with its bytes, mode, owner and
//! group; its copy is root's alone while it is kept; a file another package owns is not kept,
//! and dpkg refuses the install as it did before.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::Path;
use std::process::Command;

use hostkeeper_testkit::Scratch;

use super::daemon::{self, HOOK_COMMAND};
use super::other_user;
use super::repo::run;
use super::rig::{Rig, Trust, dpkg_version, print};

/// A file every fixture package installs, and not as a conffile: the purge deletes it whatever
/// its content.
const FILE: &str = "/usr/share/hkaptfix/version";
/// What the operator wrote there.
const HAND: &str = "placed by hand\n";
/// While it exists the fixture hook fails `verify`.
const FAIL_VERIFY: &str = "/run/hkaptfix-hook.fail-verify";
/// The package of [`Other`], which owns [`FILE`].
const OTHER: &str = "hkaptfix-other";

/// The operator's file at [`FILE`]: the runner's own user's, mode 0640, in a directory made for
/// it; taken away with that directory when the test ends, however it ends.
struct Hand;

impl Hand {
    fn place() -> Hand {
        let dir = Path::new(FILE).parent().expect("a parent");
        fs::create_dir_all(dir).expect("the file's directory");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o750)).expect("its mode");
        fs::write(FILE, HAND).expect("the operator's file");
        let (uid, gid) = other_user::other_user();
        chown(FILE, Some(uid), Some(gid)).expect("its owner");
        fs::set_permissions(FILE, fs::Permissions::from_mode(0o640)).expect("its mode");
        Hand
    }
}

impl Drop for Hand {
    fn drop(&mut self) {
        let _ = fs::remove_file(FILE);
        let _ = fs::remove_dir(Path::new(FILE).parent().expect("a parent"));
        let _ = fs::remove_file(FAIL_VERIFY);
    }
}

/// The bytes, mode, owner and group at [`FILE`], and the parent's mode.
fn seen() -> (String, u32, u32, u32, u32) {
    let meta = fs::symlink_metadata(FILE).expect("a file at the path");
    let dir = fs::symlink_metadata(Path::new(FILE).parent().expect("a parent")).expect("its dir");
    (
        fs::read_to_string(FILE).expect("its bytes"),
        meta.mode() & 0o7777,
        meta.uid(),
        meta.gid(),
        dir.mode() & 0o7777,
    )
}

/// Whether dpkg says a package owns `path`.
fn owned(path: &str) -> bool {
    print("dpkg-query", &["--search", path]).code == Some(0)
}

/// The verdict and reason an order finished with.
fn finished(reply: hostkeeper_daemon::Reply) -> (hostkeeper_hook::Verdict, Option<String>) {
    match reply {
        hostkeeper_daemon::Reply::Finished {
            verdict, reason, ..
        } => (verdict, reason),
        other => panic!("the order did not finish: {other:?}"),
    }
}

/// Proves (a) on the host's real dpkg: a hooked first install whose `verify` fails is rolled
/// back (the hook's `rollback`, then `dpkg --purge`), and the operator's file is back with its
/// bytes, mode, owner and group, owned by no package, and its copy dropped.
///
/// Catches: the purge of a failed first install deleting a file the operator placed by hand.
#[test]
fn a_hand_placed_file_survives_a_failed_first_install_on_dpkg() {
    let rig = Rig::new("apt-kept-failed-first", Trust::Entry);
    let _hand = Hand::place();
    let before = seen();
    assert!(!owned(FILE), "no package owns the operator's file");
    let (daemon, _, scratch) = daemon::daemon(&rig);
    fs::write(FAIL_VERIFY, "").expect("make verify fail");
    let target = daemon::entry_running("0.7-hook", &rig.pin("0.7-hook"), &HOOK_COMMAND);
    let order = daemon::install_of(target, "0.7-hook", &rig.pin("0.7-hook"), 1);
    let (verdict, _) = finished(daemon.order(&order));
    assert!(
        matches!(verdict, hostkeeper_hook::Verdict::Failed { .. }),
        "the first install fails: {verdict:?}\n{}",
        daemon::explain(&scratch)
    );
    assert_eq!(dpkg_version(), None, "the package is purged");
    assert_eq!(seen(), before);
    assert!(!owned(FILE));
    assert!(!scratch.path().join("kept").join("hkaptfix").exists());
}

/// Proves (b) and (e) on the host's real dpkg: an install replaces the operator's file with the
/// package's and keeps a copy no other user can read (the set 0700 and root's); the uninstall
/// purges the package and puts the file back with its bytes, mode, owner and group.
#[test]
fn a_hand_placed_file_survives_an_install_and_the_uninstall_on_dpkg() {
    let rig = Rig::new("apt-kept-uninstall", Trust::Entry);
    let _hand = Hand::place();
    let before = seen();
    let (daemon, _, scratch) = daemon::daemon(&rig);
    let one = rig.pin("1.0-1");
    let (verdict, reason) = finished(daemon.order(&daemon::install("1.0-1", &one, 1)));
    assert_eq!(
        (verdict, reason),
        (
            hostkeeper_hook::Verdict::Succeeded {
                promotion_pending: false
            },
            None
        ),
        "{}",
        daemon::explain(&scratch)
    );
    assert_ne!(seen().0, HAND, "the package's file replaced it");
    assert!(owned(FILE));
    let base = scratch.path();
    let set = base.join("kept").join("hkaptfix");
    let meta = fs::symlink_metadata(&set).expect("the set");
    assert_eq!((meta.uid(), meta.mode() & 0o7777), (0, 0o700));
    let tried = other_user::assert_root_only(base, &[&base.join("kept")]);
    assert!(tried > 0, "the copy is kept");

    let (verdict, _) = finished(daemon.order(&daemon::uninstall(2)));
    assert_eq!(
        verdict,
        hostkeeper_hook::Verdict::Succeeded {
            promotion_pending: false
        },
        "{}",
        daemon::explain(&scratch)
    );
    assert_eq!(dpkg_version(), None);
    assert_eq!(seen(), before);
    assert!(!set.exists());
}

/// A package other than the fixture that owns [`FILE`], installed with `dpkg -i`, purged when
/// the test ends, however it ends.
struct Other;

impl Other {
    fn install(dir: &Path) -> Other {
        let tree = dir.join("other");
        fs::create_dir_all(tree.join("DEBIAN")).expect("its control directory");
        let control = format!(
            "Package: {OTHER}\nVersion: 1.0\nArchitecture: all\nMaintainer: hostkeeper tests \
             <tests@example.invalid>\nDescription: a package that owns the fixture's file\n"
        );
        fs::write(tree.join("DEBIAN/control"), control).expect("its control file");
        let file = tree.join(FILE.trim_start_matches('/'));
        fs::create_dir_all(file.parent().expect("a parent")).expect("its directory");
        fs::write(&file, "the other package's\n").expect("its file");
        let deb = dir.join("other.deb");
        run(Command::new("dpkg-deb")
            .args(["--root-owner-group", "--build"])
            .arg(&tree)
            .arg(&deb));
        run(Command::new("dpkg").arg("-i").arg(&deb));
        Other
    }
}

impl Drop for Other {
    fn drop(&mut self) {
        print("dpkg", &["--purge", OTHER]);
    }
}

/// Proves (c) on the host's real dpkg: a file another installed package owns is not kept, and
/// dpkg refuses the first install as it did before (`dpkg_error`): the other package's file is
/// as it was, the fixture is not installed, and nothing is kept.
#[test]
fn a_file_another_package_owns_is_not_kept_and_dpkg_refuses_the_first_install() {
    let rig = Rig::new("apt-kept-owned", Trust::Entry);
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "apt-kept-other")
        .expect("a scratch directory");
    let _other = Other::install(scratch.path());
    assert!(owned(FILE));
    let before = fs::read(FILE).expect("the other package's file");
    let (daemon, _, dirs) = daemon::daemon(&rig);
    let one = rig.pin("1.0-1");
    let (verdict, reason) = finished(daemon.order(&daemon::install("1.0-1", &one, 1)));
    assert!(
        matches!(verdict, hostkeeper_hook::Verdict::Failed { .. }),
        "dpkg refuses the install: {verdict:?}\n{}",
        daemon::explain(&dirs)
    );
    assert_eq!(reason.as_deref(), Some("dpkg_error"));
    assert_eq!(dpkg_version(), None);
    assert_eq!(fs::read(FILE).expect("still there"), before);
    assert!(!dirs.path().join("kept").exists(), "nothing was kept");
}
