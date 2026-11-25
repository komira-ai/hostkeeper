//! The tests of `update_account`: the file in a scratch directory, owned by the test's own user
//! (the owner the reader is told to want, or told not to), and the update call over this test
//! binary as its child, or over the scripted runner. No real update-account file is read and no
//! real update program is run.

use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use hostkeeper_launchd::{FakeStdinRun, StdinProcess};
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_testkit::Scratch;

use super::*;

/// A password with bytes an argument list, a shell or a log line would treat specially, and an
/// `=` (a value is the rest of its line after the first one).
const PASSWORD: &[u8] = b"Pa ss=w\"rd$1\\\x01";

/// A good file: the account `hkupdate` and [`PASSWORD`].
fn good() -> Vec<u8> {
    let mut text = b"name=hkupdate\npassword=".to_vec();
    text.extend_from_slice(PASSWORD);
    text.push(b'\n');
    text
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The file `update-account` in `dir`, holding `bytes`, with `mode`.
fn file(dir: &Scratch, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.path().join("update-account");
    fs::write(&path, bytes).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
    path
}

/// What reading `bytes` (mode 0600, the test's own user) is refused with, as text.
fn refused(bytes: &[u8]) -> String {
    let dir = scratch("update-account-refused");
    let path = file(&dir, bytes, 0o600);
    let error = UpdateAccount::read(&path, me()).expect_err("refused");
    let text = error.to_string();
    let prefix = format!("the update-account file {} ", path.display());
    text.strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("{text:?} does not start with {prefix:?}"))
        .to_owned()
}

/// Catches a good file not reading back exactly: the name, and the password byte for byte (an
/// `=` in it kept, nothing trimmed).
#[test]
fn a_good_file_reads_back() {
    let dir = scratch("update-account-good");
    let path = file(&dir, &good(), 0o600);
    let account = UpdateAccount::read(&path, me()).expect("read");
    assert_eq!(account.name(), "hkupdate");
    assert_eq!(account.password.0, PASSWORD);
}

/// Catches the order of the lines, a blank line or a missing last newline mattering.
#[test]
fn the_lines_may_come_in_either_order_with_blank_lines_and_no_last_newline() {
    let dir = scratch("update-account-order");
    let path = file(&dir, b"\npassword=pw\n\nname=up.date_1-x", 0o600);
    let account = UpdateAccount::read(&path, me()).expect("read");
    assert_eq!(account.name(), "up.date_1-x");
    assert_eq!(account.password.0, b"pw");
}

/// Catches a debug print of the account holding a byte of the password.
#[test]
fn a_debug_print_of_the_account_withholds_the_password() {
    let dir = scratch("update-account-debug");
    let path = file(&dir, &good(), 0o600);
    let account = UpdateAccount::read(&path, me()).expect("read");
    assert_eq!(
        format!("{account:?}"),
        "UpdateAccount { name: \"hkupdate\", password: Password(withheld) }"
    );
    assert_eq!(format!("{:#?}", account.password), "Password(withheld)");
}

/// Catches a file of another owner being read: the error names the owner found and the one
/// wanted, and holds nothing of the file, in its text and in its debug print.
#[test]
fn a_file_of_another_owner_is_refused() {
    let dir = scratch("update-account-owner");
    let path = file(&dir, &good(), 0o600);
    let other = me().wrapping_add(1);
    let error = UpdateAccount::read(&path, other).expect_err("another owner");
    assert_eq!(
        error,
        UpdateAccountError {
            path: path.clone(),
            problem: Problem::Owner {
                found: me(),
                wanted: other
            }
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "the update-account file {} is owned by uid {}, not uid {other}",
            path.display(),
            me()
        )
    );
    assert_eq!(
        format!("{error:?}"),
        format!(
            "UpdateAccountError {{ path: {path:?}, problem: Owner {{ found: {}, wanted: {other} }} }}",
            me()
        )
    );
}

/// Catches a mode other than 0600 being read: one others or the group may read, one its owner
/// may not write, one with more bits for its owner, and 0600 with the setuid bit.
#[test]
fn a_file_of_another_mode_is_refused() {
    for (mode, shown) in [
        (0o644, "0644"),
        (0o640, "0640"),
        (0o400, "0400"),
        (0o700, "0700"),
        (0o4600, "4600"),
    ] {
        let dir = scratch("update-account-mode");
        let path = file(&dir, &good(), mode);
        let error = UpdateAccount::read(&path, me()).expect_err("another mode");
        assert_eq!(
            error.to_string(),
            format!(
                "the update-account file {} has mode {shown}, not 0600",
                path.display()
            ),
            "mode {mode:o}"
        );
    }
}

/// Catches a link being followed: a link to a good file is refused as a link.
#[test]
fn a_symbolic_link_is_refused() {
    let dir = scratch("update-account-link");
    let target = file(&dir, &good(), 0o600);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("the link");
    let error = UpdateAccount::read(&link, me()).expect_err("a link");
    assert_eq!(
        error.to_string(),
        format!(
            "the update-account file {} is a symbolic link",
            link.display()
        )
    );
}

/// Catches what is not a regular file being read, or a pipe holding the read: a directory and a
/// named pipe, each of the test's own user.
#[test]
fn a_directory_or_a_pipe_is_refused() {
    let dir = scratch("update-account-kinds");
    let directory = dir.path().join("directory");
    fs::create_dir(&directory).expect("the directory");
    let pipe = dir.path().join("pipe");
    hostkeeper_testkit::fifo(&pipe, 0o600).expect("the pipe");
    for path in [directory, pipe] {
        let error = UpdateAccount::read(&path, me()).expect_err("not a file");
        assert_eq!(error.problem, Problem::NotRegular, "{}", path.display());
        assert_eq!(
            error.to_string(),
            format!(
                "the update-account file {} is not a regular file",
                path.display()
            )
        );
    }
}

/// Catches a missing file, or one that cannot be opened, being taken for another problem.
#[test]
fn a_missing_or_unopenable_file_is_named_so() {
    let dir = scratch("update-account-missing");
    let missing = dir.path().join("update-account");
    assert_eq!(
        UpdateAccount::read(&missing, me()).map(|_| ()),
        Err(UpdateAccountError {
            path: missing.clone(),
            problem: Problem::Missing
        })
    );
    let under_a_file = file(&dir, b"", 0o600).join("update-account");
    let error = UpdateAccount::read(&under_a_file, me()).expect_err("not a directory");
    assert_eq!(
        error.to_string(),
        format!(
            "the update-account file {} could not be read: not a directory",
            under_a_file.display()
        )
    );
}

/// Catches the size cap being off by one: a good file of exactly [`MAX_BYTES`] reads, one byte
/// more is refused.
#[test]
fn a_file_of_the_cap_reads_and_one_byte_more_is_refused() {
    let head = b"name=hkupdate\npassword=";
    let mut text = head.to_vec();
    text.resize(usize::try_from(MAX_BYTES).expect("small"), b'p');
    let dir = scratch("update-account-cap");
    let path = file(&dir, &text, 0o600);
    let account = UpdateAccount::read(&path, me()).expect("at the cap");
    assert_eq!(account.password.0.len() + head.len(), text.len());
    text.push(b'p');
    assert_eq!(refused(&text), "is larger than 4096 bytes");
}

/// Catches a malformed file being read, or its error holding a byte of it: each fault of a line
/// is named by its number alone (the line that is not `key=value` here is the password itself),
/// and a file with no name or no password is refused.
#[test]
fn a_malformed_file_is_refused_by_line_number_alone() {
    let cases: [(&[u8], &str); 11] = [
        (b"", "has no name line"),
        (b"\n\n", "has no name line"),
        (b"name=hkupdate\n", "has no password line"),
        (b"password=Pa ss\n", "has no name line"),
        (b"name=hkupdate\nPa ss\n", "line 2 is not key=value"),
        (
            b"name=hkupdate\nPassword=Pa ss\n",
            "line 2 has a key other than name and password",
        ),
        (b"name=a\nname=b\npassword=Pa ss\n", "line 2 repeats a key"),
        (
            b"password=Pa ss\nname=a\npassword=Pa ss\n",
            "line 3 repeats a key",
        ),
        (
            b"name=-rf\npassword=Pa ss\n",
            "line 1 has a name that is not an account name",
        ),
        (
            b"name=a b\npassword=Pa ss\n",
            "line 1 has a name that is not an account name",
        ),
        (
            b"name=hkupdate\npassword=\n",
            "line 2 has an empty password",
        ),
    ];
    for (bytes, why) in cases {
        assert_eq!(refused(bytes), why, "{:?}", String::from_utf8_lossy(bytes));
    }
    assert_eq!(
        refused(b"name=\npassword=Pa ss\n"),
        "line 1 has a name that is not an account name"
    );
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

/// Catches nothing itself: the child of `the_password_reaches_the_update_call_on_its_standard_input_alone`.
#[test]
#[ignore = "the child process of the_password_reaches_the_update_call_on_its_standard_input_alone; run only by it"]
fn child_records_its_arguments_environment_and_input() {
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    let _ = std::io::stderr().write_all(&record(&args, &own_environment(), &stdin));
    std::process::exit(0);
}

/// Catches nothing itself: the child of `a_failing_update_call_names_its_step_and_exit_code_alone`.
#[test]
#[ignore = "the child process of a_failing_update_call_names_its_step_and_exit_code_alone; run only by it"]
fn child_echoes_its_input_to_both_streams_and_exits_three() {
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let _ = std::io::stdout().write_all(&stdin);
    let _ = std::io::stderr().write_all(&stdin);
    std::process::exit(3);
}

/// The arguments that run the helper test `name` of this module in this binary.
fn helper(name: &str) -> Vec<String> {
    vec![
        format!("update_account::tests::{name}"),
        "--exact".to_owned(),
        "--ignored".to_owned(),
        "--nocapture".to_owned(),
    ]
}

/// The update call over this test binary.
fn child_runner() -> StdinProcess {
    let exe = std::env::current_exe().expect("the test binary");
    StdinProcess::new(&exe, Duration::from_secs(60))
}

fn read_good(dir: &Scratch) -> UpdateAccount {
    UpdateAccount::read(&file(dir, &good(), 0o600), me()).expect("read")
}

/// Catches the password going anywhere but the update call's standard input: the child's
/// arguments are exactly those given, its environment is exactly empty, its standard input is
/// exactly the password and a newline, and the one log line is the step and the exit code.
#[test]
fn the_password_reaches_the_update_call_on_its_standard_input_alone() {
    let dir = scratch("update-account-call");
    let account = read_good(&dir);
    let log = RecordingLog::default();
    let args = helper("child_records_its_arguments_environment_and_input");
    let out = account
        .run(&child_runner(), "softwareupdate", &args, &log)
        .expect("the call ran");
    let given: Vec<Vec<u8>> = args.iter().map(|arg| arg.as_bytes().to_vec()).collect();
    let mut stdin = PASSWORD.to_vec();
    stdin.push(b'\n');
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&record(&given, &[], &stdin))
    );
    assert_eq!(log.lines(), vec!["softwareupdate: exited with code 0"]);
}

/// Catches a failing update call's error, its debug print or its log line holding a byte of the
/// password: the child echoes the password to both its streams and exits 3, and each text is the
/// step and the exit code alone.
#[test]
fn a_failing_update_call_names_its_step_and_exit_code_alone() {
    let dir = scratch("update-account-fail");
    let account = read_good(&dir);
    let log = RecordingLog::default();
    let args = helper("child_echoes_its_input_to_both_streams_and_exits_three");
    let result = account.run(&child_runner(), "softwareupdate", &args, &log);
    assert_eq!(
        format!("{result:?}"),
        "Err(Exit { step: \"softwareupdate\", code: 3 })"
    );
    let error = result.expect_err("exit code 3");
    assert_eq!(
        error.to_string(),
        "softwareupdate: the program exited with code 3"
    );
    assert_eq!(
        log.lines(),
        vec!["softwareupdate: the program exited with code 3"]
    );
}

/// Catches a run error of the update call (a program that cannot start, a deadline) not being
/// logged, or logged with more than the error's own text, which names the step alone.
#[test]
fn a_run_error_of_the_update_call_is_logged_as_its_own_text() {
    let dir = scratch("update-account-run-error");
    let account = read_good(&dir);
    let fake = FakeStdinRun::new();
    let args = vec!["--install".to_owned()];
    fake.then(
        &args,
        Err(StdinError::TimedOut {
            step: "softwareupdate".to_owned(),
        }),
    );
    let log = RecordingLog::default();
    let error = account
        .run(&fake, "softwareupdate", &args, &log)
        .expect_err("timed out");
    assert_eq!(
        log.lines(),
        vec!["softwareupdate: the program did not finish within its deadline and was killed"]
    );
    assert_eq!(log.lines(), vec![error.to_string()]);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, args);
    assert_eq!(calls[0].input, [PASSWORD, &b"\n"[..]].concat());
}
