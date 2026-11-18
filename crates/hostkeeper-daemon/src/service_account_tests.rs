//! The tests of `service_account`: a passwd file in a scratch directory, and `dscl` as a scripted
//! program. No real passwd file or directory service is read.

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn passwd(scratch: &Scratch, text: &str) -> PasswdAccounts {
    let path = scratch.path().join("passwd");
    fs::write(&path, text).expect("the passwd file");
    PasswdAccounts::new(path)
}

const FILE: &str = "root:x:0:0:root:/root:/bin/bash\n\
                    jsagent:x:998:997:js agent:/var/lib/js-agent:/usr/sbin/nologin\n\
                    short:x:1\n\
                    wordy:x:one:two:::\n";

/// Catches an account's ids read from another line, or from a field that is not its own: the
/// uid and gid of the line named, whatever lines come before it, root's included.
#[test]
fn a_linux_account_is_the_uid_and_gid_of_its_passwd_line() {
    let scratch = scratch("sa-passwd");
    let accounts = passwd(&scratch, FILE);
    assert_eq!(accounts.lookup("root"), Ok(Owner { uid: 0, gid: 0 }));
    assert_eq!(accounts.lookup("jsagent"), Ok(Owner { uid: 998, gid: 997 }));
}

/// Catches an unknown account, a damaged line, an unreadable file and a name that is not an
/// account name being given files: each is its reason, word for word.
#[test]
fn a_linux_account_the_host_does_not_have_is_refused_with_its_reason() {
    let scratch = scratch("sa-passwd-refused");
    let accounts = passwd(&scratch, FILE);
    assert_eq!(
        accounts.lookup("nobody"),
        Err("the host has no account named \"nobody\"".to_owned())
    );
    let damaged = "is not name:password:uid:gid:gecos:home:shell";
    for name in ["short", "wordy"] {
        assert_eq!(
            accounts.lookup(name),
            Err(format!("the passwd line of {name:?} {damaged}"))
        );
    }
    for name in ["", "-x", "a b", "a/b", &"a".repeat(256)] {
        assert_eq!(
            accounts.lookup(name),
            Err(format!("{name:?} is not an account name"))
        );
    }
    let missing = PasswdAccounts::new(scratch.path().join("absent"));
    assert_eq!(
        missing.lookup("root"),
        Err(format!(
            "cannot read the passwd file {}: No such file or directory (os error 2)",
            scratch.path().join("absent").display()
        ))
    );
}

fn read(name: &str) -> Vec<String> {
    [
        ".",
        "-read",
        &format!("{}/{name}", "/Users"),
        "UniqueID",
        "PrimaryGroupID",
    ]
    .iter()
    .map(|word| (*word).to_owned())
    .collect()
}

/// Catches the Mac's lookup asking `dscl` for something else or reading the wrong keys: it reads
/// the user record of the name with exactly those two keys, and the account is their values.
#[test]
fn a_mac_account_is_read_from_its_dscl_record() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &read("jsagent"),
        Ok(LaunchctlOutput::success(
            "UniqueID: 502\nPrimaryGroupID: 20\n",
        )),
    );
    let accounts = DsclAccounts::new(Box::new(fake.clone()));
    assert_eq!(accounts.lookup("jsagent"), Ok(Owner { uid: 502, gid: 20 }));
    assert_eq!(fake.calls(), vec![read("jsagent")]);
}

/// Catches a Mac account that does not exist, or whose record lacks a number, being given
/// files: each is its reason, and a name that is not an account name never reaches `dscl`.
#[test]
fn a_mac_account_the_host_does_not_have_is_refused_with_its_reason() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &read("ghost"),
        Ok(LaunchctlOutput::exit(
            56,
            "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)",
        )),
    );
    fake.always(
        &read("half"),
        Ok(LaunchctlOutput::success(
            "UniqueID: 502\nNo such key: PrimaryGroupID\n",
        )),
    );
    fake.always(
        &read("words"),
        Ok(LaunchctlOutput::success(
            "UniqueID: five\nPrimaryGroupID: 20\n",
        )),
    );
    let accounts = DsclAccounts::new(Box::new(fake.clone()));
    assert_eq!(
        accounts.lookup("ghost"),
        Err(
            "the account \"ghost\" cannot be read: dscl read of a service account failed with exit code 56"
                .to_owned()
        )
    );
    for name in ["half", "words"] {
        assert_eq!(
            accounts.lookup(name),
            Err(format!(
                "dscl did not give a user id and a primary group for the account {name:?}"
            ))
        );
    }
    assert_eq!(
        accounts.lookup("a b"),
        Err("\"a b\" is not an account name".to_owned())
    );
    assert_eq!(fake.calls().len(), 3);
}

/// Catches the host's choice of source: a Linux or OpenRC host reads the passwd file and a Mac
/// the directory service. Neither is read here: a Linux host's lookup of a name no passwd file
/// has is the one that reads `/etc/passwd`, and is refused for the name alone when it is not one.
#[test]
fn the_host_reads_the_source_of_its_platform() {
    for kind in [
        PlatformKind::Linux,
        PlatformKind::Openrc,
        PlatformKind::Macos,
    ] {
        let accounts = at_host(kind);
        assert_eq!(
            accounts.lookup("not an account"),
            Err("\"not an account\" is not an account name".to_owned())
        );
    }
}
