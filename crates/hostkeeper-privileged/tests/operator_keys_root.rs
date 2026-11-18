#![cfg(feature = "privileged")]
//! The operator keys' file as root (`crates/hostkeeper-daemon/src/authorized_keys.rs`, design
//! `docs/design/operator-keys.md` section 4): the daemon writes the managed block of an account
//! that is not root's, so `.ssh` and `authorized_keys` must end owned by that account's uid and
//! group with the modes sshd's `StrictModes` takes (0700 and 0600), which only a root process can
//! give a file of another account. The ordinary tests do the same as the test's own user, where
//! the change of owner is a no-op. Also the Linux account rule against the host's own passwd file.
//!
//! The account is `nobody`'s ids (65534) with a home in the test's scratch directory: no real
//! account's home is touched, and the ids need no passwd entry for a file to be owned by them.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use hostkeeper_daemon::authorized_keys::{BEGIN, END, KeysFileError, Put, put};
use hostkeeper_daemon::login_account::{Account, AccountSource, LinuxRoot};
use hostkeeper_testkit::Scratch;

/// The ids the block is written for: `nobody` and `nogroup` on Debian and Ubuntu.
const NOBODY: u32 = 65534;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// Refuses to run as anything but root: only root gives a file to another account.
fn as_root() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "the privileged tests run as root (privileged.yml's cargo runner `sudo`)"
    );
}

/// The account of `nobody`'s ids whose home is `home`.
fn nobody(home: &Path) -> Account {
    Account {
        name: "nobody".to_owned(),
        uid: NOBODY,
        gid: NOBODY,
        home: home.to_path_buf(),
    }
}

/// The owner, group and permission bits of `path`.
fn owned(path: &Path) -> (u32, u32, u32) {
    let meta = fs::symlink_metadata(path).expect("stat");
    (meta.uid(), meta.gid(), meta.mode() & 0o7777)
}

/// Proves that as root the block is written into a new `.ssh` of another account, both owned by
/// that account's uid and group, `.ssh` mode 0700 and the file 0600; that a key added by hand
/// outside the block stays when a key is removed from the set; and that the empty set leaves the
/// hand key alone, still owned by the account. Catches: a file left owned by root, which sshd's
/// `StrictModes` refuses for another account's login.
#[test]
fn the_block_of_another_account_is_owned_by_it_with_the_modes_sshd_takes() {
    as_root();
    let dir = scratch("operator-keys-owner");
    let home = dir.path().join("home");
    fs::create_dir(&home).expect("home");
    let account = nobody(&home);
    let keys = vec![
        "ssh-ed25519 AAAA ops laptop".to_owned(),
        "ssh-rsa BBBB".to_owned(),
    ];

    assert_eq!(put(&account, &keys).expect("written"), Put::Written);
    let ssh = home.join(".ssh");
    let file = ssh.join("authorized_keys");
    assert_eq!(owned(&ssh), (NOBODY, NOBODY, 0o700));
    assert_eq!(owned(&file), (NOBODY, NOBODY, 0o600));
    assert_eq!(
        fs::read_to_string(&file).expect("read"),
        format!("{BEGIN}\n{}\n{}\n{END}\n", keys[0], keys[1])
    );
    assert_eq!(put(&account, &keys).expect("unchanged"), Put::Unchanged);

    let hand = "ssh-ed25519 CCCC added by hand";
    let text = fs::read_to_string(&file).expect("read");
    fs::write(&file, format!("{hand}\n{text}")).expect("a hand key");
    assert_eq!(put(&account, &keys[1..]).expect("written"), Put::Written);
    assert_eq!(
        fs::read_to_string(&file).expect("read"),
        format!("{hand}\n{BEGIN}\n{}\n{END}\n", keys[1])
    );
    assert_eq!(owned(&file), (NOBODY, NOBODY, 0o600));

    assert_eq!(put(&account, &[]).expect("written"), Put::Written);
    assert_eq!(
        fs::read_to_string(&file).expect("read"),
        format!("{hand}\n")
    );
    assert_eq!(owned(&file), (NOBODY, NOBODY, 0o600));
    let left: Vec<_> = fs::read_dir(&ssh)
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(left, ["authorized_keys"], "no temporary file is left");
}

/// Proves that as root a `.ssh` that root owns is not taken over for another account: the put is
/// refused and nothing is written into it. Catches: a daemon that, running as root, writes keys
/// into a directory the account does not own (and so would log in through a key it cannot see
/// or change).
#[test]
fn a_ssh_directory_root_owns_is_not_taken_for_another_account() {
    as_root();
    let dir = scratch("operator-keys-other-owner");
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(".ssh")).expect(".ssh");
    fs::set_permissions(home.join(".ssh"), fs::Permissions::from_mode(0o700)).expect("mode");
    let refused = put(&nobody(&home), &["ssh-ed25519 AAAA".to_owned()]);
    assert!(
        matches!(refused, Err(KeysFileError::NotOwned { .. })),
        "{refused:?}"
    );
    assert_eq!(
        fs::read_dir(home.join(".ssh")).expect("list").count(),
        0,
        "nothing is written"
    );
}

/// Proves the Linux account rule reads the host's own `/etc/passwd`: `root`, uid and gid 0, and a
/// home that is a directory. Catches: a parse of the real file (its field order, a trailing
/// newline) that the scripted files of the ordinary tests do not have.
#[test]
fn the_linux_login_account_is_root_from_the_hosts_passwd_file() {
    as_root();
    let account = LinuxRoot::host().login_account().expect("root's line");
    assert_eq!(
        (account.name.as_str(), account.uid, account.gid),
        ("root", 0, 0)
    );
    assert!(account.home.is_absolute(), "{}", account.home.display());
    assert!(account.home.is_dir(), "{}", account.home.display());
}
