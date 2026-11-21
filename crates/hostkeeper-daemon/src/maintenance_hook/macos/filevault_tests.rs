//! The tests of the `macos` kind on a Mac with FileVault on, over the fake Mac of
//! `apply_tests.rs`: the readiness `cutover` checks (`fdesetup status`, `supportsauthrestart`,
//! `list`, the password), the install without `--restart` followed by `fdesetup authrestart
//! -inputplist`, and the refusals. Each outcome is asserted by equality of the whole result, each
//! run by equality of the whole list (its standard input byte for byte), the log by its lines and
//! the plan by its bytes, so the password and the property list can hide in none of them.

use super::*;
use crate::maintenance_hook::macos::apply::filevault::{
    AUTHRESTART_DEADLINE, REASON_AUTHRESTART_FAILED, REASON_FILEVAULT_NOT_READY, RestartPlist,
    STEP_AUTHRESTART,
};

/// `fdesetup list` naming the update account, and one naming only an administrator, an account
/// whose name starts with the update account's, and the update account's name with no uuid.
const LIST: &str = include_str!("../../../tests/fixtures/maintenance/mac/fdesetup_list.txt");
const LIST_WITHOUT: &str =
    include_str!("../../../tests/fixtures/maintenance/mac/fdesetup_list_without.txt");

/// The property list of the update account of `apply_tests.rs`, byte for byte: its password's
/// `"` escaped, its `$`, `\` and `=` as they are.
const PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Username</key>\n\
\t<string>hkupdate</string>\n\
\t<key>Password</key>\n\
\t<string>Up date=pa&quot;ss$HOME\\1</string>\n\
</dict>\n\
</plist>\n";

/// A Mac of macOS 26.6 whose `--list` answers `list`, with FileVault on, able to restart
/// authenticated, and the update account a FileVault user.
fn filevault_on(list: &str) -> Mac {
    let mac = Mac::on_26(list);
    mac.look(FDESETUP, &["status"], ok(FILEVAULT_ON));
    mac.look(FDESETUP, &["supportsauthrestart"], ok("true\n"));
    mac.look(FDESETUP, &["list"], ok(LIST));
    mac
}

/// The effects of the FileVault looks of a `cutover` on a Mac with FileVault on, in order.
fn looks_of_filevault() -> Vec<String> {
    strings(&[
        "run /usr/bin/fdesetup status",
        "run /usr/bin/fdesetup supportsauthrestart",
        "run /usr/bin/fdesetup list",
    ])
}

/// The install of the minor update macOS Tahoe 26.7 without `--restart`.
fn update_without_restart() -> Installed {
    Installed {
        program: PathBuf::from(SOFTWAREUPDATE),
        deadline: INSTALL_DEADLINE,
        step: STEP_UPDATE.to_owned(),
        args: strings(&[
            "--install",
            TAHOE_26_7,
            "--agree-to-license",
            "--user",
            ACCOUNT,
            "--stdinpass",
        ]),
        input: input(),
    }
}

/// The authenticated restart, `plist` on its standard input.
fn authrestart_with(plist: &str) -> Installed {
    Installed {
        program: PathBuf::from(FDESETUP),
        deadline: AUTHRESTART_DEADLINE,
        step: STEP_AUTHRESTART.to_owned(),
        args: strings(&["authrestart", "-inputplist"]),
        input: plist.as_bytes().to_vec(),
    }
}

/// Writes `password` as the update account's password; the file keeps its mode 0600.
fn password(files: &Files, password: &[u8]) {
    let bytes = [
        format!("name={ACCOUNT}\npassword=").as_bytes(),
        password,
        b"\n",
    ]
    .concat();
    fs::write(&files.account, bytes).expect("the account");
}

/// The detail of a Mac whose FileVault is not ready for `why`.
fn not_ready(why: &str) -> Result<(), KindError> {
    failure(
        REASON_FILEVAULT_NOT_READY,
        &format!("{why}, so the update is not installed"),
    )
}

/// Proves: with FileVault on, the `cutover` asks `fdesetup status`, `supportsauthrestart` and
/// `list` before it lists, keeps a plan with `"authrestart":true`, records the reboot under way,
/// and only then runs `softwareupdate --install` with exactly the plain path's arguments but
/// `--restart`, the password on its standard input, and then `fdesetup authrestart -inputplist`,
/// nothing else in its arguments, the exact property list on its standard input; the step ends
/// `reboot_under_way` without the host's rebooter; the two log lines name each step and its exit
/// code alone. A restart asked again in the same boot runs the same two programs again, from the
/// plan, with no look.
///
/// Catches: `--restart` on a Mac whose restart stops at the pre-boot login, the restart asked
/// before the install finished, the password or the property list in an argument, the log or the
/// plan, a property list whose bytes `fdesetup` would read as another password, or a second look
/// choosing anew in the same boot.
#[test]
fn with_filevault_on_the_install_runs_without_restart_then_fdesetup_authrestart_with_the_plist() {
    let files = Files::new("macos-filevault-on");
    let mac = filevault_on(LIST_MAJOR);
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(ended, restarting());
    let mut expected = looks_of_filevault();
    expected.extend(strings(&[
        "run /usr/bin/sw_vers",
        "run /usr/sbin/softwareupdate --list",
    ]));
    expected.push(format!("record {RUN} macos reboot_under_way {BOOT_A}"));
    expected.push(format!("install {SOFTWAREUPDATE}"));
    expected.push(format!("install {FDESETUP}"));
    assert_eq!(mac.effects(), expected);
    let first = [update_without_restart(), authrestart_with(PLIST)];
    assert_eq!(mac.installs(), first);
    assert_eq!(
        mac.lines(),
        [
            "softwareupdate --install: exited with code 0",
            "fdesetup authrestart: exited with code 0",
        ]
    );
    assert_eq!(
        files.plan(),
        format!(r#"{{"authrestart":true,"build":"25H62","label":"{TAHOE_26_7}","runId":"{RUN}"}}"#)
    );

    let again = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(again, restarting());
    assert_eq!(
        mac.effects(),
        [
            format!("install {SOFTWAREUPDATE}"),
            format!("install {FDESETUP}"),
        ]
    );
    assert_eq!(mac.installs(), [first.clone(), first].concat());
}

/// Proves: every character XML treats specially in a password (`&`, `<`, `>`, `"`, `'`) is
/// escaped in the property list, a tab is kept as it is, and the list's `Debug` prints none of
/// it.
///
/// Catches: a password that ends the `<string>` early or injects a key of its own into the
/// property list, or a list printed whole by a `{:?}`.
#[test]
fn the_plist_escapes_the_password_and_its_debug_withholds_it() {
    let files = Files::new("macos-filevault-escape");
    password(&files, b"a&b<c>d\"e'f\tg");
    let mac = filevault_on(LIST_MAJOR);
    let apply = mac.apply(&files);
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        Ok::<(), KindError>(())
    );
    assert_eq!(apply.install(&params(false)), Ok(()));
    let escaped = PLIST.replace(
        "Up date=pa&quot;ss$HOME\\1",
        "a&amp;b&lt;c&gt;d&quot;e&apos;f\tg",
    );
    assert_eq!(mac.installs()[1], authrestart_with(&escaped));
    let account = UpdateAccount::read(&files.account, me()).expect("the account");
    let plist = RestartPlist::of(&account).expect("a plist");
    assert_eq!(format!("{plist:?}"), "RestartPlist(withheld)");
}

/// Proves: an update account that `fdesetup list` does not name, exactly (only an administrator,
/// an account whose name starts with its own, its name on a line with no uuid), fails
/// `filevault_not_ready`, naming the account, after the three FileVault looks and before
/// `sw_vers`, any listing, any record, any plan or any install program; through the hook too.
///
/// Catches: an authenticated restart the account cannot authorise (the Mac would stop at the
/// pre-boot login), a name matched by its start, or the check made after the install.
#[test]
fn an_account_that_is_not_a_filevault_user_is_refused_before_any_install_program_runs() {
    let files = Files::new("macos-filevault-not-user");
    let mac = filevault_on(LIST_MAJOR);
    mac.look(FDESETUP, &["list"], ok(LIST_WITHOUT));
    let why = "FileVault is on and the update account hkupdate is not a FileVault user \
               (fdesetup list)";
    assert_eq!(
        mac.apply(&files).cutover(&params(false)).map(|_| ()),
        not_ready(why)
    );
    assert_eq!(mac.effects(), looks_of_filevault());
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(
        ended,
        failed_ending(
            REASON_FILEVAULT_NOT_READY,
            &format!("{why}, so the update is not installed")
        )
    );
    assert_eq!(mac.effects(), looks_of_filevault());
    assert_eq!(mac.installs(), Vec::new());
    assert!(!files.run_dir.join(PLAN_FILE).exists());
}

/// Proves: with FileVault on, `fdesetup supportsauthrestart` saying `false`, or anything but
/// `true`, fails `filevault_not_ready` before `fdesetup list` is asked; FileVault in no state
/// `fdesetup status` names fails it before `supportsauthrestart` is; each of the three FileVault
/// looks that fails (`status`, `supportsauthrestart`, `list`) fails `macos_look_failed`, naming
/// it, and asks nothing after it; none lists, keeps or installs anything.
///
/// Catches: an authenticated restart asked of a Mac that cannot make one, a Mac in an unknown
/// state read as on or off, or a failed look read as a FileVault user.
#[test]
fn a_mac_that_cannot_restart_authenticated_is_refused_before_anything_is_listed() {
    let files = Files::new("macos-filevault-unsupported");
    let mac = filevault_on(LIST_MAJOR);
    let apply = mac.apply(&files);
    let unsupported = "FileVault is on and fdesetup supportsauthrestart does not say true";
    for answer in ["false\n", "maybe\n"] {
        mac.look(FDESETUP, &["supportsauthrestart"], ok(answer));
        assert_eq!(
            apply.cutover(&params(false)).map(|_| ()),
            not_ready(unsupported)
        );
        assert_eq!(
            mac.effects(),
            [
                "run /usr/bin/fdesetup status",
                "run /usr/bin/fdesetup supportsauthrestart",
            ]
        );
    }
    mac.look(FDESETUP, &["status"], ok("something else\n"));
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        not_ready("FileVault is in no state fdesetup names (fdesetup status), neither on nor off")
    );
    assert_eq!(mac.effects(), ["run /usr/bin/fdesetup status"]);

    mac.look(FDESETUP, &["status"], Ok(LaunchctlOutput::exit(1, "busy")));
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_LOOK_FAILED,
            "fdesetup status failed with exit code 1: busy"
        )
    );
    assert_eq!(mac.effects(), ["run /usr/bin/fdesetup status"]);
    mac.look(FDESETUP, &["status"], ok(FILEVAULT_ON));
    mac.look(
        FDESETUP,
        &["supportsauthrestart"],
        Ok(LaunchctlOutput::exit(1, "busy")),
    );
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_LOOK_FAILED,
            "fdesetup supportsauthrestart failed with exit code 1: busy"
        )
    );
    assert_eq!(
        mac.effects(),
        [
            "run /usr/bin/fdesetup status",
            "run /usr/bin/fdesetup supportsauthrestart",
        ]
    );
    mac.look(FDESETUP, &["supportsauthrestart"], ok("true\n"));
    mac.look(
        FDESETUP,
        &["list"],
        Ok(LaunchctlOutput::exit(1, "not root")),
    );
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        failure(
            REASON_LOOK_FAILED,
            "fdesetup list failed with exit code 1: not root"
        )
    );
    assert_eq!(mac.effects(), looks_of_filevault());
    assert_eq!(mac.installs(), Vec::new());
    assert!(!files.run_dir.join(PLAN_FILE).exists());
}

/// Proves: with FileVault on and the order allowing a major upgrade Apple offers, the `cutover`
/// fails `filevault_not_ready`, naming the version, after the listings and before any plan is
/// kept or `startosinstall` runs.
///
/// Catches: `startosinstall` run on a Mac with FileVault on while it is not shown that it
/// restarts authenticated, so the Mac would wait at the pre-boot login.
#[test]
fn a_major_upgrade_with_filevault_on_is_refused() {
    let files = Files::new("macos-filevault-major");
    files.installer(APP_27, true);
    let mac = filevault_on(LIST_MAJOR);
    assert_eq!(
        mac.apply(&files).cutover(&params(true)).map(|_| ()),
        failure(
            REASON_FILEVAULT_NOT_READY,
            "FileVault is on and the order allows the major upgrade to macOS 27.1: that \
             startosinstall restarts a Mac with FileVault on authenticated is not shown, so the \
             upgrade is not installed"
        )
    );
    let mut expected = looks_of_filevault();
    expected.extend(strings(&[
        "run /usr/bin/sw_vers",
        "run /usr/sbin/softwareupdate --list",
        "run /usr/sbin/softwareupdate --list-full-installers",
    ]));
    assert_eq!(mac.effects(), expected);
    assert_eq!(mac.installs(), Vec::new());
    assert!(!files.run_dir.join(PLAN_FILE).exists());
}

/// Proves: an authenticated restart that exits 1, having echoed its standard input to both
/// streams, fails the step `filevault_authrestart_failed` with a detail naming the step and the
/// exit code alone, logged as that same text after the install's line; an install that fails
/// with FileVault on fails `macos_install_failed` and runs no authenticated restart.
///
/// Catches: the property list (and so the password) carried into the error, the report or the
/// log; a failed restart reported as under way; a restart asked after an install that failed.
#[test]
fn a_failing_authrestart_or_install_fails_naming_the_step_and_exit_code_alone() {
    let files = Files::new("macos-filevault-fails");
    let mac = filevault_on(LIST_MAJOR);
    let echoed = LaunchctlOutput {
        code: Some(1),
        stdout: PLIST.as_bytes().to_vec(),
        stderr: PLIST.as_bytes().to_vec(),
    };
    mac.script()
        .installed_by
        .insert(PathBuf::from(FDESETUP), Ok(echoed));
    let detail = "fdesetup authrestart: the program exited with code 1";
    let ended = mac.hook(&files, HookStep::Cutover, &params(false));
    assert_eq!(ended, failed_ending(REASON_AUTHRESTART_FAILED, detail));
    assert_eq!(
        ended.report().to_line(),
        format!(
            r#"{{"class":"release","detail":"{detail}","promotion_pending":false,"reason":"filevault_authrestart_failed"}}"#
        )
    );
    assert_eq!(
        mac.lines(),
        ["softwareupdate --install: exited with code 0", detail]
    );
    assert_eq!(mac.installs().len(), 2);

    let files = Files::new("macos-filevault-install-fails");
    let mac = filevault_on(LIST_MAJOR);
    mac.script().install = Ok(LaunchctlOutput::exit(1, "stopped"));
    let detail = "softwareupdate --install: the program exited with code 1";
    assert_eq!(
        mac.hook(&files, HookStep::Cutover, &params(false)),
        failed_ending(REASON_INSTALL_FAILED, detail)
    );
    assert_eq!(mac.installs(), [update_without_restart()]);
    assert_eq!(mac.lines(), [detail]);
}

/// Proves: with FileVault on, a password a property list cannot carry (a carriage return, which
/// XML reads as a line feed; a byte that is not UTF-8; U+FFFE or U+FFFF, which XML refuses)
/// fails `filevault_not_ready` at the
/// `cutover` after the FileVault looks, before `sw_vers`; and a password changed to one after
/// the plan was kept fails the restart the same way before `softwareupdate` runs. No detail holds
/// a byte of it.
///
/// Catches: a property list that would authorise with another password than the account's, an
/// update installed whose authenticated restart then cannot be asked, or the password quoted in
/// the refusal.
#[test]
fn a_password_the_plist_cannot_carry_is_refused_before_anything_is_installed() {
    let why = "FileVault is on and the update account's password is not UTF-8 or holds a \
               control character, which the property list of fdesetup authrestart cannot carry";
    for (name, bad) in [
        ("macos-filevault-password-cr", &b"pass\rword"[..]),
        ("macos-filevault-password-utf8", &b"pass\xffword"[..]),
        (
            "macos-filevault-password-fffe",
            "pass\u{FFFE}word".as_bytes(),
        ),
        (
            "macos-filevault-password-ffff",
            "pass\u{FFFF}word".as_bytes(),
        ),
    ] {
        let files = Files::new(name);
        password(&files, bad);
        let mac = filevault_on(LIST_MAJOR);
        assert_eq!(
            mac.apply(&files).cutover(&params(false)).map(|_| ()),
            not_ready(why)
        );
        assert_eq!(mac.effects(), looks_of_filevault());
        assert!(!files.run_dir.join(PLAN_FILE).exists());
    }

    let files = Files::new("macos-filevault-password-later");
    let mac = filevault_on(LIST_MAJOR);
    let apply = mac.apply(&files);
    assert_eq!(
        apply.cutover(&params(false)).map(|_| ()),
        Ok::<(), KindError>(())
    );
    password(&files, b"pass\rword");
    assert_eq!(apply.install(&params(false)), not_ready(why));
    assert_eq!(mac.installs(), Vec::new());
    assert_eq!(mac.lines(), Vec::<String>::new());
}
