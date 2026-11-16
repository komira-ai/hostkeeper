//! The tests of `ssh_files`: the record's checks, the bytes of each file, and the writes, removals
//! and restorations in a scratch directory (never a real `/etc/ssh`).

use std::os::unix::fs::PermissionsExt;

use hostkeeper_testkit::Scratch;

use super::*;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn paths_in(dir: &Scratch) -> SshPaths {
    SshPaths {
        config_dir: dir.path().join("sshd_config.d"),
        own_dir: dir.path().join("hostkeeper"),
    }
}

const ED: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

fn access() -> PushedSsh {
    PushedSsh {
        generation: 4,
        enabled: true,
        ca_public_keys: vec![format!("{ED} fleet-ca")],
        login_user: "root".to_owned(),
        principal: "hk-host-web-04".to_owned(),
        revoked_serials: vec![9, 3, 9],
        password_auth: "keep".to_owned(),
    }
}

/// Proves a good record is a plan with the key's comment dropped, the serials sorted and
/// deduplicated, and passwords off only for `off`.
#[test]
fn a_good_record_is_a_plan() {
    let plan = plan(&access()).expect("a plan");
    assert_eq!(plan.ca_keys, [ED]);
    assert_eq!(plan.login_user, "root");
    assert_eq!(plan.principal, "hk-host-web-04");
    assert_eq!(plan.serials, [3, 9]);
    assert!(!plan.passwords_off);
    let mut off = access();
    off.password_auth = "off".to_owned();
    assert!(super::plan(&off).expect("a plan").passwords_off);
    off.password_auth = "anything else".to_owned();
    assert!(!super::plan(&off).expect("a plan").passwords_off);
    let mut two = access();
    two.ca_public_keys.push("ecdsa-sha2-nistp256 AAAAE2VjZHNh".to_owned());
    assert_eq!(super::plan(&two).expect("a plan").ca_keys.len(), 2);
}

/// Proves a record that cannot be written safely is refused with the field named: no login user
/// (its own state), a login user or principal outside the alphabet or of the wrong shape, and CA
/// keys that are none, three, of another type, with an option or a character outside base64.
#[test]
fn a_record_that_cannot_be_written_safely_is_refused_with_its_field() {
    let refused = |change: &dyn Fn(&mut PushedSsh)| {
        let mut record = access();
        change(&mut record);
        plan(&record).expect_err("refused")
    };
    assert_eq!(
        refused(&|a| a.login_user.clear()),
        Refused::new(SshState::NoLoginUser, "login_user")
    );
    let long = "u".repeat(33);
    for user in ["-root", "ro ot", "a/b", "root\nx", long.as_str()] {
        assert_eq!(
            refused(&|a| a.login_user = user.to_owned()),
            Refused::new(SshState::ConfigInvalid, "login_user"),
            "{user:?}"
        );
    }
    for principal in ["", "hk-host-", "root", "hk-host-a b", "hk-host-a\nb", "hk-host--x", "hk-host-../x"] {
        assert_eq!(
            refused(&|a| a.principal = principal.to_owned()),
            Refused::new(SshState::ConfigInvalid, "principal"),
            "{principal:?}"
        );
    }
    let keys = Refused::new(SshState::ConfigInvalid, "ca_public_keys");
    assert_eq!(refused(&|a| a.ca_public_keys.clear()), keys);
    assert_eq!(refused(&|a| a.ca_public_keys = vec![ED.to_owned(); 3]), keys);
    assert_eq!(refused(&|a| a.ca_public_keys = vec!["ssh-dss AAAA".to_owned()]), keys);
    assert_eq!(
        refused(&|a| a.ca_public_keys = vec![format!("command=\"x\" {ED}")]),
        keys
    );
    assert_eq!(refused(&|a| a.ca_public_keys = vec!["ssh-ed25519 AA;AA".to_owned()]), keys);
    assert_eq!(refused(&|a| a.ca_public_keys = vec!["ssh-ed25519".to_owned()]), keys);
    assert_eq!(refused(&|a| a.ca_public_keys = vec![String::new()]), keys);
}

/// Proves the drop-in is exactly the three trust keywords, with the password keywords added only
/// when passwords are turned off, and no `Match` block.
#[test]
fn the_drop_in_sets_the_three_trust_keywords_and_passwords_only_on_request() {
    let dir = scratch("ssh-render");
    let paths = paths_in(&dir);
    let own = paths.own_dir.display();
    let keep = String::from_utf8(render_drop_in(&paths, false)).expect("text");
    assert_eq!(
        keep,
        format!(
            "# Written by hostkeeper (ssh-v1); the coordinator's record replaces it. Do not edit.\n\
             TrustedUserCAKeys {own}/user_ca.pub\n\
             AuthorizedPrincipalsFile {own}/principals.%u\n\
             RevokedKeys {own}/revoked\n"
        )
    );
    let off = String::from_utf8(render_drop_in(&paths, true)).expect("text");
    assert_eq!(off, keep + "PasswordAuthentication no\nKbdInteractiveAuthentication no\n");
    assert!(!off.contains("Match"));
    assert_eq!(
        String::from_utf8(render_ca_keys(&["a b".to_owned(), "c d".to_owned()])).expect("text"),
        "a b\nc d\n"
    );
}

/// Proves where each file goes, the wire name of every state, and that the wanted files are the
/// drop-in, the CA keys and the login user's principals file, with the revoked list only when one
/// is given.
#[test]
fn the_wanted_files_and_the_names_of_the_states() {
    let paths = SshPaths::linux();
    assert_eq!(paths.drop_in(), Path::new("/etc/ssh/sshd_config.d/10-hostkeeper.conf"));
    assert_eq!(paths.ca_keys(), Path::new("/etc/ssh/hostkeeper/user_ca.pub"));
    assert_eq!(paths.revoked(), Path::new("/etc/ssh/hostkeeper/revoked"));
    assert_eq!(paths.principals("root"), Path::new("/etc/ssh/hostkeeper/principals.root"));
    assert_eq!(paths.candidate(), Path::new("/etc/ssh/hostkeeper/candidate.conf"));
    assert_eq!(paths.principals_template(), "/etc/ssh/hostkeeper/principals.%u");

    let plan = plan(&access()).expect("a plan");
    let without = wanted(&paths, &plan, None);
    assert_eq!(without.files.len(), 3);
    assert_eq!(without.files[2], (paths.principals("root"), b"hk-host-web-04\n".to_vec()));
    let with = wanted(&paths, &plan, Some(b"list".to_vec()));
    assert_eq!(with.files[3], (paths.revoked(), b"list".to_vec()));

    let names: Vec<&str> = [
        SshState::Applied,
        SshState::Disabled,
        SshState::BlockedByAllowList,
        SshState::SshdMissing,
        SshState::ConfigInvalid,
        SshState::ReloadFailed,
        SshState::WouldLockOut,
        SshState::NoLoginUser,
        SshState::CertLoginNotPossible,
        SshState::ShadowedByStock,
        SshState::Unsupported,
    ]
    .iter()
    .map(|state| state.as_str())
    .collect();
    assert_eq!(
        names,
        [
            "applied",
            "disabled",
            "blocked_by_allow_list",
            "sshd_missing",
            "config_invalid",
            "reload_failed",
            "would_lock_out",
            "no_login_user",
            "cert_login_not_possible",
            "shadowed_by_stock",
            "unsupported"
        ]
    );
}

fn installed(dir: &Scratch) -> (SshPaths, Wanted) {
    let paths = paths_in(dir);
    let plan = plan(&access()).expect("a plan");
    let files = wanted(&paths, &plan, Some(b"list".to_vec()));
    (paths, files)
}

/// Proves installing writes every file with mode 0644 and makes the directories, says it changed
/// something, says nothing changed the second time, and `current` follows the bytes on disk.
#[test]
fn install_writes_the_files_once() {
    let dir = scratch("ssh-install");
    let (paths, files) = installed(&dir);
    assert!(install(&paths, &files, me()).expect("installed"));
    for (path, bytes) in &files.files {
        assert_eq!(&fs::read(path).expect("a file"), bytes);
        assert_eq!(fs::metadata(path).expect("metadata").permissions().mode() & 0o7777, 0o644);
    }
    assert!(!install(&paths, &files, me()).expect("installed"));
    let without_list = wanted(&paths, &plan(&access()).expect("a plan"), None);
    assert!(current(&paths, &without_list));
    fs::write(paths.ca_keys(), "other\n").expect("a change");
    assert!(!current(&paths, &without_list));
    install(&paths, &files, me()).expect("installed");
    fs::remove_file(paths.revoked()).expect("removed");
    assert!(!current(&paths, &without_list));
}

/// Proves installing removes the principals file of any other account, which this setup did not
/// write for the login user, and leaves the login user's.
#[test]
fn install_removes_the_principals_of_other_accounts() {
    let dir = scratch("ssh-stale");
    let (paths, files) = installed(&dir);
    fs::create_dir_all(&paths.own_dir).expect("a directory");
    fs::write(paths.principals("alice"), "hk-host-other\n").expect("a stale file");
    fs::write(paths.own_dir.join("notes"), "mine").expect("an unrelated file");
    assert!(install(&paths, &files, me()).expect("installed"));
    assert!(!paths.principals("alice").exists());
    assert!(paths.principals("root").exists());
    assert!(paths.own_dir.join("notes").exists(), "only principals.* files are removed");
    assert!(!install(&paths, &files, me()).expect("installed"));
}

/// Proves an install that cannot write a file (something that is not a file is at its path) or
/// remove a stale one is an error that names the path.
#[test]
fn an_install_that_fails_names_the_path() {
    let dir = scratch("ssh-install-fail");
    let (paths, files) = installed(&dir);
    fs::create_dir_all(paths.drop_in()).expect("a directory in the way");
    let why = install(&paths, &files, me()).expect_err("not written");
    assert!(why.ends_with("10-hostkeeper.conf: not a file"), "{why}");

    let other = scratch("ssh-install-stale");
    let (paths, files) = installed(&other);
    fs::create_dir_all(paths.principals("alice").join("inside")).expect("a directory named like a file");
    let why = install(&paths, &files, me()).expect_err("not removed");
    assert!(why.contains("principals.alice"), "{why}");

    let blocked = scratch("ssh-install-io");
    let (paths, files) = installed(&blocked);
    fs::write(&paths.own_dir, "a file where the directory goes").expect("a file");
    let why = install(&paths, &files, me()).expect_err("no directory");
    assert!(why.contains("hostkeeper"), "{why}");
}

/// Proves a snapshot puts back what was there: a file's old bytes, no file where there was none,
/// and no principals file the run made.
#[test]
fn a_snapshot_puts_the_files_back() {
    let dir = scratch("ssh-snapshot");
    let (paths, files) = installed(&dir);
    install(&paths, &files, me()).expect("installed");
    fs::write(paths.principals("alice"), "hk-host-old\n").expect("an old file");
    let snapshot = Snapshot::of(&paths);
    fs::write(paths.ca_keys(), "changed\n").expect("a change");
    fs::remove_file(paths.revoked()).expect("a removal");
    fs::remove_file(paths.principals("alice")).expect("a removal");
    fs::write(paths.principals("bob"), "hk-host-new\n").expect("a new file");
    snapshot.restore(me(), &paths).expect("restored");
    assert_eq!(fs::read(paths.ca_keys()).expect("a file"), render_ca_keys(&plan(&access()).expect("a plan").ca_keys));
    assert_eq!(fs::read(paths.revoked()).expect("a file"), b"list");
    assert_eq!(fs::read(paths.principals("alice")).expect("a file"), b"hk-host-old\n");
    assert!(!paths.principals("bob").exists());

    let empty = scratch("ssh-snapshot-empty");
    let paths = paths_in(&empty);
    let snapshot = Snapshot::of(&paths);
    install(&paths, &installed(&empty).1, me()).expect("installed");
    snapshot.restore(me(), &paths).expect("restored");
    assert!(!paths.drop_in().exists() && !paths.ca_keys().exists() && !paths.principals("root").exists());
}

/// Proves a file that cannot be put back, and a run's file that cannot be removed, are named in
/// the error, and the others are still put back.
#[test]
fn a_restore_that_fails_names_the_paths_and_restores_the_rest() {
    let dir = scratch("ssh-restore-fail");
    let (paths, files) = installed(&dir);
    install(&paths, &files, me()).expect("installed");
    let snapshot = Snapshot::of(&paths);
    fs::remove_file(paths.ca_keys()).expect("a removal");
    fs::create_dir_all(paths.ca_keys()).expect("a directory in the way");
    fs::remove_file(paths.drop_in()).expect("a removal");
    fs::create_dir_all(paths.principals("bob").join("inside")).expect("a directory the run made");
    let why = snapshot.restore(me(), &paths).expect_err("not restored");
    assert!(why.starts_with("not restored: "), "{why}");
    assert!(why.contains("user_ca.pub") && why.contains("principals.bob"), "{why}");
    assert!(paths.drop_in().is_file(), "the others are put back");
}

/// Proves uninstalling removes every file of the setup, the principals files of every account
/// and a candidate left behind, removes the directory when it is then empty and not otherwise,
/// and says whether anything was there.
#[test]
fn uninstall_removes_every_file_of_the_setup() {
    let dir = scratch("ssh-uninstall");
    let (paths, files) = installed(&dir);
    install(&paths, &files, me()).expect("installed");
    fs::write(paths.principals("alice"), "x\n").expect("a file");
    fs::write(paths.candidate(), "x\n").expect("a file");
    assert!(uninstall(&paths).expect("removed"));
    assert!(!paths.own_dir.exists() && !paths.drop_in().exists());
    assert!(paths.config_dir.exists(), "sshd's own directory stays");
    assert!(!uninstall(&paths).expect("nothing"));

    install(&paths, &files, me()).expect("installed");
    fs::write(paths.own_dir.join("notes"), "mine").expect("an unrelated file");
    assert!(uninstall(&paths).expect("removed"));
    assert!(paths.own_dir.join("notes").exists(), "a directory with a file of someone else's stays");
}

/// Proves an uninstall that cannot remove a path says which.
#[test]
fn an_uninstall_that_fails_names_the_path() {
    let dir = scratch("ssh-uninstall-fail");
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.drop_in().join("inside")).expect("a directory in the way");
    let why = uninstall(&paths).expect_err("not removed");
    assert!(why.contains("10-hostkeeper.conf"), "{why}");
}
