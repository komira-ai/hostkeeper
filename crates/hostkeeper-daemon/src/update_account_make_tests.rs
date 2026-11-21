//! The tests of `update_account_make`: the update-account file and the bootstrap's settings in a
//! scratch directory, owned by the test's own user, and the Mac's programs as one scripted Mac
//! ([`Mac`]) whose `dscl`, `dseditgroup` and `sysadminctl` read and change one directory, and
//! record every call (its arguments and its standard input) for an exact comparison. One test runs
//! the two `sysadminctl` calls through the real [`StdinProcess`], over this test binary as its
//! child, to see the arguments, the environment and the standard input a program is given. No real
//! account, file or program of the host is touched.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_launchd::{LaunchctlOutput, StdinError};
use hostkeeper_testkit::Scratch;

use super::fake::*;
use super::*;
use crate::login_account::{AccountError, AccountSource, MacAdmin};

/// The farm CA's hash of a settings source.
const CA: &str = "3d2a0000000000000000000000000000000000000000000000000000000000aa";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The update-account file in `dir`, naming [`NAME`] and [`PASSWORD`], with `mode`.
fn account_file(dir: &Scratch, mode: u32) -> PathBuf {
    let path = dir.path().join("update-account");
    fs::write(&path, format!("name={NAME}\npassword={PASSWORD}\n")).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
    path
}

/// An administrator's source that says what it is told, and counts how often it was asked.
#[derive(Clone)]
struct Admins {
    answer: Result<Option<(String, String)>, String>,
    asked: Arc<Mutex<u32>>,
}

impl Admins {
    fn of(answer: Result<Option<(&str, &str)>, &str>) -> Admins {
        Admins {
            answer: answer
                .map(|admin| admin.map(|(name, pw)| (name.to_owned(), pw.to_owned())))
                .map_err(str::to_owned),
            asked: Arc::default(),
        }
    }

    fn setup_assistant() -> Admins {
        Admins::of(Ok(Some((ADMIN, ADMIN_PASSWORD))))
    }

    fn asked(&self) -> u32 {
        *self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl AdminSource for Admins {
    fn admin(&self) -> Result<Option<Admin>, String> {
        *self.asked.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.answer
            .clone()
            .map(|admin| admin.map(|(name, password)| Admin::new(name, password.into_bytes())))
    }
}

/// What was said, line by line.
#[derive(Clone, Default)]
struct Said(Arc<Mutex<Vec<String>>>);

impl Said {
    fn say(&self) -> impl Fn(&str) + '_ {
        |line| {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line.to_owned());
        }
    }

    fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A rig: the file in a scratch directory, the scripted Mac and the administrator's source.
struct Rig {
    _dir: Scratch,
    file: PathBuf,
    mac: Mac,
    admins: Admins,
}

impl Rig {
    fn new(name: &str, mac: Mac) -> Rig {
        let dir = scratch(name);
        let file = account_file(&dir, 0o600);
        Rig {
            _dir: dir,
            file,
            mac,
            admins: Admins::setup_assistant(),
        }
    }

    fn maker(&self) -> Maker {
        Maker::new(
            self.file.clone(),
            me(),
            Box::new(self.admins.clone()),
            self.mac.programs(),
        )
    }

    /// [`Maker::make`], and what it said.
    fn make(&self) -> (Result<Vec<Change>, String>, Vec<String>) {
        let said = Said::default();
        let made = self.maker().make(&said.say());
        (made, said.lines())
    }
}

/// The user record of the update account.
fn user() -> String {
    format!("{USERS}/{NAME}")
}

fn read_user() -> Call {
    Call {
        program: "dscl",
        args: words(&[".", "-read", &user(), "RecordName", "IsHidden", "UserShell"]),
        input: None,
    }
}

fn create(key: &str, value: &str) -> Call {
    Call {
        program: "dscl",
        args: words(&[".", "-create", &user(), key, value]),
        input: None,
    }
}

fn read_ssh() -> Call {
    Call {
        program: "dscl",
        args: words(&[".", "-read", ACCESS_SSH_GROUP, "GroupMembership"]),
        input: None,
    }
}

fn leave_ssh() -> Call {
    Call {
        program: "dseditgroup",
        args: words(&["-o", "edit", "-d", NAME, "-t", "user", ACCESS_GROUP]),
        input: None,
    }
}

fn token_status() -> Call {
    Call {
        program: "sysadminctl",
        args: words(&["-secureTokenStatus", NAME]),
        input: None,
    }
}

fn add_user_args() -> Vec<String> {
    words(&["-addUser", NAME, "-password", "-", "-admin"])
}

fn add_user() -> Call {
    Call {
        program: "sysadminctl",
        args: add_user_args(),
        input: Some(format!("{PASSWORD}\n")),
    }
}

fn token_on_args() -> Vec<String> {
    words(&[
        "-secureTokenOn",
        NAME,
        "-password",
        "-",
        "-adminUser",
        ADMIN,
        "-adminPassword",
        "-",
    ])
}

fn token_on() -> Call {
    Call {
        program: "sysadminctl",
        args: token_on_args(),
        input: Some(format!("{PASSWORD}\n{ADMIN_PASSWORD}\n")),
    }
}

/// The line said for `change`.
fn line(change: Change) -> String {
    change.line()
}

/// Catches a missing account not being made, being made out of order or visible longer than it
/// must, or a password reaching anything but the standard input of the two `sysadminctl` calls:
/// every call is compared whole (arguments and input), the account's password and a newline go
/// to `-addUser`, both passwords in the order of the arguments to `-secureTokenOn`, and each
/// change is said once, naming no account.
#[test]
fn a_missing_account_is_made_hidden_and_given_a_token_with_the_passwords_on_standard_input() {
    let rig = Rig::new("make-missing", Mac::new());
    let (made, said) = rig.make();
    let changes = vec![
        Change::Created,
        Change::Hidden,
        Change::NoShell,
        Change::TokenGranted,
    ];
    assert_eq!(made, Ok(changes.clone()));
    assert_eq!(
        rig.mac.calls(),
        vec![
            read_user(),
            add_user(),
            read_user(),
            create("IsHidden", "1"),
            create("UserShell", NO_SHELL),
            read_ssh(),
            token_status(),
            token_on(),
            token_status(),
        ]
    );
    assert_eq!(said, changes.into_iter().map(line).collect::<Vec<_>>());
    assert_eq!(
        said,
        vec![
            "level=info update_account: the update account was made, an administrator (sysadminctl -addUser)",
            "level=info update_account: the update account was hidden (IsHidden 1)",
            "level=info update_account: the update account's login shell was set to /usr/bin/false",
            "level=info update_account: the update account was given a Secure Token (sysadminctl -secureTokenOn)",
        ]
    );
    assert_eq!(
        rig.mac.user(NAME),
        Some(User {
            uid: 502,
            hidden: true,
            shell: NO_SHELL.to_owned(),
            token: true
        })
    );
    assert!(rig.mac.state().admins.contains(&NAME.to_owned()));
    assert_eq!(rig.admins.asked(), 1);
}

/// Catches the made account becoming the login account the operator keys go to, or leaving the
/// Mac with two visible administrators (which gives the keys to none): after the start, the rule
/// of `login_account` still picks the Setup Assistant administrator.
#[test]
fn the_made_account_is_hidden_and_not_the_operator_keys_login_account() {
    let rig = Rig::new("make-login-account", Mac::new());
    let (made, _) = rig.make();
    assert!(made.is_ok(), "{made:?}");
    let account = MacAdmin::new(rig.mac.port("dscl"))
        .login_account()
        .expect("one visible administrator");
    assert_eq!(account.name, ADMIN);
    assert_eq!(account.home, PathBuf::from(format!("{USERS}/{ADMIN}")));
}

/// Catches the hide being what keeps the operator keys' account: when the hide fails, the start
/// says why, makes no token call, and the Mac is left with two visible administrators, which the
/// login account's rule refuses.
#[test]
fn a_failed_hide_stops_the_steps_and_says_why() {
    let rig = Rig::new("make-hide-fails", Mac::new());
    rig.mac
        .fail("dscl", &["-create", "IsHidden"], Fails::Exit(1));
    let (made, said) = rig.make();
    assert_eq!(
        made,
        Err("dscl write of the update account's IsHidden failed with exit code 1".to_owned())
    );
    assert_eq!(said, vec![line(Change::Created)]);
    assert_eq!(
        rig.mac.calls(),
        vec![
            read_user(),
            add_user(),
            read_user(),
            create("IsHidden", "1")
        ]
    );
    assert_eq!(
        MacAdmin::new(rig.mac.port("dscl")).login_account(),
        Err(AccountError::Refused(
            "2 local administrator accounts: the managed block takes exactly one".to_owned()
        ))
    );
}

/// Catches an account that is as it should be being changed, or the administrator's password
/// being read when no call needs it: three reads, no call that changes anything, nothing said.
#[test]
fn an_account_that_is_ready_gets_no_call_that_changes_it() {
    let mac = Mac::new();
    mac.add_ready(true);
    let rig = Rig::new("make-ready", mac);
    let (made, said) = rig.make();
    assert_eq!(made, Ok(Vec::new()));
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(
        rig.mac.calls(),
        vec![read_user(), read_ssh(), token_status()]
    );
    assert_eq!(rig.admins.asked(), 0);
}

/// Catches an account without a token being made again, or given anything but the token call.
#[test]
fn an_account_without_a_token_gets_the_token_call_alone() {
    let mac = Mac::new();
    mac.add_ready(false);
    let rig = Rig::new("make-no-token", mac);
    let (made, said) = rig.make();
    assert_eq!(made, Ok(vec![Change::TokenGranted]));
    assert_eq!(said, vec![line(Change::TokenGranted)]);
    assert_eq!(
        rig.mac.calls(),
        vec![
            read_user(),
            read_ssh(),
            token_status(),
            token_on(),
            token_status()
        ]
    );
    assert!(rig.mac.user(NAME).expect("there").token);
}

/// Catches a visible account with a login shell, in the ssh access group, being left so: it is
/// hidden, its shell set to none and it is taken out of the group (no other member is), and a
/// token it holds is not asked for again.
#[test]
fn a_visible_account_with_a_shell_in_the_ssh_group_is_hidden_and_taken_out() {
    let mac = Mac::new();
    mac.add(NAME, 502, false, "/bin/zsh", true);
    mac.state().ssh = Some(words(&[ADMIN, NAME]));
    let rig = Rig::new("make-visible", mac);
    let (made, said) = rig.make();
    let changes = vec![Change::Hidden, Change::NoShell, Change::LeftSsh];
    assert_eq!(made, Ok(changes.clone()));
    assert_eq!(said, changes.into_iter().map(line).collect::<Vec<_>>());
    assert_eq!(
        said[2],
        "level=info update_account: the update account was taken out of com.apple.access_ssh"
    );
    assert_eq!(
        rig.mac.calls(),
        vec![
            read_user(),
            create("IsHidden", "1"),
            create("UserShell", NO_SHELL),
            read_ssh(),
            leave_ssh(),
            token_status()
        ]
    );
    assert_eq!(rig.mac.state().ssh, Some(words(&[ADMIN])));
    assert_eq!(rig.admins.asked(), 0);
}

/// Catches an ssh access group that holds others being read as holding the account.
#[test]
fn an_ssh_group_without_the_account_is_left_alone() {
    let mac = Mac::new();
    mac.add_ready(true);
    mac.state().ssh = Some(words(&[ADMIN]));
    let rig = Rig::new("make-ssh-others", mac);
    let (made, _) = rig.make();
    assert_eq!(made, Ok(Vec::new()));
    assert_eq!(
        rig.mac.calls(),
        vec![read_user(), read_ssh(), token_status()]
    );
}

/// The texts a test must find no password in.
fn holds_a_password(text: &str) -> bool {
    text.contains(PASSWORD) || text.contains(ADMIN_PASSWORD)
}

/// Catches a failing `sysadminctl` call's error, or any line the start says, holding a byte of a
/// password or of what the program wrote (here: its input, both passwords, on both streams): the
/// error is the step and the exit code alone, and the start says it once, as its own text.
#[test]
fn a_failing_call_says_its_step_and_exit_code_and_no_password() {
    let cases = [
        (
            "-addUser",
            None,
            "sysadminctl -addUser: the program exited with code 3",
        ),
        (
            "-secureTokenOn",
            Some(false),
            "sysadminctl -secureTokenOn: the program exited with code 3",
        ),
    ];
    for (call, existing, error) in cases {
        let mac = Mac::new();
        if let Some(token) = existing {
            mac.add_ready(token);
        }
        mac.fail("sysadminctl", &[call], Fails::Exit(3));
        let rig = Rig::new("make-call-fails", mac);
        let said = Said::default();
        let made = at_start(PlatformKind::Macos, &rig.maker(), &said.say());
        assert_eq!(made, Err(error.to_owned()), "{call}");
        assert_eq!(
            said.lines(),
            vec![format!(
                "level=error update_account: {error}; the start goes on, and the next start tries again"
            )],
            "{call}"
        );
        assert!(!said.lines().iter().any(|line| holds_a_password(line)));
        assert!(!holds_a_password(&format!("{made:?}")));
        let echoed = rig
            .mac
            .calls()
            .into_iter()
            .filter(|c| c.args.contains(&call.to_owned()))
            .count();
        assert_eq!(echoed, 1, "{call}: the call ran once");
    }
}

/// Catches a `sysadminctl` that cannot be started being said with more than its step and the
/// system's reason.
#[test]
fn a_sysadminctl_that_cannot_start_is_said_by_its_step() {
    let rig = Rig::new("make-unrunnable", Mac::new());
    rig.mac
        .fail("sysadminctl", &["-addUser"], Fails::Unrunnable);
    let (made, said) = rig.make();
    assert_eq!(
        made,
        Err(
            "sysadminctl -addUser: the program could not be started: No such file or directory (os error 2)"
                .to_owned()
        )
    );
    assert_eq!(said, Vec::<String>::new());
}

/// Catches a `sysadminctl` that exits 0 without doing what it was asked being taken at its word:
/// an account still missing after `-addUser`, and no token after `-secureTokenOn`, are each the
/// step's error.
#[test]
fn a_sysadminctl_that_does_nothing_is_an_error() {
    let rig = Rig::new("make-inert-add", Mac::new());
    rig.mac.state().inert = true;
    let (made, said) = rig.make();
    assert_eq!(
        made,
        Err("sysadminctl -addUser exited 0, but dscl does not find the update account".to_owned())
    );
    assert_eq!(said, vec![line(Change::Created)]);
    assert_eq!(rig.mac.calls(), vec![read_user(), add_user(), read_user()]);

    let mac = Mac::new();
    mac.add_ready(false);
    mac.state().inert = true;
    let rig = Rig::new("make-inert-token", mac);
    let (made, said) = rig.make();
    assert_eq!(
        made,
        Err(
            "sysadminctl -secureTokenOn exited 0, but the update account holds no Secure Token"
                .to_owned()
        )
    );
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(
        rig.mac.calls(),
        vec![
            read_user(),
            read_ssh(),
            token_status(),
            token_on(),
            token_status()
        ]
    );
}

/// Catches a token being granted on a guess: an answer of `-secureTokenStatus` that says neither
/// ENABLED nor DISABLED makes no token call.
#[test]
fn an_unread_token_answer_grants_nothing() {
    let mac = Mac::new();
    mac.add_ready(false);
    mac.state().token_text =
        Some("2026-11-20 10:00:00.000 sysadminctl[512:4096] Done\n".to_owned());
    let rig = Rig::new("make-token-unknown", mac);
    let (made, _) = rig.make();
    assert_eq!(
        made,
        Err(
            "sysadminctl -secureTokenStatus said neither ENABLED nor DISABLED; no Secure Token is \
             granted"
                .to_owned()
        )
    );
    assert_eq!(
        rig.mac.calls(),
        vec![read_user(), read_ssh(), token_status()]
    );
    assert_eq!(rig.admins.asked(), 0);
}

/// Catches a token call with no administrator, or with a reason holding a value of the settings:
/// settings that name none, and settings that cannot be read, each make no token call.
#[test]
fn no_administrator_makes_no_token_call() {
    let cases = [
        (
            Ok(None),
            "the update account holds no Secure Token, and the bootstrap's settings name no \
             administrator to give it one",
        ),
        (
            Err("the settings are refused"),
            "the administrator's keys could not be read: the settings are refused",
        ),
    ];
    for (answer, error) in cases {
        let mac = Mac::new();
        mac.add_ready(false);
        let mut rig = Rig::new("make-no-admin", mac);
        rig.admins = Admins::of(answer);
        let (made, _) = rig.make();
        assert_eq!(made, Err(error.to_owned()));
        assert_eq!(
            rig.mac.calls(),
            vec![read_user(), read_ssh(), token_status()]
        );
        assert_eq!(rig.admins.asked(), 1);
    }
}

/// Catches the administrator's password being kept after its call: the maker asks its source
/// again for the next grant (it holds no copy), and gives the password to that call alone.
#[test]
fn the_administrators_password_is_read_for_each_grant_and_not_kept() {
    let mac = Mac::new();
    mac.add_ready(false);
    let rig = Rig::new("make-not-kept", mac);
    let maker = rig.maker();
    let said = Said::default();
    assert_eq!(maker.make(&said.say()), Ok(vec![Change::TokenGranted]));
    assert_eq!(rig.admins.asked(), 1);
    rig.mac.state().users.get_mut(NAME).expect("there").token = false;
    assert_eq!(maker.make(&said.say()), Ok(vec![Change::TokenGranted]));
    assert_eq!(rig.admins.asked(), 2);
    let with_input: Vec<Call> = rig
        .mac
        .calls()
        .into_iter()
        .filter(|call| call.input.is_some())
        .collect();
    assert_eq!(with_input, vec![token_on(), token_on()]);
    assert_eq!(
        format!("{:?}", Admin::new(ADMIN.to_owned(), ADMIN_PASSWORD.into())),
        "Admin { name: \"admin\", password: withheld }"
    );
}

/// Catches a Mac without the update-account file running anything, and a file that is refused
/// running anything or being said with a byte of it.
#[test]
fn no_file_or_a_refused_file_runs_nothing() {
    let rig = Rig::new("make-no-file", Mac::new());
    fs::remove_file(&rig.file).expect("removed");
    assert_eq!(rig.make(), (Ok(Vec::new()), Vec::new()));
    assert_eq!(rig.mac.calls(), Vec::new());

    let dir = scratch("make-refused-file");
    let file = account_file(&dir, 0o644);
    let mac = Mac::new();
    let admins = Admins::setup_assistant();
    let maker = Maker::new(file.clone(), me(), Box::new(admins.clone()), mac.programs());
    assert_eq!(
        maker.make(&|_| {}),
        Err(format!(
            "the update-account file {} has mode 0644, not 0600",
            file.display()
        ))
    );
    assert_eq!(mac.calls(), Vec::new());
    assert_eq!(admins.asked(), 0);
}

/// Catches each read or write that fails being taken for another answer, or said with what a
/// program wrote when its arguments name the account: the step and the exit code alone, but the
/// ssh access group's read, whose arguments name no account, which adds the first line of its
/// standard error.
#[test]
fn a_failing_read_or_write_is_said_by_its_step() {
    let cases: [(&'static str, &[&str], Fails, String); 8] = [
        (
            "dscl",
            &["-read", "UserShell"],
            Fails::Exit(1),
            "dscl read of the update account failed with exit code 1".to_owned(),
        ),
        (
            "dscl",
            &["-read", "UserShell"],
            Fails::Unrunnable,
            "dscl read of the update account could not be run: No such file or directory (os error 2)"
                .to_owned(),
        ),
        (
            "dscl",
            &["-create", "UserShell"],
            Fails::Exit(1),
            "dscl write of the update account's UserShell failed with exit code 1".to_owned(),
        ),
        (
            "dscl",
            &["-create", "IsHidden"],
            Fails::Unrunnable,
            "dscl write of the update account's IsHidden could not be run: No such file or \
             directory (os error 2)"
                .to_owned(),
        ),
        (
            "dscl",
            &[ACCESS_SSH_GROUP],
            Fails::Unrunnable,
            "dscl read of the ssh access group could not be run: No such file or directory (os \
             error 2)"
                .to_owned(),
        ),
        (
            "dseditgroup",
            &["-d"],
            Fails::Exit(64),
            "dseditgroup removal from the ssh access group failed with exit code 64".to_owned(),
        ),
        (
            "dseditgroup",
            &["-d"],
            Fails::Unrunnable,
            "dseditgroup removal from the ssh access group could not be run: No such file or \
             directory (os error 2)"
                .to_owned(),
        ),
        (
            "sysadminctl",
            &["-secureTokenStatus"],
            Fails::Exit(1),
            "sysadminctl -secureTokenStatus failed with exit code 1".to_owned(),
        ),
    ];
    for (program, marks, how, error) in cases {
        let mac = Mac::new();
        mac.add(NAME, 502, false, "/bin/zsh", false);
        mac.state().ssh = Some(words(&[NAME]));
        mac.fail(program, marks, how);
        let rig = Rig::new("make-read-fails", mac);
        let (made, _) = rig.make();
        assert_eq!(made, Err(error), "{program} {marks:?}");
    }
}

/// Catches an ssh access group's read that fails being taken for a group that is not there: a
/// failure other than `eDSRecordNotFound` is the step's error, with the first line of its
/// standard error.
#[test]
fn an_ssh_group_read_that_fails_is_an_error_and_a_missing_group_is_none() {
    let mac = Mac::new();
    mac.add_ready(true);
    let rig = Rig::new("make-ssh-read", mac);
    assert_eq!(rig.make().0, Ok(Vec::new()));

    let mac = Mac::new();
    mac.add_ready(true);
    mac.state().ssh = Some(Vec::new());
    mac.fail("dscl", &[ACCESS_SSH_GROUP], Fails::Exit(2));
    let rig = Rig::new("make-ssh-read-fails", mac);
    assert_eq!(
        rig.make().0,
        Err("dscl read of the ssh access group failed with exit code 2".to_owned())
    );
}

/// Catches a host that is not a Mac making the account, or running anything.
#[test]
fn a_linux_start_makes_nothing() {
    let rig = Rig::new("make-linux", Mac::new());
    let said = Said::default();
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        assert_eq!(at_start(kind, &rig.maker(), &said.say()), Ok(Vec::new()));
    }
    assert_eq!(rig.mac.calls(), Vec::new());
    assert_eq!(said.lines(), Vec::<String>::new());
    at_host_start(PlatformKind::Linux);
    to_stderr("level=info update_account: the line a Mac's start writes to its standard error");
}

/// A settings source of the bootstrap in `dir`: the farm values and `extra`, mode 0600.
fn bootstrap_toml(dir: &Scratch, extra: &str) -> Sources {
    let fallback = dir.path().join("bootstrap.toml");
    fs::write(
        &fallback,
        format!("coordinator = \"https://coordinator.example\"\nca_sha256 = \"{CA}\"\n{extra}"),
    )
    .expect("the settings");
    fs::set_permissions(&fallback, fs::Permissions::from_mode(0o600)).expect("its mode");
    Sources {
        managed: dir.path().join("dev.hostkeeper.config.plist"),
        fallback,
        trusted_uid: me(),
    }
}

/// Catches the administrator being read from anything but the bootstrap's settings, or read with
/// another's name or password: the two `admin_` keys of the source are the administrator, byte
/// for byte.
#[test]
fn the_administrator_is_read_from_the_bootstrap_settings() {
    let dir = scratch("make-settings");
    let keys = format!(
        "update_account_name = \"{NAME}\"\nupdate_account_password = '{PASSWORD}'\nadmin_name = \"{ADMIN}\"\nadmin_password = '{ADMIN_PASSWORD}'\n"
    );
    let admin = BootstrapSettings::new(bootstrap_toml(&dir, &keys))
        .admin()
        .expect("read")
        .expect("named");
    assert_eq!(admin.name, ADMIN);
    assert_eq!(admin.password, ADMIN_PASSWORD.as_bytes());
}

/// Catches settings without the update account's keys, or no settings at all, being read as an
/// administrator, and settings that are refused being read past: each refusal is the
/// bootstrap's own reason, which holds no value.
#[test]
fn settings_without_the_keys_name_none_and_refused_ones_are_an_error() {
    let dir = scratch("make-settings-none");
    let sources = bootstrap_toml(&dir, "");
    assert!(
        BootstrapSettings::new(sources.clone())
            .admin()
            .expect("read")
            .is_none()
    );
    fs::remove_file(&sources.fallback).expect("removed");
    assert!(
        BootstrapSettings::new(sources)
            .admin()
            .expect("read")
            .is_none()
    );

    let dir = scratch("make-settings-refused");
    let sources = bootstrap_toml(&dir, &format!("admin_password = '{ADMIN_PASSWORD}'\n"));
    let error = BootstrapSettings::new(sources.clone())
        .admin()
        .expect_err("incomplete");
    assert_eq!(
        error,
        format!(
            "{} has no update_account_name: the update account's four keys are given together or \
             not at all",
            sources.fallback.display()
        )
    );
    assert!(!holds_a_password(&error));
    fs::set_permissions(&sources.fallback, fs::Permissions::from_mode(0o620)).expect("its mode");
    let error = BootstrapSettings::new(sources.clone())
        .admin()
        .expect_err("refused");
    assert_eq!(
        error,
        format!(
            "{} is refused: mode 620 lets the group or others write it",
            sources.fallback.display()
        )
    );
}

/// Catches the host's maker reading another file or settings, or another program: its paths are
/// the host's own.
#[test]
fn the_host_maker_reads_the_hosts_own_file_and_settings() {
    let maker = Maker::at_host();
    assert_eq!(maker.file, PathBuf::from(UPDATE_ACCOUNT_FILE));
    assert_eq!(maker.owner, ROOT_UID);
}

/// The record the helper child writes of what it saw: one `arg:` line per argument after the
/// program, one `env:` line per variable, then `stdin:` and its standard input to its end.
fn record(args: &[Vec<u8>], env: &[(Vec<u8>, Vec<u8>)], stdin: &[u8]) -> Vec<u8> {
    let mut text = Vec::new();
    for arg in args {
        text.extend_from_slice(b"arg:");
        text.extend_from_slice(arg);
        text.push(b'\n');
    }
    for (name, value) in env {
        text.extend_from_slice(b"env:");
        text.extend_from_slice(name);
        text.push(b'=');
        text.extend_from_slice(value);
        text.push(b'\n');
    }
    text.extend_from_slice(b"stdin:");
    text.extend_from_slice(stdin);
    text
}

/// The environment this process has, without what a process sets in itself and no parent passed:
/// an instrumented build's `__LLVM_PROFILE_RT_*`, and on macOS CoreFoundation's
/// `__CF_USER_TEXT_ENCODING`, which it sets as it loads (this binary links it through
/// `hostkeeper-bootstrap`'s `rustls-native-certs`).
#[expect(
    clippy::disallowed_methods,
    reason = "the helper child reports the environment its parent gave it"
)]
fn own_environment() -> Vec<(Vec<u8>, Vec<u8>)> {
    std::env::vars_os()
        .filter(|(name, _)| !set_by_itself(name.as_bytes()))
        .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
        .collect()
}

/// Whether the variable `name` is one this process set in itself (see `own_environment`).
fn set_by_itself(name: &[u8]) -> bool {
    name.starts_with(b"__LLVM_PROFILE_RT_")
        || (cfg!(target_os = "macos") && name == b"__CF_USER_TEXT_ENCODING")
}

/// Catches nothing itself: the child of `the_two_calls_get_their_passwords_on_standard_input_and_an_empty_environment`.
#[test]
#[ignore = "the child process of the_two_calls_get_their_passwords_on_standard_input_and_an_empty_environment; run only by it"]
fn child_records_what_it_was_given() {
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    let _ = std::io::stderr().write_all(&record(&args, &own_environment(), &stdin));
    std::process::exit(0);
}

/// The arguments that run the helper child of this module in this binary, before `args`, which
/// the test harness takes as names to match after `--` and the child records.
fn helper() -> Vec<String> {
    vec![
        "update_account_make::tests::child_records_what_it_was_given".to_owned(),
        "--exact".to_owned(),
        "--ignored".to_owned(),
        "--nocapture".to_owned(),
        "--".to_owned(),
    ]
}

/// The runner with input of a test: each call runs this test binary's child through the real
/// [`StdinProcess`], keeps what the child recorded, and then plays the call on the scripted Mac.
struct Child {
    process: StdinProcess,
    mac: Mac,
    records: Mutex<Vec<String>>,
}

/// A handle on a [`Child`], which the test keeps a clone of.
struct Shared(Arc<Child>);

impl StdinRun for Shared {
    fn run(
        &self,
        step: &str,
        args: &[String],
        input: &[u8],
    ) -> Result<LaunchctlOutput, StdinError> {
        let mut given = helper();
        given.extend_from_slice(args);
        let output = self.0.process.run(step, &given, input)?;
        assert_eq!(output.code, Some(0), "the child ran");
        self.0
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(String::from_utf8_lossy(&output.stderr).into_owned());
        StdinRun::run(&*self.0.mac.port("sysadminctl"), step, args, input)
    }
}

/// Catches a password reaching a program's arguments or environment, or its standard input
/// holding anything but the passwords in the order of the arguments: the real runner starts a
/// child for each `sysadminctl` call, and what each child saw (its arguments, its environment,
/// which is empty, and its standard input) is compared whole.
#[test]
fn the_two_calls_get_their_passwords_on_standard_input_and_an_empty_environment() {
    let dir = scratch("make-child");
    let file = account_file(&dir, 0o600);
    let mac = Mac::new();
    let exe = std::env::current_exe().expect("the test binary");
    let child = Arc::new(Child {
        process: StdinProcess::new(&exe, Duration::from_secs(60)),
        mac: mac.clone(),
        records: Mutex::default(),
    });
    let mut programs = mac.programs();
    programs.sysadminctl_input = Box::new(Shared(Arc::clone(&child)));
    let maker = Maker::new(file, me(), Box::new(Admins::setup_assistant()), programs);
    assert_eq!(
        maker.make(&|_| {}),
        Ok(vec![
            Change::Created,
            Change::Hidden,
            Change::NoShell,
            Change::TokenGranted
        ])
    );
    let seen = |args: Vec<String>, stdin: &str| {
        let mut given = helper();
        given.extend(args);
        let given: Vec<Vec<u8>> = given.iter().map(|arg| arg.as_bytes().to_vec()).collect();
        String::from_utf8_lossy(&record(&given, &[], stdin.as_bytes())).into_owned()
    };
    assert_eq!(
        *child.records.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            seen(add_user_args(), &format!("{PASSWORD}\n")),
            seen(token_on_args(), &format!("{PASSWORD}\n{ADMIN_PASSWORD}\n")),
        ]
    );
}
