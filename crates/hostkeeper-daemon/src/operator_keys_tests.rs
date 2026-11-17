//! The tests of `operator_keys`: the sink over real files in a scratch home owned by the test's own
//! user, a scripted account source and scripted Mac programs; the report, the reasons and the log
//! lines are asserted by equality.

use std::fs;
use std::path::PathBuf;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_testkit::Scratch;

use super::*;
use crate::authorized_keys::{BEGIN, END};
use crate::key_fixture_tests::key;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// An account source that answers what it was last told; clones share the answer.
#[derive(Clone)]
struct Scripted(Arc<Mutex<Result<Account, AccountError>>>);

impl AccountSource for Scripted {
    fn login_account(&self) -> Result<Account, AccountError> {
        self.0.lock().expect("the answer").clone()
    }
}

/// A sink over a scratch home, and what it is made of.
struct Rig {
    sink: OperatorKeys,
    account: Account,
    accounts: Scripted,
    log: Arc<RecordingLog>,
    _scratch: Scratch,
}

impl Rig {
    fn file(&self) -> PathBuf {
        self.account.home.join(".ssh/authorized_keys")
    }

    fn answer(&self, answer: Result<Account, AccountError>) {
        *self.accounts.0.lock().expect("the answer") = answer;
    }

    fn lines(&self) -> Vec<String> {
        self.log.lines()
    }
}

fn rig_with(mac: Option<MacSsh>, name: &str) -> Rig {
    let scratch = scratch(name);
    let home = scratch.path().join("home");
    fs::create_dir(&home).expect("the home");
    let account = Account {
        name: "ops".to_owned(),
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
        home,
    };
    let log = Arc::new(RecordingLog::default());
    let accounts = Scripted(Arc::new(Mutex::new(Ok(account.clone()))));
    let sink = OperatorKeys::new(Box::new(accounts.clone()), mac, log.clone());
    Rig {
        sink,
        account,
        accounts,
        log,
        _scratch: scratch,
    }
}

fn rig(name: &str) -> Rig {
    rig_with(None, name)
}

fn push(keys: Vec<String>) -> PushedKeys {
    PushedKeys {
        digest: rules::digest(&keys),
        keys,
    }
}

fn keys(fills: &[u8]) -> Vec<String> {
    fills.iter().map(|fill| key(*fill, "ops laptop")).collect()
}

fn short(set: &PushedKeys) -> &str {
    &set.digest[..12]
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// Proves a set is written into the login account's file and reported by its digest, the log says
/// how many keys are in force, and the same set sent again is "already in force" and writes
/// nothing; a set of one key says "key".
#[test]
fn a_set_is_put_in_force_and_reported_by_its_digest() {
    let rig = rig("ok-force");
    assert_eq!(rig.sink.report(), KeysReport::default());
    let set = push(keys(&[1, 2]));
    rig.sink.apply(&set);
    let text = fs::read_to_string(rig.file()).expect("the file");
    assert_eq!(
        text,
        format!("{BEGIN}\n{}\n{}\n{END}\n", set.keys[0], set.keys[1])
    );
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: set.digest.clone(),
            refused: None,
            failed: String::new(),
        }
    );
    rig.sink.apply(&set);
    let one = push(keys(&[3]));
    rig.sink.apply(&one);
    assert_eq!(
        rig.lines(),
        [
            format!(
                "operator keys {}: 2 keys in force in the login account's authorized_keys",
                short(&set)
            ),
            format!(
                "operator keys {}: 2 keys already in force in the login account's authorized_keys",
                short(&set)
            ),
            format!(
                "operator keys {}: 1 key in force in the login account's authorized_keys",
                short(&one)
            ),
        ]
    );
    assert_eq!(rig.sink.report().digest, one.digest);
}

/// Proves the empty set is in force with no file (nothing is made, the report holds the digest of
/// no keys), and after a set it takes the block away.
#[test]
fn the_empty_set_is_in_force_with_no_file() {
    let rig = rig("ok-empty");
    let none = push(Vec::new());
    rig.sink.apply(&none);
    assert!(!rig.file().exists());
    assert_eq!(rig.sink.report().digest, none.digest);
    assert_eq!(
        rig.lines(),
        [format!(
            "operator keys {}: 0 keys already in force in the login account's authorized_keys",
            short(&none)
        )]
    );
    let set = push(keys(&[1]));
    rig.sink.apply(&set);
    rig.sink.apply(&none);
    assert_eq!(fs::read_to_string(rig.file()).expect("the file"), "");
    assert_eq!(rig.sink.report().digest, none.digest);
}

/// Proves a block edited by hand makes the next report's digest empty, the coordinator's next
/// send of the set restores it, and so does a block taken away or a file whose mode was opened.
#[test]
fn a_file_changed_by_hand_reports_no_digest_until_the_set_is_put_back() {
    let rig = rig("ok-hand");
    let set = push(keys(&[1, 2]));
    rig.sink.apply(&set);
    let good = fs::read_to_string(rig.file()).expect("the file");

    fs::write(
        rig.file(),
        good.replace(&set.keys[0], &key(9, "ops laptop")),
    )
    .expect("edit");
    assert_eq!(rig.sink.report().digest, "");
    rig.sink.apply(&set);
    assert_eq!(fs::read_to_string(rig.file()).expect("the file"), good);
    assert_eq!(rig.sink.report().digest, set.digest);

    fs::write(rig.file(), "ssh-rsa HAND\n").expect("block removed");
    assert_eq!(rig.sink.report().digest, "");
    rig.sink.apply(&set);
    assert!(
        fs::read_to_string(rig.file())
            .expect("the file")
            .contains(BEGIN)
    );
    assert_eq!(rig.sink.report().digest, set.digest);

    let mut mode = fs::metadata(rig.file()).expect("meta").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o644);
    fs::set_permissions(rig.file(), mode).expect("mode");
    assert_eq!(rig.sink.report().digest, "");
    rig.sink.apply(&set);
    assert_eq!(rig.sink.report().digest, set.digest);
}

/// Proves a set whose digest is not the digest of its keys is refused with that reason and
/// written nowhere, the report says which digest was refused, and what was in force stays in force
/// and reported.
#[test]
fn a_digest_that_is_not_the_digest_of_the_keys_is_refused() {
    let rig = rig("refuse-digest");
    let good = push(keys(&[1]));
    rig.sink.apply(&good);
    let before = fs::read_to_string(rig.file()).expect("the file");
    let bad = PushedKeys {
        digest: rules::digest(&keys(&[7])),
        keys: keys(&[2]),
    };
    rig.sink.apply(&bad);
    let reason = "the coordinator's digest is not the digest of its keys";
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: good.digest.clone(),
            refused: Some(KeysRefusal {
                digest: bad.digest.clone(),
                reason: reason.to_owned(),
            }),
            failed: String::new(),
        }
    );
    assert_eq!(fs::read_to_string(rig.file()).expect("the file"), before);
    assert_eq!(
        rig.lines().last().expect("a line"),
        &format!("error: operator keys {} are refused: {reason}", short(&bad))
    );
}

/// Proves a set that breaks a rule of a set is refused with the rule's own text, which names the
/// key's place and never its text, and a later good set takes the refusal away.
#[test]
fn a_set_that_breaks_a_rule_is_refused_without_its_key_text() {
    let rig = rig("refuse-set");
    let bad = push(vec!["ssh-rsa SECRETTEXT".to_owned()]);
    rig.sink.apply(&bad);
    let reason = "key 1: a key blob that is not base64 of its key type";
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: String::new(),
            refused: Some(KeysRefusal {
                digest: bad.digest.clone(),
                reason: reason.to_owned(),
            }),
            failed: String::new(),
        }
    );
    assert!(!rig.file().exists());
    assert_eq!(
        rig.lines(),
        [format!(
            "error: operator keys {} are refused: {reason}",
            short(&bad)
        )]
    );
    rig.sink.apply(&push(keys(&[1])));
    assert_eq!(rig.sink.report().refused, None);
}

/// Proves a host with no account the rule picks refuses the set with the source's reason, keeping
/// what is in force, while a source that cannot read its file fails it: the report then holds no
/// digest and says why, and a later set that works clears the failure.
#[test]
fn an_account_that_is_refused_refuses_the_set_and_one_that_fails_fails_it() {
    let rig = rig("account");
    let good = push(keys(&[1]));
    rig.sink.apply(&good);
    let next = push(keys(&[2]));
    rig.answer(Err(AccountError::Refused(
        "no local administrator account".to_owned(),
    )));
    rig.sink.apply(&next);
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: good.digest.clone(),
            refused: Some(KeysRefusal {
                digest: next.digest.clone(),
                reason: "no local administrator account".to_owned(),
            }),
            failed: String::new(),
        }
    );

    rig.answer(Err(AccountError::Failed(
        "dscl list of users failed with exit code 70".to_owned(),
    )));
    rig.sink.apply(&next);
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: String::new(),
            refused: None,
            failed: "dscl list of users failed with exit code 70".to_owned(),
        }
    );
    assert_eq!(
        rig.lines().last().expect("a line"),
        &format!(
            "error: operator keys {} are not in force: dscl list of users failed with exit code 70",
            short(&next)
        )
    );

    rig.answer(Ok(rig.account.clone()));
    rig.sink.apply(&next);
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: next.digest.clone(),
            refused: None,
            failed: String::new(),
        }
    );
}

/// Proves a file that cannot be written fails the set with the file's error: a file whose markers
/// make no block is left as it is, and the report holds no digest, so the coordinator sends the set
/// again. The reason writes the home `~`. Catches: a Mac's home path, which names its account, in
/// a report that goes to the coordinator.
#[test]
fn a_file_that_cannot_be_written_fails_the_set() {
    let rig = rig("file-fails");
    let good = push(keys(&[1]));
    rig.sink.apply(&good);
    let broken = format!("{BEGIN}\nhand\n");
    fs::write(rig.file(), &broken).expect("the file");
    let next = push(keys(&[2]));
    rig.sink.apply(&next);
    assert_eq!(fs::read_to_string(rig.file()).expect("the file"), broken);
    // The home is written `~`: a Mac's home names its account, which no report carries.
    let failed =
        "~/.ssh/authorized_keys: the block that begins at line 1 has no end line".to_owned();
    assert!(
        !rig.sink
            .report()
            .failed
            .contains(&*rig.account.home.to_string_lossy())
    );
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: String::new(),
            refused: None,
            failed: failed.clone(),
        }
    );
    assert_eq!(
        rig.lines().last().expect("a line"),
        &format!(
            "error: operator keys {} are not in force: {failed}",
            short(&next)
        )
    );
}

/// Proves a reason is cut to 256 bytes at a character boundary, in the report and in the log.
#[test]
fn a_long_reason_is_cut_at_a_character_boundary() {
    let rig = rig("clip");
    let reason = format!("{}{}", "a".repeat(255), "\u{e9}".repeat(10));
    rig.answer(Err(AccountError::Failed(reason.clone())));
    let set = push(keys(&[1]));
    rig.sink.apply(&set);
    assert_eq!(rig.sink.report().failed, "a".repeat(255));
    assert_eq!(clip(&reason), "a".repeat(255));
    assert_eq!(clip("short"), "short");
    assert_eq!(clip(&"b".repeat(300)), "b".repeat(256));
}

/// A Mac's three programs, scripted so that the steps find Remote Login on and the account a
/// member of the access group; each fake keeps the calls it was given.
fn ready_mac() -> (MacSsh, FakeLaunchctl, FakeLaunchctl, FakeLaunchctl) {
    let launchctl = FakeLaunchctl::new();
    let dscl = FakeLaunchctl::new();
    let dseditgroup = FakeLaunchctl::new();
    launchctl.always(
        &words(&["print", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::success("loaded\n")),
    );
    dscl.always(
        &words(&[
            ".",
            "-read",
            "/Groups/com.apple.access_ssh",
            "GroupMembership",
        ]),
        Ok(LaunchctlOutput::success("GroupMembership: ops\n")),
    );
    let steps = MacSsh::new(
        Box::new(launchctl.clone()),
        Box::new(dscl.clone()),
        Box::new(dseditgroup.clone()),
    );
    (steps, launchctl, dscl, dseditgroup)
}

/// Proves on a Mac a set with keys makes sure of Remote Login and the account's ssh access after
/// the file, and the empty set runs none of it.
#[test]
fn a_mac_makes_sure_of_ssh_for_a_set_with_keys_and_not_for_the_empty_set() {
    let (steps, launchctl, dscl, dseditgroup) = ready_mac();
    let rig = rig_with(Some(steps), "mac-ensure");
    rig.sink.apply(&push(Vec::new()));
    assert!(launchctl.calls().is_empty());
    assert!(dscl.calls().is_empty());
    let set = push(keys(&[1]));
    rig.sink.apply(&set);
    assert_eq!(launchctl.calls().len(), 1);
    assert_eq!(dscl.calls().len(), 1);
    assert!(dseditgroup.calls().is_empty());
    assert_eq!(rig.sink.report().digest, set.digest);
}

/// Proves a Mac whose Remote Login cannot be turned on fails the set after the file is written:
/// the report holds no digest and says why, and the coordinator's next send, with Remote Login on,
/// puts it in force with the file unchanged.
#[test]
fn a_mac_that_cannot_turn_remote_login_on_fails_the_set() {
    let launchctl = FakeLaunchctl::new();
    let print = words(&["print", "system/com.openssh.sshd"]);
    launchctl.always(
        &print,
        Ok(LaunchctlOutput::exit(113, "Could not find service\n")),
    );
    launchctl.always(
        &words(&["enable", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::exit(1, "denied\n")),
    );
    let dscl = FakeLaunchctl::new();
    dscl.always(
        &words(&[
            ".",
            "-read",
            "/Groups/com.apple.access_ssh",
            "GroupMembership",
        ]),
        Ok(LaunchctlOutput::success("GroupMembership: ops\n")),
    );
    let steps = MacSsh::new(
        Box::new(launchctl.clone()),
        Box::new(dscl),
        Box::new(FakeLaunchctl::new()),
    );
    let rig = rig_with(Some(steps), "mac-fails");
    let set = push(keys(&[1]));
    rig.sink.apply(&set);
    let failed = "Remote Login is not on: launchctl enable of sshd failed with exit code 1: denied";
    assert_eq!(
        rig.sink.report(),
        KeysReport {
            digest: String::new(),
            refused: None,
            failed: failed.to_owned(),
        }
    );
    assert!(
        fs::read_to_string(rig.file())
            .expect("the file")
            .contains(&set.keys[0])
    );
    assert_eq!(
        rig.lines(),
        [format!(
            "error: operator keys {} are not in force: {failed}",
            short(&set)
        )]
    );
    launchctl.always(&print, Ok(LaunchctlOutput::success("loaded\n")));
    rig.sink.apply(&set);
    assert_eq!(rig.sink.report().digest, set.digest);
    assert_eq!(rig.sink.report().failed, "");
}

/// Proves a Mac whose login account may not use ssh (the access group exists and `dseditgroup`
/// fails) fails the set, with the account's name nowhere in the reason.
#[test]
fn a_mac_that_cannot_allow_the_account_fails_the_set() {
    let launchctl = FakeLaunchctl::new();
    launchctl.always(
        &words(&["print", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::success("loaded\n")),
    );
    let dscl = FakeLaunchctl::new();
    dscl.always(
        &words(&[
            ".",
            "-read",
            "/Groups/com.apple.access_ssh",
            "GroupMembership",
        ]),
        Ok(LaunchctlOutput::success("GroupMembership: other\n")),
    );
    let dseditgroup = FakeLaunchctl::new();
    dseditgroup.always(
        &words(&[
            "-o",
            "edit",
            "-a",
            "ops",
            "-t",
            "user",
            "com.apple.access_ssh",
        ]),
        Ok(LaunchctlOutput::exit(66, "not authorized\n")),
    );
    let steps = MacSsh::new(Box::new(launchctl), Box::new(dscl), Box::new(dseditgroup));
    let rig = rig_with(Some(steps), "mac-access");
    rig.sink.apply(&push(keys(&[1])));
    let report = rig.sink.report();
    assert_eq!(
        report.failed,
        "the login account is not allowed to use ssh: dseditgroup add to the ssh access group \
         failed with exit code 66"
    );
    assert!(!report.failed.contains("ops"));
    assert_eq!(report.digest, "");
}

/// Proves a sink is made for each kind of host and starts with nothing in force: no program is run
/// and no file is read until a set comes.
#[test]
fn a_sink_is_made_for_each_kind_of_host() {
    for kind in [
        PlatformKind::Linux,
        PlatformKind::Openrc,
        PlatformKind::Macos,
    ] {
        let sink = OperatorKeys::at_host(kind, Arc::new(RecordingLog::default()));
        assert_eq!(sink.report(), KeysReport::default(), "{kind:?}");
        assert_eq!(sink.mac.is_some(), kind == PlatformKind::Macos, "{kind:?}");
    }
}

/// Proves a lock poisoned by a thread that panicked does not stop the report or the next set.
#[test]
fn a_poisoned_lock_does_not_stop_the_sink() {
    let rig = rig("poisoned");
    let sink = Arc::new(rig.sink);
    let poisoner = Arc::clone(&sink);
    let result = std::thread::spawn(move || {
        let _state = poisoner.state.lock().expect("the lock");
        panic!("a thread that panics holding the lock");
    })
    .join();
    assert!(result.is_err());
    assert_eq!(sink.report(), KeysReport::default());
    let set = push(keys(&[1]));
    sink.apply(&set);
    assert_eq!(sink.report().digest, set.digest);
}

/// Proves a reason has the account's home written `~`, and that a home of `/` (or none) leaves the
/// reason as it is rather than writing every `/` as `~`.
#[test]
fn a_reason_writes_the_home_as_a_tilde() {
    let account = |home: &str| Account {
        name: "admin".to_owned(),
        uid: 501,
        gid: 20,
        home: PathBuf::from(home),
    };
    let reason = concat!("/Users", "/admin/.ssh is not a directory");
    assert_eq!(
        home_as_tilde(reason, &account(concat!("/Users", "/admin"))),
        "~/.ssh is not a directory"
    );
    assert_eq!(home_as_tilde(reason, &account("/")), reason);
    assert_eq!(home_as_tilde(reason, &account("")), reason);
}

/// Proves `redact` writes the account's home `~` and its name `the login account`, the home first
/// (it holds the name), and leaves an empty name and a home of `/` alone.
#[test]
fn a_reason_is_redacted_of_the_home_and_the_name() {
    let account = |name: &str, home: &str| Account {
        name: name.to_owned(),
        uid: 501,
        gid: 20,
        home: PathBuf::from(home),
    };
    let home = concat!("/Users", "/admin");
    let reason = format!("{home}/.ssh: record admin is gone");
    assert_eq!(
        redact(&reason, &account("admin", home)),
        "~/.ssh: record the login account is gone"
    );
    assert_eq!(redact(&reason, &account("", "/")), reason);
}

/// Proves a reason that holds the account's name and home reaches the report without them: the
/// `dseditgroup` step gives its exit code alone though its standard error names both, and a
/// `launchctl` step's standard error that names both is redacted before it is reported and logged.
///
/// Catches: a failure reason that sent the account's name or home to the coordinator.
#[test]
fn a_failure_that_names_the_account_and_its_home_reports_neither() {
    let launchctl = FakeLaunchctl::new();
    let dscl = FakeLaunchctl::new();
    let dseditgroup = FakeLaunchctl::new();
    let steps = MacSsh::new(
        Box::new(launchctl.clone()),
        Box::new(dscl.clone()),
        Box::new(dseditgroup.clone()),
    );
    let rig = rig_with(Some(steps), "mac-names");
    let said = format!("ops at {}\n", rig.account.home.display());
    launchctl.always(
        &words(&["print", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::success("loaded\n")),
    );
    dscl.always(
        &words(&[
            ".",
            "-read",
            "/Groups/com.apple.access_ssh",
            "GroupMembership",
        ]),
        Ok(LaunchctlOutput::success("GroupMembership: other\n")),
    );
    dseditgroup.always(
        &words(&[
            "-o",
            "edit",
            "-a",
            "ops",
            "-t",
            "user",
            "com.apple.access_ssh",
        ]),
        Ok(LaunchctlOutput::exit(66, &said)),
    );
    let set = push(keys(&[1]));
    rig.sink.apply(&set);
    let expected = "the login account is not allowed to use ssh: dseditgroup add to the ssh \
                    access group failed with exit code 66";
    assert_eq!(rig.sink.report().failed, expected);
    assert_eq!(
        rig.lines().last().expect("a line"),
        &format!(
            "error: operator keys {} are not in force: {expected}",
            short(&set)
        )
    );

    launchctl.always(
        &words(&["print", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::exit(113, "")),
    );
    launchctl.always(
        &words(&["enable", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::exit(1, &said)),
    );
    rig.sink.apply(&set);
    assert_eq!(
        rig.sink.report().failed,
        "Remote Login is not on: launchctl enable of sshd failed with exit code 1: \
         the login account at ~"
    );
}
