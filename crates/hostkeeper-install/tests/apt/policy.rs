//! The `policy-rc.d` file held while the package manager runs maintainer scripts
//! (packages-native.md section 10.2): present with its exact text during the install and during
//! every try of the purge, inside the installer lock, and gone afterwards however the run ended.

use hostkeeper_install::repository::policy::{Guard, POLICY_RC_D, TEXT, clear_stale};
use hostkeeper_install::repository::{Class, Outcome, Reason};

use crate::apt_fake::Call;
use crate::harness::*;

use Class::Release;
use Outcome::Failed;

fn policy_path(env: &Env) -> std::path::PathBuf {
    env.dirs.root.join(POLICY_RC_D)
}

fn ours(call: &Call) -> bool {
    call.policy == Some((TEXT.to_owned(), 0o755))
}

/// Proves the file's text is exactly the contract: a shell script that answers 101.
#[test]
fn the_policy_text_answers_101() {
    assert_eq!(
        TEXT,
        "#!/bin/sh\n# Written by hostkeeper while it runs the package manager; removed when it ends.\nexit 101\n"
    );
}

/// Proves the file is there with its exact text and mode 0755 during the `apt-get install` of an
/// activate, with the installer lock held, and absent before and after it.
#[test]
fn the_policy_file_is_present_during_the_install_and_gone_after() {
    let env = Env::new("apt-policy-install");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    assert!(!policy_path(&env).exists());
    env.installer().activate(&order, &dir).expect("activates");
    let installs = env.host.calls_of("apt-get install");
    assert_eq!(installs.len(), 1);
    assert!(ours(&installs[0]), "{:?}", installs[0].policy);
    assert!(installs[0].locked);
    for call in env.host.calls() {
        if call.label != "apt-get install" {
            assert_eq!(call.policy, None, "{} saw the policy file", call.label);
        }
    }
    assert!(!policy_path(&env).exists());
}

/// Proves the file is gone after an install that fails (a dpkg error from a maintainer script)
/// and after a rollback activate.
#[test]
fn the_policy_file_is_gone_after_a_failed_install() {
    let env = Env::new("apt-policy-failed");
    let (o1, d1) = env.installed(&deb(V1), &["mdm.service"]);
    let (o2, d2) = env.staged(&deb(V2), &["mdm.service"]);
    env.host
        .edit(|s| s.faults.script_fails_for = Some(V2.to_owned()));
    let failure = env.installer().activate(&o2, &d2).expect_err("fails");
    assert_failure(&failure, Reason::DpkgError, Release, Failed);
    assert!(ours(&env.host.calls_of("apt-get install")[1]));
    assert!(!policy_path(&env).exists());
    env.installer().activate(&o1, &d1).expect("rolls back");
    assert!(ours(&env.host.calls_of("apt-get install")[2]));
    assert!(!policy_path(&env).exists());
}

/// Proves the file is present during each `dpkg --purge` try of a busy remove, and gone after
/// the remove succeeds.
#[test]
fn the_policy_file_is_present_during_each_purge_try_and_gone_after() {
    let env = Env::new("apt-policy-purge");
    env.installed(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| {
        s.faults.busy.insert("purge", 2);
    });
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    let purges = env.host.calls_of("dpkg --purge");
    assert_eq!(purges.len(), 3);
    for purge in &purges {
        assert!(ours(purge), "{:?}", purge.policy);
        assert!(purge.locked);
    }
    assert!(!policy_path(&env).exists());
}

/// Proves the file is gone after a purge that stays busy and after one a dependent package
/// refuses, and the package is held again.
#[test]
fn the_policy_file_is_gone_after_a_busy_or_refused_purge() {
    let env = Env::new("apt-policy-busy");
    env.installed(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| {
        s.faults.busy.insert("purge", 99);
    });
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("busy");
    assert!(env.host.calls_of("dpkg --purge").iter().all(ours));
    assert!(!policy_path(&env).exists());

    let env = Env::new("apt-policy-refused");
    env.installed(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| s.faults.purge_refused = true);
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("refused");
    assert!(ours(&env.host.calls_of("dpkg --purge")[0]));
    assert!(!policy_path(&env).exists());
}

/// Proves an operator's own `policy-rc.d` is left byte for byte, during the install and the
/// purge, and not removed afterwards.
#[test]
fn an_operators_policy_file_is_left_alone() {
    let env = Env::new("apt-policy-operator");
    let theirs = "#!/bin/sh\nexit 0\n";
    std::fs::create_dir_all(policy_path(&env).parent().expect("a parent")).expect("a dir");
    std::fs::write(policy_path(&env), theirs).expect("a file");
    env.installed(&deb(V1), &["mdm.service"]);
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    for label in ["apt-get install", "dpkg --purge"] {
        for call in env.host.calls_of(label) {
            assert_eq!(call.policy.expect("a file").0, theirs, "{label}");
        }
    }
    assert_eq!(
        std::fs::read_to_string(policy_path(&env)).expect("kept"),
        theirs
    );
}

/// Proves a file of exactly our text left by a killed run is reused for the install and removed
/// at its end.
#[test]
fn a_stale_policy_file_of_ours_is_reused_and_removed() {
    let env = Env::new("apt-policy-stale");
    std::fs::create_dir_all(policy_path(&env).parent().expect("a parent")).expect("a dir");
    std::fs::write(policy_path(&env), TEXT).expect("a file");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.installer().activate(&order, &dir).expect("activates");
    assert_eq!(
        env.host.calls_of("apt-get install")[0]
            .policy
            .as_ref()
            .map(|p| p.0.as_str()),
        Some(TEXT)
    );
    assert!(!policy_path(&env).exists());
}

/// Proves `clear_stale` removes a file of our text and answers true, keeps an operator's file
/// and answers false, and answers false when there is no file or a directory is there.
#[test]
fn clear_stale_removes_only_ours() {
    let env = Env::new("apt-policy-clear");
    let root = &env.dirs.root;
    assert!(!clear_stale(root).expect("nothing there"));
    std::fs::create_dir_all(policy_path(&env).parent().expect("a parent")).expect("a dir");
    std::fs::write(policy_path(&env), "#!/bin/sh\nexit 0\n").expect("a file");
    assert!(!clear_stale(root).expect("foreign"));
    assert!(policy_path(&env).exists());
    std::fs::remove_file(policy_path(&env)).expect("removed");
    std::fs::create_dir(policy_path(&env)).expect("a directory");
    assert!(!clear_stale(root).expect("a directory"));
    std::fs::remove_dir(policy_path(&env)).expect("removed");
    std::fs::write(policy_path(&env), TEXT).expect("ours");
    assert!(clear_stale(root).expect("ours"));
    assert!(!policy_path(&env).exists());
}

/// Proves the guard writes the file with its parent directories, and that a guard over a
/// directory or a link at the path takes nothing away and writes nothing; a root that cannot
/// hold the file is an error.
#[test]
fn the_guard_makes_its_directories_and_leaves_what_is_not_ours() {
    let env = Env::new("apt-policy-guard");
    let root = &env.dirs.root;
    let guard = Guard::hold(root).expect("held");
    assert_eq!(
        std::fs::read_to_string(policy_path(&env)).expect("written"),
        TEXT
    );
    drop(guard);
    assert!(!policy_path(&env).exists());

    std::os::unix::fs::symlink("/bin/true", policy_path(&env)).expect("a link");
    drop(Guard::hold(root).expect("held"));
    assert!(policy_path(&env).is_symlink());
    std::fs::remove_file(policy_path(&env)).expect("removed");

    std::fs::write(root.join("blocked"), "x").expect("a file");
    Guard::hold(&root.join("blocked")).expect_err("a file is not a root");
}
