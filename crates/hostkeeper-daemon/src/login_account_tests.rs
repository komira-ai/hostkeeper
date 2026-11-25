//! The tests of `login_account`: the passwd file in a scratch directory, and `dscl` as a scripted
//! program. No real passwd file or directory service is read.

use hostkeeper_launchd::FakeLaunchctl;
use hostkeeper_testkit::Scratch;

use super::*;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The sources of a passwd file with `text`.
fn linux(scratch: &Scratch, text: &str) -> LinuxRoot {
    let path = scratch.path().join("passwd");
    fs::write(&path, text).expect("the passwd file");
    LinuxRoot::new(path)
}

const LIST: &[&str] = &[".", "-list", "/Users", "UniqueID"];
const ADMINS: &[&str] = &[".", "-read", "/Groups/admin", "GroupMembership"];

fn user(name: &str) -> Vec<String> {
    words(&[
        ".",
        "-read",
        &format!("{}/{name}", "/Users"),
        "NFSHomeDirectory",
        "PrimaryGroupID",
        "IsHidden",
    ])
}

/// A Mac whose `dscl` lists `users`, has `members` of `admin`, and answers each `(name, record)`.
fn mac(users: &str, members: &str, records: &[(&str, &str)]) -> (MacAdmin, FakeLaunchctl) {
    let fake = FakeLaunchctl::new();
    fake.always(&words(LIST), Ok(LaunchctlOutput::success(users)));
    fake.always(&words(ADMINS), Ok(LaunchctlOutput::success(members)));
    for (name, record) in records {
        fake.always(&user(name), Ok(LaunchctlOutput::success(record)));
    }
    (MacAdmin::new(Box::new(fake.clone())), fake)
}

const ALICE: &str =
    "NFSHomeDirectory: /Volumes/homes/alice\nPrimaryGroupID: 20\nNo such key: IsHidden\n";

/// Proves the root line of a passwd file gives root's group and home, whatever lines come before
/// it, and the host's source reads `/etc/passwd`.
#[test]
fn linux_takes_the_root_line_of_the_passwd_file() {
    let scratch = scratch("la-root");
    let source = linux(
        &scratch,
        "daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\nroot:x:0:0:root:/root:/bin/bash\n",
    );
    assert_eq!(
        source.login_account(),
        Ok(Account {
            name: "root".to_owned(),
            uid: 0,
            gid: 0,
            home: PathBuf::from("/root"),
        })
    );
    assert_eq!(LinuxRoot::host().passwd, PathBuf::from("/etc/passwd"));
}

/// Proves a passwd file with no root line, or whose root line is not a uid 0 account with an
/// absolute home, is refused (the host has no account to take), each with its reason.
#[test]
fn linux_refuses_a_passwd_file_with_no_usable_root_line() {
    let scratch = scratch("la-no-root");
    let missing = "the passwd file has no root account";
    let broken = "the root line of the passwd file is not name:password:uid:gid:gecos:home:shell \
                  with uid 0 and an absolute home";
    let cases = [
        ("daemon:x:1:1::/:/bin/sh\n", missing),
        ("", missing),
        ("root:x:0\n", broken),
        ("root:x:one:0:root:/root:/bin/sh\n", broken),
        ("root:x:0:zero:root:/root:/bin/sh\n", broken),
        ("root:x:1:0:root:/root:/bin/sh\n", broken),
        ("root:x:0:0:root:root:/bin/sh\n", broken),
    ];
    for (text, reason) in cases {
        assert_eq!(
            linux(&scratch, text).login_account(),
            Err(AccountError::Refused(reason.to_owned())),
            "{text:?}"
        );
    }
}

/// Proves a passwd file that cannot be read fails (the next set tries again) and says which file
/// and why.
#[test]
fn linux_fails_when_the_passwd_file_cannot_be_read() {
    let scratch = scratch("la-unreadable");
    let path = scratch.path().join("missing");
    let why = fs::read_to_string(&path).expect_err("no file");
    assert_eq!(
        LinuxRoot::new(path.clone()).login_account(),
        Err(AccountError::Failed(format!(
            "cannot read the passwd file {}: {why}",
            path.display()
        )))
    );
}

/// Proves the one local administrator is found: its name, uid, primary group and home are read,
/// and the three `dscl` reads are the ones asked, in order.
#[test]
fn a_mac_takes_its_one_local_administrator() {
    let (source, fake) = mac(
        "root 0\ndaemon 1\nalice 501\n",
        "GroupMembership: root alice\n",
        &[("alice", ALICE)],
    );
    assert_eq!(
        source.login_account(),
        Ok(Account {
            name: "alice".to_owned(),
            uid: 501,
            gid: 20,
            home: PathBuf::from("/Volumes/homes/alice"),
        })
    );
    assert_eq!(fake.calls(), [words(LIST), words(ADMINS), user("alice")]);
}

/// Proves who is left out: a uid under 501, a name that starts with `_`, a user that is not in
/// `admin`, and a line that is not `name uid`; only the one that passes every rule has its record
/// read.
#[test]
fn a_mac_leaves_out_system_accounts_and_non_members() {
    let (source, fake) = mac(
        "root 0\n_spotlight 600\ncarol 502\ndave 500\nbroken\nerin notanumber\nalice 501\n",
        "GroupMembership: root _spotlight dave erin alice\n",
        &[("alice", ALICE)],
    );
    assert_eq!(source.login_account().expect("alice").name, "alice");
    assert_eq!(fake.calls(), [words(LIST), words(ADMINS), user("alice")]);
}

/// Proves a hidden account is left out, however `dscl` spells hidden (1, YES, true, in any case),
/// and one marked 0 or with no such key is not hidden.
#[test]
fn a_mac_leaves_out_a_hidden_account() {
    for hidden in ["1", "YES", "yes", "true", "True"] {
        let record = format!(
            "NFSHomeDirectory: /Volumes/homes/bob\nPrimaryGroupID: 20\nIsHidden: {hidden}\n"
        );
        let (source, _) = mac(
            "alice 501\nbob 502\n",
            "GroupMembership: alice bob\n",
            &[("alice", ALICE), ("bob", &record)],
        );
        assert_eq!(
            source.login_account().expect("alice").name,
            "alice",
            "{hidden}"
        );
    }
    let record = "NFSHomeDirectory: /Volumes/homes/bob\nPrimaryGroupID: 21\nIsHidden: 0\n";
    let (source, _) = mac("bob 502\n", "GroupMembership: bob\n", &[("bob", record)]);
    assert_eq!(source.login_account().expect("bob").gid, 21);
}

/// Catches a hidden administrator counted as visible on macOS 27, whose `dscl -read` writes the
/// key with its native prefix, as read on a farm Mac (`dsAttrTypeNative:IsHidden: 1`, before the
/// other keys): the Mac's one visible administrator is still the login account, where two would
/// leave the operator keys with none.
#[test]
fn a_mac_leaves_out_an_account_dscl_marks_hidden_with_the_native_prefix() {
    let record =
        "dsAttrTypeNative:IsHidden: 1\nNFSHomeDirectory: /Volumes/homes/bob\nPrimaryGroupID: 20\n";
    let (source, _) = mac(
        "alice 501\nbob 502\n",
        "GroupMembership: alice bob\n",
        &[("alice", ALICE), ("bob", record)],
    );
    assert_eq!(source.login_account().expect("alice").name, "alice");
    assert!(is_hidden("dsAttrTypeNative:IsHidden: 1\nRecordName: bob\n"));
    assert!(!is_hidden(
        "dsAttrTypeNative:IsHidden: 0\nRecordName: bob\n"
    ));
    assert_eq!(
        dscl_value(
            "dsAttrTypeNative:IsHidden:\n 1\nRecordName: bob\n",
            "IsHidden"
        ),
        Some("1".to_owned())
    );
    assert_eq!(dscl_value("dsAttrTypeNative:Other: 1\n", "IsHidden"), None);
}

/// Proves a Mac with no administrator the rule picks is refused, and one with two is refused with
/// their number; neither reason holds a name.
#[test]
fn a_mac_with_no_administrator_or_with_two_is_refused() {
    let (none, _) = mac("root 0\nalice 501\n", "GroupMembership: root\n", &[]);
    assert_eq!(
        none.login_account(),
        Err(AccountError::Refused(
            "no local administrator account: uid 501 or more, in admin, not hidden".to_owned()
        ))
    );
    let bob = "NFSHomeDirectory: /Volumes/homes/bob\nPrimaryGroupID: 20\n";
    let (two, _) = mac(
        "alice 501\nbob 502\n",
        "GroupMembership: alice bob\n",
        &[("alice", ALICE), ("bob", bob)],
    );
    assert_eq!(
        two.login_account(),
        Err(AccountError::Refused(
            "2 local administrator accounts: the managed block takes exactly one".to_owned()
        ))
    );
}

/// Proves the group's members and a record's values are read whether `dscl` writes them after the
/// key, on the lines that follow (each starting with a space), or on the next line; a key it says
/// is absent has no value.
#[test]
fn dscl_values_are_read_with_their_continuation_lines() {
    let (source, fake) = mac(
        "alice 501\n",
        "GroupMembership: root\n alice\n daemon\nNext: line\n",
        &[(
            "alice",
            "NFSHomeDirectory:\n /Volumes/homes/alice\nPrimaryGroupID: 20\nIsHidden:\n 0\n",
        )],
    );
    assert_eq!(
        source.login_account().expect("alice").home,
        PathBuf::from("/Volumes/homes/alice")
    );
    assert_eq!(fake.calls().len(), 3);
    assert_eq!(
        group_members("GroupMembership:\n root alice\n"),
        ["root", "alice"]
    );
    assert!(group_members("No such key: GroupMembership\n").is_empty());
    assert_eq!(
        dscl_value("Key: a\n\tb\nOther: c\n", "Key"),
        Some("a b".to_owned())
    );
    assert_eq!(dscl_value("Other: c\n", "Key"), None);
    assert_eq!(dscl_value("Key:\nOther: c\n", "Key"), Some(String::new()));
}

/// Proves a candidate whose record has no home or no primary group fails (a tool answered
/// something this cannot use), never refuses.
#[test]
fn a_record_with_no_home_or_group_fails() {
    let failed = AccountError::Failed(
        "dscl did not give the home directory and the primary group of an administrator account"
            .to_owned(),
    );
    for record in [
        "PrimaryGroupID: 20\nNo such key: IsHidden\n",
        "NFSHomeDirectory: relative\nPrimaryGroupID: 20\n",
        "NFSHomeDirectory: /Volumes/homes/alice\n",
        "NFSHomeDirectory: /Volumes/homes/alice\nPrimaryGroupID: staff\n",
    ] {
        let (source, _) = mac(
            "alice 501\n",
            "GroupMembership: alice\n",
            &[("alice", record)],
        );
        assert_eq!(source.login_account(), Err(failed.clone()), "{record:?}");
    }
}

/// Proves a `dscl` that exits non-zero, cannot be run, times out or is killed fails with the step
/// and the exit code; the steps that name no account add the first line of the standard error,
/// and the read of a user record, whose arguments name the account, never does.
///
/// Catches: a reason that carried the tool's standard error for an account's record, which names
/// the account, to the coordinator.
#[test]
fn a_dscl_that_fails_fails_the_account_with_its_exit_code() {
    let (source, fake) = mac(
        "alice 501\n",
        "GroupMembership: alice\n",
        &[("alice", ALICE)],
    );
    fake.always(
        &words(LIST),
        Ok(LaunchctlOutput::exit(
            70,
            "dscl_cmd: no list\nsecond line\n",
        )),
    );
    assert_eq!(
        source.login_account(),
        Err(AccountError::Failed(
            "dscl list of users failed with exit code 70: dscl_cmd: no list".to_owned()
        ))
    );

    let (source, fake) = mac(
        "alice 501\n",
        "GroupMembership: alice\n",
        &[("alice", ALICE)],
    );
    fake.always(&words(ADMINS), Ok(LaunchctlOutput::exit(56, "")));
    assert_eq!(
        source.login_account(),
        Err(AccountError::Failed(
            "dscl read of the admin group failed with exit code 56".to_owned()
        ))
    );

    let (source, fake) = mac(
        "alice 501\n",
        "GroupMembership: alice\n",
        &[("alice", ALICE)],
    );
    fake.always(
        &user("alice"),
        Ok(LaunchctlOutput::exit(56, "record alice is gone\n")),
    );
    let Err(AccountError::Failed(reason)) = source.login_account() else {
        panic!("a failure");
    };
    assert_eq!(
        reason,
        "dscl read of a user record failed with exit code 56"
    );

    let (source, fake) = mac(
        "alice 501\n",
        "GroupMembership: alice\n",
        &[("alice", ALICE)],
    );
    fake.always(
        &user("alice"),
        Err(LaunchctlError::Spawn {
            args: concat!("-read /Users", "/alice").to_owned(),
            why: "no such program".to_owned(),
        }),
    );
    assert_eq!(
        source.login_account(),
        Err(AccountError::Failed(
            "dscl read of a user record could not be run: no such program".to_owned()
        ))
    );

    fake.always(
        &user("alice"),
        Err(LaunchctlError::TimedOut {
            args: concat!("-read /Users", "/alice").to_owned(),
        }),
    );
    assert_eq!(
        source.login_account(),
        Err(AccountError::Failed(
            "dscl read of a user record did not finish within its deadline".to_owned()
        ))
    );

    fake.always(
        &user("alice"),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    );
    assert_eq!(
        source.login_account(),
        Err(AccountError::Failed(
            "dscl read of a user record was ended by a signal".to_owned()
        ))
    );
}

/// Proves the two kinds of error are told apart and read as their reason.
#[test]
fn an_account_error_reads_as_its_reason() {
    assert_eq!(AccountError::Refused("no".to_owned()).to_string(), "no");
    assert_eq!(
        AccountError::Failed("later".to_owned()).to_string(),
        "later"
    );
    assert_ne!(
        AccountError::Refused("x".to_owned()),
        AccountError::Failed("x".to_owned())
    );
}
