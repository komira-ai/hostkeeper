//! The tests of `maintenance_account`: the collector over a scripted source of the account's name
//! and scripted `dscl` and `sysadminctl` that record their calls, and the file source over a
//! scratch directory. Sections, reasons and call lists are asserted by equality; no real file,
//! account or program is touched.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};
use hostkeeper_testkit::Scratch;
use prost::Message;

use super::*;
use crate::maintenance_collect::LinuxScope;

const USER_HIDDEN: &str = include_str!("../tests/fixtures/maintenance/account/user-hidden.txt");
const USER_VISIBLE: &str = include_str!("../tests/fixtures/maintenance/account/user-visible.txt");
const USER_NOT_HIDDEN: &str =
    include_str!("../tests/fixtures/maintenance/account/user-not-hidden.txt");
const ADMIN_MEMBERS: &str = include_str!("../tests/fixtures/maintenance/account/admin-members.txt");
const ADMIN_OTHERS: &str = include_str!("../tests/fixtures/maintenance/account/admin-others.txt");
const TOKEN_DISABLED_LINE: &str =
    include_str!("../tests/fixtures/maintenance/account/token-disabled.txt");
const TOKEN_ENABLED_LINE: &str =
    include_str!("../tests/fixtures/maintenance/account/token-enabled.txt");
const TOKEN_UNKNOWN_LINE: &str =
    include_str!("../tests/fixtures/maintenance/account/token-unknown.txt");

/// A password no other byte of a test would spell.
const PASSWORD: &str = "Qv7-distinct-password-Zk3";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A source of the account's name that says what it is told.
struct Named(Result<Option<String>, String>);

impl AccountName for Named {
    fn name(&self) -> Result<Option<String>, String> {
        self.0.clone()
    }
}

/// The arguments of the read of the update account's user record.
fn user_args() -> Vec<String> {
    words(&[".", "-read", "/Users/hkupdate", "RecordName", "IsHidden"])
}

/// The arguments of the read of the admin group.
fn admin_args() -> Vec<String> {
    words(&[".", "-read", "/Groups/admin", "GroupMembership"])
}

/// The arguments of the Secure Token status of the update account.
fn token_args() -> Vec<String> {
    words(&["-secureTokenStatus", "hkupdate"])
}

/// What `sysadminctl` exits 0 with, `text` on its standard error.
fn on_stderr(text: &str) -> LaunchctlOutput {
    LaunchctlOutput {
        code: Some(0),
        stdout: Vec::new(),
        stderr: text.as_bytes().to_vec(),
    }
}

/// The two scripted programs.
struct Rig {
    dscl: FakeLaunchctl,
    sysadminctl: FakeLaunchctl,
}

impl Rig {
    /// Programs that answer a user record, the admin group and the token status.
    fn answering(user: &str, admins: &str, token: LaunchctlOutput) -> Rig {
        let rig = Rig {
            dscl: FakeLaunchctl::new(),
            sysadminctl: FakeLaunchctl::new(),
        };
        rig.dscl
            .always(&user_args(), Ok(LaunchctlOutput::success(user)));
        rig.dscl
            .always(&admin_args(), Ok(LaunchctlOutput::success(admins)));
        rig.sysadminctl.always(&token_args(), Ok(token));
        rig
    }

    /// Programs with nothing scripted: each call fails to start.
    fn silent() -> Rig {
        Rig {
            dscl: FakeLaunchctl::new(),
            sysadminctl: FakeLaunchctl::new(),
        }
    }

    fn collect_from(&self, name: Box<dyn AccountName>) -> Result<Section, String> {
        let own = BTreeSet::new();
        let look = Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        };
        UpdateAccountFacts::new(
            name,
            Box::new(self.dscl.clone()),
            Box::new(self.sysadminctl.clone()),
        )
        .collect(&look)
    }

    /// The collection for an account named `hkupdate`.
    fn collect(&self) -> Result<Section, String> {
        self.collect_from(Box::new(Named(Ok(Some("hkupdate".to_owned())))))
    }
}

/// The section a look of the update account gives.
fn section_of(
    present: bool,
    admin: bool,
    secure_token: bool,
    hidden: bool,
    unparsed: u32,
) -> Section {
    Section {
        mac: Some(pb::MacUpdateFacts {
            update_account: Some(pb::UpdateAccountState {
                present,
                admin,
                secure_token,
                hidden,
            }),
            ..Default::default()
        }),
        unparsed,
        ..Default::default()
    }
}

/// Proves the collector's name, which a failed look's reason starts with, and that the one over
/// the host's own file and programs is built.
#[test]
fn the_collector_is_named_update_account_and_is_built_over_the_host() {
    assert_eq!(UpdateAccountFacts::at_host().name(), "update-account");
}

/// Proves the host's source reads the file the design names, owned by root.
#[test]
fn the_host_source_is_the_root_owned_update_account_file() {
    let host = AccountFile::at_host();
    assert_eq!(
        host.path,
        PathBuf::from("/Library/Application Support/hostkeeper/update-account")
    );
    assert_eq!(host.owner, 0);
}

/// Proves an account that is there, an administrator and hidden, whose `sysadminctl` says on
/// standard error that it holds no Secure Token, is reported as exactly that, after exactly the
/// three reads.
#[test]
fn an_account_without_a_secure_token_is_reported_so() {
    let rig = Rig::answering(USER_HIDDEN, ADMIN_MEMBERS, on_stderr(TOKEN_DISABLED_LINE));
    assert_eq!(rig.collect(), Ok(section_of(true, true, false, true, 0)));
    assert_eq!(rig.dscl.calls(), vec![user_args(), admin_args()]);
    assert_eq!(rig.sysadminctl.calls(), vec![token_args()]);
}

/// Proves an account with a token, not hidden and not an administrator, is reported as exactly
/// that, the token line read from standard output as well.
#[test]
fn an_account_with_a_token_that_is_not_an_admin_or_hidden_is_reported_so() {
    let rig = Rig::answering(
        USER_VISIBLE,
        ADMIN_OTHERS,
        LaunchctlOutput::success(TOKEN_ENABLED_LINE),
    );
    assert_eq!(rig.collect(), Ok(section_of(true, false, true, false, 0)));
}

/// Proves `IsHidden: 1` hides the account and `IsHidden: 0` or no `IsHidden` does not.
#[test]
fn only_an_is_hidden_of_one_hides_the_account() {
    for (user, hidden) in [
        (USER_HIDDEN, true),
        (USER_NOT_HIDDEN, false),
        (USER_VISIBLE, false),
    ] {
        let rig = Rig::answering(user, ADMIN_MEMBERS, on_stderr(TOKEN_ENABLED_LINE));
        assert_eq!(
            rig.collect(),
            Ok(section_of(true, true, true, hidden, 0)),
            "{user}"
        );
    }
}

/// Proves a Mac with no update-account file reports the state all false and runs no program.
#[test]
fn a_mac_with_no_update_account_file_reports_all_false_and_runs_nothing() {
    let rig = Rig::silent();
    assert_eq!(
        rig.collect_from(Box::new(Named(Ok(None)))),
        Ok(section_of(false, false, false, false, 0))
    );
    assert_eq!(rig.dscl.calls(), Vec::<Vec<String>>::new());
    assert_eq!(rig.sysadminctl.calls(), Vec::<Vec<String>>::new());
}

/// Proves a source that cannot give the name fails the look with its reason, unchanged.
#[test]
fn a_source_that_fails_fails_the_collection_with_its_reason() {
    let rig = Rig::silent();
    assert_eq!(
        rig.collect_from(Box::new(Named(Err("the file is odd".to_owned())))),
        Err("the file is odd".to_owned())
    );
    assert_eq!(rig.dscl.calls(), Vec::<Vec<String>>::new());
}

/// Proves a `dscl` exit of 56 (no such record) reports the state all false and reads neither the
/// admin group nor the token status.
#[test]
fn an_account_dscl_does_not_know_is_absent_and_nothing_else_is_read() {
    let rig = Rig::silent();
    rig.dscl.always(
        &user_args(),
        Ok(LaunchctlOutput::exit(56, "hkupdate: record not found\n")),
    );
    assert_eq!(rig.collect(), Ok(section_of(false, false, false, false, 0)));
    assert_eq!(rig.dscl.calls(), vec![user_args()]);
    assert_eq!(rig.sysadminctl.calls(), Vec::<Vec<String>>::new());
}

/// Proves a status with neither the enabled nor the disabled line reads as no token and counts
/// one unparsed line.
#[test]
fn a_status_with_neither_line_is_no_token_and_one_unparsed_line() {
    let rig = Rig::answering(USER_HIDDEN, ADMIN_MEMBERS, on_stderr(TOKEN_UNKNOWN_LINE));
    assert_eq!(rig.collect(), Ok(section_of(true, true, false, true, 1)));
}

/// Proves a `dscl` exit other than 0 and 56 on the user record fails the look with the step and
/// the exit code alone, without the account's name or the standard error.
#[test]
fn a_failed_user_read_fails_with_the_exit_code_alone() {
    let rig = Rig::silent();
    rig.dscl.always(
        &user_args(),
        Ok(LaunchctlOutput::exit(64, "trouble with hkupdate\n")),
    );
    let error = rig.collect().expect_err("failed");
    assert_eq!(
        error,
        "dscl read of the update account failed with exit code 64"
    );
    assert!(!error.contains("hkupdate"));
    assert_eq!(rig.sysadminctl.calls(), Vec::<Vec<String>>::new());
}

/// Proves a user read that cannot start fails the look with the step and why, without the name.
#[test]
fn a_user_read_that_cannot_start_fails_without_the_name() {
    let rig = Rig::silent();
    let error = rig.collect().expect_err("failed");
    assert_eq!(
        error,
        "dscl read of the update account could not be run: the fake has no answer"
    );
    assert!(!error.contains("hkupdate"));
}

/// Proves a failing read of the admin group fails the look with the step, the exit code and the
/// first line of its standard error, and reads no token status.
#[test]
fn a_failed_admin_group_read_fails_the_look() {
    let rig = Rig::silent();
    rig.dscl
        .always(&user_args(), Ok(LaunchctlOutput::success(USER_HIDDEN)));
    rig.dscl.always(
        &admin_args(),
        Ok(LaunchctlOutput::exit(1, "group trouble\n")),
    );
    let error = rig.collect().expect_err("failed");
    assert_eq!(
        error,
        "dscl read of the admin group failed with exit code 1: group trouble"
    );
    assert!(!error.contains("hkupdate"));
    assert_eq!(rig.sysadminctl.calls(), Vec::<Vec<String>>::new());
}

/// Proves a `sysadminctl` exit other than 0 fails the look with the step and the exit code
/// alone, without the account's name or the standard error.
#[test]
fn a_failed_token_status_fails_with_the_exit_code_alone() {
    let rig = Rig::answering(
        USER_HIDDEN,
        ADMIN_MEMBERS,
        LaunchctlOutput::exit(1, "no hkupdate here\n"),
    );
    let error = rig.collect().expect_err("failed");
    assert_eq!(
        error,
        "sysadminctl -secureTokenStatus failed with exit code 1"
    );
    assert!(!error.contains("hkupdate"));
}

/// Proves a `sysadminctl` that cannot start fails the look with the step and why, without the
/// account's name.
#[test]
fn a_token_status_that_cannot_start_fails_without_the_name() {
    let rig = Rig::silent();
    rig.dscl
        .always(&user_args(), Ok(LaunchctlOutput::success(USER_HIDDEN)));
    rig.dscl
        .always(&admin_args(), Ok(LaunchctlOutput::success(ADMIN_MEMBERS)));
    let error = rig.collect().expect_err("failed");
    assert_eq!(
        error,
        "sysadminctl -secureTokenStatus could not be run: the fake has no answer"
    );
    assert!(!error.contains("hkupdate"));
}

/// The file `update-account` in `dir` holding the account `hkupdate` and [`PASSWORD`], with
/// `mode`, and the uid that owns it (the test's own).
fn account_file(dir: &Scratch, mode: u32) -> (PathBuf, u32) {
    let path = dir.path().join("update-account");
    fs::write(&path, format!("name=hkupdate\npassword={PASSWORD}\n")).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
    let owner = fs::metadata(dir.path()).expect("the directory").uid();
    (path, owner)
}

/// Proves a 0600 file of the owner gives the account's name.
#[test]
fn a_private_file_of_the_owner_gives_its_name() {
    let dir = scratch("account-file-good");
    let (path, owner) = account_file(&dir, 0o600);
    assert_eq!(
        AccountFile::new(path, owner).name(),
        Ok(Some("hkupdate".to_owned()))
    );
}

/// Proves a file that is not there gives no name and no error.
#[test]
fn a_missing_file_gives_no_name() {
    let dir = scratch("account-file-missing");
    let owner = fs::metadata(dir.path()).expect("the directory").uid();
    let path = dir.path().join("update-account");
    assert_eq!(AccountFile::new(path, owner).name(), Ok(None));
}

/// Proves a 0644 file is refused with the reader's text, which names the file and the mode and
/// holds no byte of the file.
#[test]
fn a_readable_file_is_refused_with_the_readers_text() {
    let dir = scratch("account-file-mode");
    let (path, owner) = account_file(&dir, 0o644);
    let error = AccountFile::new(path.clone(), owner)
        .name()
        .expect_err("refused");
    assert_eq!(
        error,
        format!(
            "the update-account file {} has mode 0644, not 0600",
            path.display()
        )
    );
    assert!(!error.contains(PASSWORD));
}

/// Proves the password of a real file is in no byte of the section's report and in no argument of
/// any call, while the collector still reports the state.
#[test]
fn the_password_is_in_no_report_byte_and_no_argument() {
    let dir = scratch("account-file-password");
    let (path, owner) = account_file(&dir, 0o600);
    let rig = Rig::answering(USER_HIDDEN, ADMIN_MEMBERS, on_stderr(TOKEN_ENABLED_LINE));
    let section = rig
        .collect_from(Box::new(AccountFile::new(path, owner)))
        .expect("collected");
    assert_eq!(section, section_of(true, true, true, true, 0));
    let bytes = section.mac.expect("the Mac's facts").encode_to_vec();
    let password = PASSWORD.as_bytes();
    assert!(!bytes.is_empty());
    assert!(
        !bytes
            .windows(password.len())
            .any(|window| window == password)
    );
    let calls: Vec<String> = rig
        .dscl
        .calls()
        .into_iter()
        .chain(rig.sysadminctl.calls())
        .flatten()
        .collect();
    assert_eq!(calls.len(), 11);
    assert!(calls.iter().all(|argument| !argument.contains(PASSWORD)));
}
