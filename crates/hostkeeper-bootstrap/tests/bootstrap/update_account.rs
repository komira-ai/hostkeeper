//! The update account's four keys: how a source gives them, what refuses them, the update-account
//! file a run writes from them, and where neither password goes.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use hostkeeper_bootstrap::files::{FileError, Written};
use hostkeeper_bootstrap::run::{Error, Outcome, Paths};
use hostkeeper_bootstrap::settings::{Settings, SettingsError, Sources};
use hostkeeper_bootstrap::update_account::{Account, UPDATE_ACCOUNT_FILE, UpdateAccountKeys};
use hostkeeper_install::tools::Tool;

use crate::support::*;

/// The update account's password: a space, an `=`, a quote, a `$` and a backslash, which a shell,
/// an argument list or a `key=value` reader would treat specially.
const UPDATE_PASSWORD: &str = r#"Up date=pa"ss$1\x"#;
/// The administrator's password.
const ADMIN_PASSWORD: &str = "Adm1n pass=word";

/// The four keys as TOML, the passwords as literal strings (their bytes as they are).
fn keys_text() -> String {
    format!(
        "update_account_name = \"hkupdate\"\nupdate_account_password = '{UPDATE_PASSWORD}'\nadmin_name = \"admin\"\nadmin_password = '{ADMIN_PASSWORD}'\n"
    )
}

/// The update-account file of [`keys_text`], byte for byte.
fn file_bytes() -> String {
    format!("name=hkupdate\npassword={UPDATE_PASSWORD}\n")
}

fn no_wait() -> &'static str {
    "[bounds]\nwait_secs = 0\n"
}

/// What loading a source of `extra` is refused with, as text, and the source's path.
fn refused(name: &str, extra: &str) -> (String, PathBuf) {
    let dir = scratch(name);
    let paths = paths_in(dir.path(), extra);
    let error = paths.sources.load().expect_err("refused");
    (error.to_string(), paths.sources.fallback.clone())
}

/// Every file under `dir`, at any depth.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(&next).expect("a directory") {
            let path = entry.expect("an entry").path();
            match fs::symlink_metadata(&path).expect("metadata").is_dir() {
                true => todo.push(path),
                false => found.push(path),
            }
        }
    }
    found
}

/// The files under `dir` but `except` that hold `needle`.
fn holding(dir: &Path, except: &Path, needle: &str) -> Vec<PathBuf> {
    files_under(dir)
        .into_iter()
        .filter(|path| path != except)
        .filter(|path| {
            let bytes = fs::read(path).expect("a file");
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        })
        .collect()
}

/// Catches the file not being written, being written after `installer` (the daemon's first start,
/// in its postinstall, would not find it), or holding other bytes, another owner or another mode
/// than the daemon's reader takes.
#[test]
fn the_keys_make_the_update_account_file_before_the_installer_runs() {
    let dir = scratch("account-file");
    let paths = paths_in(dir.path(), &keys_text());
    let world = World::good();
    world
        .tools
        .watch
        .lock()
        .expect("watch")
        .push(paths.update_account.clone());
    let outcome = world.run(&paths).expect("installed");
    assert_eq!(
        outcome,
        Outcome::Installed {
            version: VERSION.into(),
            daemon_config: Written::Made,
            signers_file: Written::Made,
            update_account: Some(Written::Made),
        }
    );
    assert_eq!(
        fs::read(&paths.update_account).expect("the file"),
        file_bytes().as_bytes()
    );
    let meta = fs::metadata(&paths.update_account).expect("the file");
    assert_eq!(meta.uid(), paths.sources.trusted_uid);
    assert_eq!(meta.mode() & 0o7777, 0o600);
    assert!(meta.is_file());
    assert_eq!(
        *world.tools.at_installer.lock().expect("seen"),
        [vec![Some(file_bytes().into_bytes())]],
        "the file is there, whole, when installer runs"
    );
}

/// Catches a file written from settings that name no update account: such a Mac has none, as
/// before the keys existed.
#[test]
fn settings_without_the_keys_write_no_update_account_file() {
    let dir = scratch("account-none");
    let paths = paths_in(dir.path(), "");
    let outcome = World::good().run(&paths).expect("installed");
    assert!(matches!(
        outcome,
        Outcome::Installed {
            update_account: None,
            ..
        }
    ));
    assert!(!exists(&paths.update_account));
    assert!(
        paths
            .sources
            .load()
            .expect("settings")
            .update_account
            .is_none()
    );
}

/// Catches a source with some of the four keys being taken: each one left out is named, in the
/// order of the keys, and the error holds no value of the keys that were given.
#[test]
fn a_partial_set_of_keys_is_refused_naming_the_first_missing_key() {
    let lines: Vec<String> = keys_text().lines().map(str::to_owned).collect();
    let names = [
        "update_account_name",
        "update_account_password",
        "admin_name",
        "admin_password",
    ];
    for (left_out, name) in names.iter().enumerate() {
        let given: String = lines
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != left_out)
            .map(|(_, line)| format!("{line}\n"))
            .collect();
        let (text, path) = refused("account-partial", &given);
        assert_eq!(
            text,
            format!(
                "{} has no {name}: the update account's four keys are given together or not at all",
                path.display()
            )
        );
    }
    let (text, path) = refused(
        "account-one",
        &format!("admin_password = '{ADMIN_PASSWORD}'\n"),
    );
    assert_eq!(
        text,
        format!(
            "{} has no update_account_name: the update account's four keys are given together or not at all",
            path.display()
        )
    );
}

/// Catches a value that breaks its key's rule being taken, and an error quoting the value: each
/// is refused by its key's name and the rule, by exact equality.
#[test]
fn a_bad_value_is_refused_by_its_key_without_its_value() {
    let with = |key: &str, value: &str| {
        keys_text()
            .lines()
            .map(|line| match line.starts_with(&format!("{key} ")) {
                true => format!("{key} = {value}\n"),
                false => format!("{line}\n"),
            })
            .collect::<String>()
    };
    let name_rule =
        "is not an account name: ASCII letters, digits, `_`, `.` and `-`, not starting with `-`";
    let cases = [
        (
            with("update_account_name", "\"-hkupdate\""),
            "update_account_name",
            name_rule,
        ),
        (
            with("update_account_name", "\"hk update\""),
            "update_account_name",
            name_rule,
        ),
        (
            with("update_account_name", "\"\""),
            "update_account_name",
            name_rule,
        ),
        (with("admin_name", "\"ad/min\""), "admin_name", name_rule),
        (
            with("update_account_password", "''"),
            "update_account_password",
            "is empty",
        ),
        (with("admin_password", "''"), "admin_password", "is empty"),
        (
            with("update_account_password", "\"Secret\\nline\""),
            "update_account_password",
            "holds a line break, which a password line cannot hold",
        ),
        (
            with("admin_password", "\"Secret\\nline\""),
            "admin_password",
            "holds a line break, which a password line cannot hold",
        ),
        (
            with("update_account_password", "12345678"),
            "update_account_password",
            "is not a string",
        ),
        (
            with("admin_password", "12345678"),
            "admin_password",
            "is not a string",
        ),
        (
            with("admin_name", "[\"admin\"]"),
            "admin_name",
            "is not a string",
        ),
        (
            with("admin_name", "\"hkupdate\""),
            "admin_name",
            "is the update account's own name: the account is given its Secure Token by another administrator",
        ),
    ];
    for (extra, key, why) in cases {
        let (text, path) = refused("account-bad", &extra);
        assert_eq!(text, format!("{} {key} {why}", path.display()), "{extra}");
    }
}

/// Catches the bound of the daemon's reader (4096 bytes) not being kept: a file of exactly 4096
/// bytes is taken, one byte more is refused, naming the password's key.
#[test]
fn the_file_is_held_to_the_readers_bound() {
    // "name=hkupdate\n" is 14 bytes, "password=" 9 and the last newline 1: 24 besides the password.
    let at = |length: usize| {
        keys_text().replace(
            &format!("'{UPDATE_PASSWORD}'"),
            &format!("'{}'", "p".repeat(length)),
        )
    };
    let dir = scratch("account-bound");
    let paths = paths_in(dir.path(), &at(4096 - 24));
    let keys = paths.sources.load().expect("at the bound").update_account;
    assert_eq!(keys.expect("keys").file_text().len(), 4096);
    let (text, path) = refused("account-over", &at(4096 - 23));
    assert_eq!(
        text,
        format!(
            "{} update_account_password makes, with the name, an update-account file larger than 4096 bytes",
            path.display()
        )
    );
}

/// Catches the managed preferences not carrying the keys as the fallback file does, and a number
/// given for a password there being quoted in its error.
#[test]
fn the_managed_preferences_carry_the_keys() {
    let dir = scratch("account-managed");
    let sources = Sources {
        managed: dir.path().join("managed.plist"),
        fallback: dir.path().join("bootstrap.toml"),
        trusted_uid: fs::metadata(dir.path()).expect("dir").uid(),
    };
    let mut dict = plist::Dictionary::new();
    dict.insert("coordinator".into(), "https://managed.example".into());
    dict.insert("ca_sha256".into(), CA.into());
    dict.insert("update_account_name".into(), "hkupdate".into());
    dict.insert("update_account_password".into(), UPDATE_PASSWORD.into());
    dict.insert("admin_name".into(), "admin".into());
    dict.insert("admin_password".into(), ADMIN_PASSWORD.into());
    let write = |dict: &plist::Dictionary| {
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dict.clone())
            .to_writer_xml(&mut bytes)
            .expect("a plist");
        write_mode(
            &sources.managed,
            std::str::from_utf8(&bytes).expect("text"),
            0o600,
        );
    };
    write(&dict);
    let keys = sources
        .load()
        .expect("settings")
        .update_account
        .expect("keys");
    assert_eq!(keys.account.name, "hkupdate");
    assert_eq!(keys.account.password.expose(), UPDATE_PASSWORD);
    assert_eq!(keys.admin.name, "admin");
    assert_eq!(keys.admin.password.expose(), ADMIN_PASSWORD);
    dict.insert(
        "admin_password".into(),
        plist::Value::Integer(12_345_678.into()),
    );
    write(&dict);
    let error = sources.load().expect_err("a number");
    assert_eq!(
        error.to_string(),
        format!(
            "{} admin_password is not a string",
            sources.managed.display()
        )
    );
    assert!(matches!(
        error,
        SettingsError::Invalid {
            key: "admin_password",
            ..
        }
    ));
}

/// Catches a password in what a run prints: every log line of a run that installs and of one that
/// fails, the outcome, the error it gave up with and the debug print of the settings, by exact
/// equality.
#[test]
fn what_a_run_prints_holds_no_password() {
    let dir = scratch("account-printed");
    let paths = paths_in(dir.path(), &keys_text());
    let world = World::good();
    let outcome = world.run(&paths).expect("installed");
    assert_eq!(
        *world.log.borrow(),
        [format!("fetching hostkeeper {VERSION} from {PKG_URL}")]
    );
    assert_eq!(
        outcome.to_string(),
        "installed hostkeeper 1.2.3 (hostkeeper.toml Made, macos-pkg.toml Made, update-account Made)"
    );
    let dir = scratch("account-printed-failed");
    let paths = paths_in(dir.path(), &format!("{}{}", keys_text(), no_wait()));
    let world = World::good();
    *world.tools.installer_code.lock().expect("code") = Some(1);
    let gave_up = world.run(&paths).expect_err("failed");
    assert_eq!(
        *world.log.borrow(),
        [
            format!("fetching hostkeeper {VERSION} from {PKG_URL}"),
            "attempt 1 failed: installer exited Some(1): installer: the install failed".to_owned(),
        ]
    );
    assert_eq!(
        gave_up.to_string(),
        "gave up after 1 attempt(s); the last failed: installer exited Some(1): installer: the install failed"
    );
    let settings: Settings = paths.sources.load().expect("settings");
    assert_eq!(
        format!("{:?}", settings.update_account),
        "Some(UpdateAccountKeys { account: Account { name: \"hkupdate\", password: Password(withheld) }, admin: Account { name: \"admin\", password: Password(withheld) } })"
    );
}

/// Catches the administrator's password being written anywhere by a run, whether it installs or
/// fails: no file the run leaves but the settings source holds it.
#[test]
fn the_administrators_password_is_not_left_on_disk() {
    for installer_code in [Some(0), Some(1)] {
        let dir = scratch("account-admin");
        let paths = paths_in(dir.path(), &format!("{}{}", keys_text(), no_wait()));
        let world = World::good();
        *world.tools.installer_code.lock().expect("code") = installer_code;
        drop(world.run(&paths));
        assert_eq!(world.tools.ran(), [Tool::Pkgutil, Tool::Installer]);
        assert_eq!(
            holding(dir.path(), &paths.sources.fallback, ADMIN_PASSWORD),
            Vec::<PathBuf>::new(),
            "after installer exited {installer_code:?}"
        );
    }
}

/// Catches a second run rewriting a file that is right: the file, its bytes and its inode are
/// the ones it had, and a file of the right owner and mode is kept even when its bytes differ
/// (the account it names may already have that password).
#[test]
fn an_existing_private_file_is_kept_unchanged() {
    for existing in [
        file_bytes(),
        "name=hkupdate\npassword=an earlier one\n".to_owned(),
    ] {
        let dir = scratch("account-kept");
        let paths = paths_in(dir.path(), &keys_text());
        write_mode(&paths.update_account, &existing, 0o600);
        let before = fs::metadata(&paths.update_account).expect("the file");
        let outcome = World::good().run(&paths).expect("installed");
        assert!(matches!(
            outcome,
            Outcome::Installed {
                update_account: Some(Written::Kept),
                ..
            }
        ));
        let after = fs::metadata(&paths.update_account).expect("the file");
        assert_eq!(read(&paths.update_account), existing);
        assert_eq!((after.ino(), after.mode()), (before.ino(), before.mode()));
    }
}

/// Catches a file there with another mode being kept or replaced: the daemon would refuse it, so
/// the attempt fails naming it, the file is left as it is, `installer` does not run and the
/// configs this attempt made are removed.
#[test]
fn an_existing_file_of_another_mode_is_refused_and_left() {
    let dir = scratch("account-mode");
    let paths = paths_in(dir.path(), &format!("{}{}", keys_text(), no_wait()));
    write_mode(&paths.update_account, "name=other\n", 0o644);
    let world = World::good();
    let gave_up = world.run(&paths).expect_err("refused");
    assert_eq!(
        gave_up.last.to_string(),
        format!(
            "{} is owned by uid {uid} with mode 0644, not by uid {uid} with mode 0600; it is not replaced",
            paths.update_account.display(),
            uid = paths.sources.trusted_uid
        )
    );
    assert_eq!(read(&paths.update_account), "name=other\n");
    assert_eq!(mode(&paths.update_account), 0o644);
    assert_eq!(world.tools.ran(), [Tool::Pkgutil]);
    assert!(!exists(&paths.config_dir.join("hostkeeper.toml")));
    assert!(!exists(&paths.config_dir.join("macos-pkg.toml")));
}

/// Catches an update-account file that cannot be written leaving the configs the attempt made:
/// they are removed, so a corrected setting is read on the next attempt, and `installer` does not
/// run.
#[test]
fn an_unwritable_update_account_file_removes_the_configs_the_attempt_made() {
    let dir = scratch("account-dir");
    let paths = paths_in(dir.path(), &format!("{}{}", keys_text(), no_wait()));
    fs::create_dir_all(&paths.update_account).expect("a directory in its place");
    let world = World::good();
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(
        matches!(gave_up.last, Error::File(FileError::NotAFile { .. })),
        "{gave_up}"
    );
    assert_eq!(world.tools.ran(), [Tool::Pkgutil]);
    assert!(!exists(&paths.config_dir.join("hostkeeper.toml")));
    assert!(!exists(&paths.config_dir.join("macos-pkg.toml")));
}

/// Catches a failed install leaving the update-account file it made, which would hold a password
/// the next attempt's settings may have corrected; a file that was there before is left.
#[test]
fn a_failed_install_removes_the_update_account_file_it_made() {
    let dir = scratch("account-rollback");
    let paths = paths_in(dir.path(), &format!("{}{}", keys_text(), no_wait()));
    let world = World::good();
    *world.tools.installer_code.lock().expect("code") = Some(1);
    world.run(&paths).expect_err("failed");
    assert!(!exists(&paths.update_account));
    let dir = scratch("account-rollback-kept");
    let paths = paths_in(dir.path(), &format!("{}{}", keys_text(), no_wait()));
    write_mode(&paths.update_account, &file_bytes(), 0o600);
    let world = World::good();
    *world.tools.installer_spawn_fails.lock().expect("flag") = true;
    world.run(&paths).expect_err("failed");
    assert_eq!(read(&paths.update_account), file_bytes());
}

/// Catches the host's path not being the one the daemon reads, and the keys' types printing a
/// password.
#[test]
fn the_host_path_is_the_daemons_and_a_password_prints_as_withheld() {
    assert_eq!(
        Paths::host().update_account,
        PathBuf::from("/Library/Application Support/hostkeeper/update-account")
    );
    assert_eq!(
        UPDATE_ACCOUNT_FILE,
        "/Library/Application Support/hostkeeper/update-account"
    );
    let dir = scratch("account-debug");
    let paths = paths_in(dir.path(), &keys_text());
    let keys: UpdateAccountKeys = paths
        .sources
        .load()
        .expect("settings")
        .update_account
        .expect("keys");
    let Account { name, password } = keys.admin;
    assert_eq!(name, "admin");
    assert_eq!(format!("{password:#?}"), "Password(withheld)");
}
