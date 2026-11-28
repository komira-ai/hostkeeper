//! The files a first install lands on that no package owns (packages-lifecycle.md section 6.5):
//! kept by the first install's `activate`, put back by `remove` once the package is purged,
//! and never kept for a file a package owns, for an update, or when nothing was there; and
//! `dpkg-query --search`'s answers as the backend reads them.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use hostkeeper_install::kept::{INDEX, Kept, Lost};
use hostkeeper_install::repository::apt::parse;
use hostkeeper_install::repository::{Order, Reason, ToolOutput};

use crate::apt_fake::Deb;
use crate::harness::*;

/// The config package's file an operator may have placed by hand.
const FILE: &str = "/etc/mdm/mdm.toml";
/// Its hook program.
const HOOK: &str = "/usr/lib/mdm/config-hook";

/// A config package (hostgroups.md section 6.1): a file under `/etc/mdm/` and a hook program.
fn config_deb(version: &str) -> Deb {
    Deb::new(PACKAGE, version, "amd64")
        .file(
            FILE.trim_start_matches('/'),
            0o644,
            &format!("version = {version:?}\n"),
        )
        .file(HOOK.trim_start_matches('/'), 0o755, "#!/bin/sh\nexit 0\n")
}

/// A host whose installs write their package's files under its root.
fn env(name: &str) -> Env {
    let env = Env::new(name);
    env.host.place_files(&env.dirs.root);
    env
}

/// The kept files' store of the test's host.
fn kept(env: &Env) -> Kept {
    Kept::new(&env.dirs.kept, &env.dirs.root)
}

/// Writes the operator's file at `path` with `mode`.
fn hand_place(env: &Env, path: &str, text: &str, mode: u32) {
    let at = at_root(env, path);
    fs::create_dir_all(at.parent().expect("a parent")).expect("the parent");
    fs::write(&at, text).expect("the hand-placed file");
    fs::set_permissions(&at, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// The bytes, mode, owner and group at `path`.
fn seen(env: &Env, path: &str) -> (String, u32, u32, u32) {
    let at = at_root(env, path);
    let meta = fs::symlink_metadata(&at).expect("a file at the path");
    (
        fs::read_to_string(&at).expect("its bytes"),
        meta.mode() & 0o7777,
        meta.uid(),
        meta.gid(),
    )
}

/// The order of `deb` with the config package's shape: no `run` unit, a hook.
fn config_order(env: &Env, deb: &Deb) -> (Order, std::path::PathBuf) {
    let mut order = env.publish(deb);
    order.run = Vec::new();
    order.hook = Some(HOOK.to_owned());
    let (staged, dir) = env.stage(&order);
    staged.expect("the version stages");
    (order, dir)
}

/// The paths `dpkg-query --search` was asked about, each run's.
fn searched(env: &Env) -> Vec<Vec<String>> {
    env.host
        .calls_of("dpkg-query")
        .into_iter()
        .filter(|call| call.run.args.first().is_some_and(|a| a == "--search"))
        .map(|call| call.run.args[1..].to_vec())
        .collect()
}

/// Proves the case the store is for: a file nobody owns at a payload path is kept by the first
/// install, which replaces it with the package's, and is back with its bytes, mode, owner and
/// group once `remove` has purged the package; the copy is gone after, and only the paths the
/// host held were asked about.
///
/// Catches: a first install whose removal leaves the operator's file deleted.
#[test]
fn a_file_nobody_owns_is_kept_at_a_first_install_and_put_back_at_its_removal() {
    let env = env("apt-kept-roundtrip");
    hand_place(&env, FILE, "hand = true\n", 0o640);
    let before = seen(&env, FILE);
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.installer().activate(&order, &dir).expect("activates");
    assert_eq!(seen(&env, FILE).0, format!("version = {V1:?}\n"));
    assert_eq!(searched(&env), [vec![FILE.to_owned()]]);
    let set = kept(&env).set_of(&service(SERVICE)).expect("the set");
    let set = set.expect("a set is kept");
    assert_eq!(set.package, PACKAGE);
    assert_eq!(set.files.len(), 1);
    assert_eq!(set.files[0].path, FILE);

    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removes");
    assert_eq!(removed.lost, Vec::new());
    assert_eq!(env.host.installed(PACKAGE), None);
    assert_eq!(seen(&env, FILE), before);
    assert!(
        !at_root(&env, HOOK).exists(),
        "the package's own file is gone"
    );
    assert_eq!(kept(&env).set_of(&service(SERVICE)).expect("read"), None);
}

/// Proves (c): a file another package owns is not kept, and the install is refused as dpkg
/// refuses it today (`dpkg_error`), with the file left as it was and nothing kept.
///
/// Catches: a keep that copies another package's file and later writes it back.
#[test]
fn a_file_another_package_owns_is_not_kept_and_the_install_is_refused_as_today() {
    let env = env("apt-kept-owned");
    hand_place(&env, FILE, "other's\n", 0o644);
    env.host.edit(|s| {
        s.owners.insert(FILE.to_owned(), "other-config".to_owned());
    });
    let (order, dir) = config_order(&env, &config_deb(V1));
    let failure = env.installer().activate(&order, &dir).expect_err("refused");
    assert_eq!(failure.reason(), Reason::DpkgError, "{failure}");
    assert_eq!(env.host.installed(PACKAGE), None, "dpkg installed nothing");
    assert_eq!(searched(&env), [vec![FILE.to_owned()]]);
    assert_eq!(seen(&env, FILE).0, "other's\n");
    assert!(!env.dirs.kept.exists(), "nothing was kept");
}

/// Proves (d): a first install with no file at any payload path asks nothing, keeps nothing,
/// and its removal leaves each path empty, as before the store.
#[test]
fn a_path_with_no_file_before_the_first_install_is_empty_after_its_removal() {
    let env = env("apt-kept-empty");
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.installer().activate(&order, &dir).expect("activates");
    assert!(at_root(&env, FILE).exists());
    assert_eq!(searched(&env), Vec::<Vec<String>>::new());
    assert!(!env.dirs.kept.exists(), "nothing was kept");

    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removes");
    assert_eq!(removed.lost, Vec::new());
    assert!(!at_root(&env, FILE).exists());
    assert!(!at_root(&env, HOOK).exists());
}

/// Proves an update neither keeps nor drops: a file placed by hand at a path the next version
/// adds is not kept, no ownership is asked, and the first install's set stays to be put back at
/// the removal.
#[test]
fn an_update_keeps_the_first_installs_set_and_keeps_nothing_new() {
    let env = env("apt-kept-update");
    hand_place(&env, FILE, "hand = true\n", 0o600);
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.installer().activate(&order, &dir).expect("activates");
    let set = kept(&env).set_of(&service(SERVICE)).expect("the set");

    let added = "/etc/mdm/extra.toml";
    hand_place(&env, added, "hand extra\n", 0o600);
    let next = config_deb(V2).file(added.trim_start_matches('/'), 0o644, "package extra\n");
    let (order, dir) = config_order(&env, &next);
    env.installer().activate(&order, &dir).expect("updates");
    assert_eq!(searched(&env).len(), 1, "the update asked nothing");
    assert_eq!(kept(&env).set_of(&service(SERVICE)).expect("read"), set);

    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removes");
    assert_eq!(seen(&env, FILE).0, "hand = true\n");
    assert!(
        !at_root(&env, added).exists(),
        "the update's file was the package's"
    );
}

/// Proves a first install whose ownership query fails is the host's failure (`io_error`) before
/// anything is installed or recorded.
#[test]
fn a_first_install_whose_ownership_query_fails_installs_nothing() {
    let env = env("apt-kept-query-fails");
    hand_place(&env, FILE, "hand\n", 0o600);
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.host.answer(
        "dpkg-query",
        0,
        Ok(ToolOutput {
            code: Some(2),
            stderr: b"dpkg-query: error: cannot read the database\n".to_vec(),
            ..ToolOutput::default()
        }),
    );
    let failure = env.installer().activate(&order, &dir).expect_err("fails");
    assert_eq!(failure.reason(), Reason::IoError, "{failure}");
    assert_eq!(
        failure.detail(),
        "dpkg-query --search: dpkg-query: error: cannot read the database"
    );
    assert_eq!(env.host.installed(PACKAGE), None);
    assert_eq!(env.records.get(SERVICE), None);
    assert!(!env.has_run("apt-get install"));
}

/// Proves a first install whose copy cannot be kept fails `io_error` before the install, and
/// the operator's file is untouched.
#[test]
fn a_first_install_that_cannot_keep_its_copy_installs_nothing() {
    let env = env("apt-kept-store-fails");
    hand_place(&env, FILE, "hand\n", 0o600);
    fs::write(&env.dirs.kept, "a file where the store goes").expect("the obstacle");
    let (order, dir) = config_order(&env, &config_deb(V1));
    let failure = env.installer().activate(&order, &dir).expect_err("fails");
    assert_eq!(failure.reason(), Reason::IoError, "{failure}");
    assert!(
        failure.detail().starts_with("the kept files of mdm: "),
        "{failure}"
    );
    assert_eq!(seen(&env, FILE).0, "hand\n");
    assert!(!env.has_run("apt-get install"));
}

/// Proves a removal whose kept copy is gone completes (the package purged, the record dropped),
/// leaves the path empty and names it in `lost`.
#[test]
fn a_removal_whose_copy_is_gone_completes_and_names_the_path() {
    let env = env("apt-kept-copy-gone");
    hand_place(&env, FILE, "hand\n", 0o600);
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.installer().activate(&order, &dir).expect("activates");
    let set = kept(&env).dir_of(&service(SERVICE));
    fs::remove_file(set.join("0")).expect("the copy goes");

    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("the removal completes");
    assert_eq!(
        removed.lost,
        [Lost {
            path: FILE.to_owned(),
            why: "the kept copy: No such file or directory (os error 2)".to_owned(),
            copy: None,
        }]
    );
    assert_eq!(env.host.installed(PACKAGE), None);
    assert_eq!(env.records.get(SERVICE), None);
    assert!(!at_root(&env, FILE).exists());
    assert!(!set.exists(), "the set is dropped");
}

/// Proves a removal cut short after the put-back (its record's drop failed) leaves the set, and
/// its next run puts the file back again and drops it.
#[test]
fn a_removal_run_again_after_its_record_drop_failed_puts_the_file_back_again() {
    let env = env("apt-kept-rerun");
    hand_place(&env, FILE, "hand\n", 0o600);
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.installer().activate(&order, &dir).expect("activates");
    env.records.faults(|f| f.clear = true);
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("the record's drop fails");
    assert_eq!(seen(&env, FILE).0, "hand\n");
    assert!(
        kept(&env)
            .set_of(&service(SERVICE))
            .expect("read")
            .is_some()
    );

    fs::write(at_root(&env, FILE), "changed meanwhile\n").expect("a change");
    env.records.faults(|f| f.clear = false);
    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removes");
    assert_eq!(removed.lost, Vec::new());
    assert_eq!(seen(&env, FILE).0, "hand\n");
    assert_eq!(kept(&env).set_of(&service(SERVICE)).expect("read"), None);
}

/// Proves a removal with no record (the install never ran, or a removal already put the files
/// back and dropped the record) puts nothing back and drops the set.
#[test]
fn a_removal_with_no_record_drops_the_set_and_puts_nothing_back() {
    let env = env("apt-kept-no-record");
    hand_place(&env, FILE, "hand\n", 0o600);
    kept(&env)
        .keep(&service(SERVICE), PACKAGE, &[FILE.to_owned()])
        .expect("kept");
    fs::write(at_root(&env, FILE), "the operator's edit\n").expect("an edit");

    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removes");
    assert_eq!(removed.lost, Vec::new());
    assert_eq!(seen(&env, FILE).0, "the operator's edit\n");
    assert!(!kept(&env).dir_of(&service(SERVICE)).join(INDEX).exists());
    assert!(!env.has_run("dpkg --purge"));
}

/// Proves an `activate` run again after its first try failed (a pending record) keeps nothing
/// again: the first try's set, of the operator's file, is the one put back.
#[test]
fn an_activate_run_again_after_a_failed_try_keeps_the_first_tries_set() {
    let env = env("apt-kept-retry");
    hand_place(&env, FILE, "hand\n", 0o600);
    let (order, dir) = config_order(&env, &config_deb(V1));
    env.host
        .edit(|s| s.faults.script_fails_for = Some(V1.to_owned()));
    env.installer()
        .activate(&order, &dir)
        .expect_err("the script fails");
    env.host.edit(|s| s.faults.script_fails_for = None);
    env.installer().activate(&order, &dir).expect("activates");
    assert_eq!(searched(&env).len(), 1, "the second try asked nothing");

    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removes");
    assert_eq!(seen(&env, FILE).0, "hand\n");
}

fn output(code: Option<i32>, stdout: &str, stderr: &str, cut: bool) -> ToolOutput {
    ToolOutput {
        code,
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
        cut,
    }
}

fn asked() -> Vec<String> {
    [
        "/etc/a",
        "/usr/lib/x86_64-linux-gnu/libc.so.6",
        "/etc/b",
        "/etc/c",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Proves `dpkg-query --search`'s lines are read for the asked paths alone: a package list, a
/// multiarch name, a diversion; a path it did not find (exit 1) and one it was not asked about
/// are not owned.
#[test]
fn the_search_answer_names_the_owned_paths_of_those_asked() {
    let stdout = "mdm, other: /etc/a\nlibc6:amd64: /usr/lib/x86_64-linux-gnu/libc.so.6\n\
                  diversion by local-tool from: /etc/b\ndiversion by local-tool to: \
                  /etc/b.distrib\nunasked: /etc/z\n";
    let stderr = "dpkg-query: no path found matching pattern /etc/c\n";
    let owned = parse::owned(&output(Some(1), stdout, stderr, false), &asked()).expect("read");
    let want: BTreeSet<String> = ["/etc/a", "/etc/b", "/usr/lib/x86_64-linux-gnu/libc.so.6"]
        .map(str::to_owned)
        .into_iter()
        .collect();
    assert_eq!(owned, want);
    let all = parse::owned(&output(Some(0), "p: /etc/c\n", "", false), &asked()).expect("read");
    assert_eq!(all, ["/etc/c".to_owned()].into_iter().collect());
}

/// Proves a search that failed, was stopped at its bound, or was cut short is the host's
/// failure: an answer that may miss an owner is never read as "nobody owns it".
#[test]
fn a_search_that_failed_stopped_or_was_cut_is_io_error() {
    let cases = [
        (
            output(Some(2), "", "dpkg-query: error: x\n", false),
            "dpkg-query --search: dpkg-query: error: x",
        ),
        (
            output(Some(0), "p: /etc/a\n", "", true),
            "dpkg-query --search: p: /etc/a",
        ),
        (
            output(None, "", "", false),
            "dpkg-query --search was stopped at its bound: ",
        ),
    ];
    for (answer, detail) in cases {
        let failure = parse::owned(&answer, &asked()).expect_err("fails");
        assert_eq!(failure.reason(), Reason::IoError);
        assert_eq!(failure.detail(), detail);
    }
}
