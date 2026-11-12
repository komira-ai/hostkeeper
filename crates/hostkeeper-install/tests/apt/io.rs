//! The host's own failures: a tool that cannot run, a records port that fails, a work directory
//! that cannot be made, files that cannot be written. Each is `io_error`, failed, environment.

use hostkeeper_install::repository::{Class, Reason, ToolOutput};

use crate::apt_fake::Download;
use crate::harness::*;

fn assert_io(failure: &hostkeeper_install::repository::Failure) {
    assert_failure(
        failure,
        Reason::IoError,
        Class::Environment,
        hostkeeper_install::repository::Outcome::Failed,
    );
}

/// Proves a tool that cannot be run at all fails the stage with `io_error`, naming the tool,
/// whichever of the stage's seven tools it is, and the host is unchanged.
#[test]
fn a_tool_that_cannot_run_fails_the_stage() {
    let labels = [
        "apt-get update",
        "dpkg --print-architecture",
        "apt-cache show",
        "apt-get simulate",
        "apt-get download",
        "dpkg-deb --field",
        "dpkg-deb --extract",
    ];
    for label in labels {
        let env = Env::new("apt-cannot-run");
        let order = env.publish(&deb(V1));
        let before = env.snapshot();
        env.host.cannot_run(label);
        let (result, _) = env.stage(&order);
        let failure = result.expect_err(label);
        assert_io(&failure);
        assert!(
            failure.detail().contains("could not be run"),
            "{label}: {failure}"
        );
        env.assert_unchanged(&before);
    }
}

/// Proves a tool that cannot be run fails the activate and the remove with `io_error` at each
/// of their steps.
#[test]
fn a_tool_that_cannot_run_fails_the_activate_and_the_remove() {
    for (label, skip) in [
        ("dpkg-query", 0),
        ("apt-get install", 0),
        ("dpkg-query", 1),
        ("apt-mark hold", 0),
        ("systemctl daemon-reload", 0),
    ] {
        let env = Env::new("apt-activate-cannot-run");
        let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
        env.host.answer(
            label,
            skip,
            Err(hostkeeper_install::repository::tools::ToolError(
                "gone".to_owned(),
            )),
        );
        let failure = env.installer().activate(&order, &dir).expect_err(label);
        assert_io(&failure);
    }
    for label in [
        "dpkg-query",
        "apt-mark unhold",
        "dpkg --purge",
        "systemctl daemon-reload",
    ] {
        let env = Env::new("apt-remove-cannot-run");
        env.installed(&deb(V1), &["mdm.service"]);
        env.host.cannot_run(label);
        let failure = env
            .installer()
            .remove(&service(SERVICE), REMOVE_SECS)
            .expect_err(label);
        assert_io(&failure);
        assert!(env.records.get(SERVICE).is_some());
    }
    let env = Env::new("apt-drift-cannot-run");
    env.host.cannot_run("dpkg-query");
    let failure = env
        .installer()
        .drift(&service(SERVICE), PACKAGE, 5)
        .expect_err("fails");
    assert_io(&failure);
}

/// Proves tools that answer in a way the installer cannot use are `io_error` too: an architecture
/// that is not a name, an `apt-cache show` that fails otherwise, a dpkg-query that fails
/// otherwise, an `apt-mark` that fails otherwise.
#[test]
fn an_answer_the_installer_cannot_use_is_an_io_error() {
    let env = Env::new("apt-odd-answers");
    let order = env.publish(&deb(V1));
    env.host.answer(
        "dpkg --print-architecture",
        0,
        Ok(ToolOutput::exit(0, "AMD 64\n")),
    );
    let (result, _) = env.stage(&order);
    assert_io(&result.expect_err("a bad architecture"));

    let env = Env::new("apt-empty-arch");
    let order = env.publish(&deb(V1));
    env.host
        .answer("dpkg --print-architecture", 0, Ok(ToolOutput::exit(0, "")));
    let (result, _) = env.stage(&order);
    assert_io(&result.expect_err("an empty architecture"));

    let env = Env::new("apt-show-fails");
    let order = env.publish(&deb(V1));
    env.host
        .answer("apt-cache show", 0, Ok(ToolOutput::exit(1, "")));
    let (result, _) = env.stage(&order);
    assert_io(&result.expect_err("show fails"));

    let env = Env::new("apt-query-fails");
    env.host
        .answer("dpkg-query", 0, Ok(ToolOutput::exit(2, "")));
    let failure = env
        .installer()
        .drift(&service(SERVICE), PACKAGE, 5)
        .expect_err("fails");
    assert_io(&failure);

    let env = Env::new("apt-hold-fails");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.host
        .answer("apt-mark hold", 0, Ok(ToolOutput::exit(100, "")));
    let failure = env
        .installer()
        .activate(&order, &dir)
        .expect_err("hold fails");
    assert_io(&failure);
}

/// Proves a package file dpkg-deb cannot read is `manifest_invalid`: the field read and the
/// extraction exit non-zero.
#[test]
fn a_package_dpkg_deb_cannot_read_is_manifest_invalid() {
    for label in ["dpkg-deb --extract", "dpkg-deb --field"] {
        let env = Env::new("apt-unreadable-deb");
        let order = env.publish(&deb(V1));
        env.host.answer(label, 0, Ok(ToolOutput::exit(2, "")));
        let (result, _) = env.stage(&order);
        assert_eq!(
            result.expect_err(label).reason(),
            Reason::ManifestInvalid,
            "{label}"
        );
    }
}

/// Proves a download that leaves no package file or two is `fetch_failed`, and one that leaves a
/// file the daemon cannot read, or no archives directory, is `io_error`.
#[test]
fn a_download_that_leaves_no_usable_file_fails() {
    let cases = [
        (
            Download::NoFile,
            Reason::FetchFailed,
            "left 0 package files",
        ),
        (
            Download::TwoFiles,
            Reason::FetchFailed,
            "left 2 package files",
        ),
        (
            Download::Unreadable,
            Reason::IoError,
            "the downloaded package",
        ),
        (
            Download::NoArchivesDir,
            Reason::IoError,
            "the archives directory",
        ),
    ];
    for (mode, reason, said) in cases {
        let env = Env::new("apt-bad-download");
        let order = env.publish(&deb(V1));
        env.host.edit(|s| s.faults.download = mode);
        let (result, _) = env.stage(&order);
        let failure = result.expect_err("fails");
        assert_eq!(failure.reason(), reason, "{mode:?}: {failure}");
        assert!(failure.detail().contains(said), "{mode:?}: {failure}");
    }
}

/// Proves an update that leaves no `lists/partial/`, whose owner and mode the archives directory
/// takes before the download, fails the stage with `io_error` naming the archives directory, and
/// nothing is downloaded.
#[test]
fn a_download_with_no_lists_partial_to_copy_is_an_io_error() {
    let env = Env::new("apt-no-partial");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.no_lists_partial = true);
    let (result, _) = env.stage(&order);
    let failure = result.expect_err("fails");
    assert_io(&failure);
    assert!(
        failure
            .detail()
            .starts_with("the archives directory could not be given apt's download owner: "),
        "{failure}"
    );
    assert!(env.host.calls_of("apt-get download").is_empty());
}

/// Proves a records port that fails is `io_error`: the holder at the stage's start (before any
/// tool), the load at the activate's start, the first save (before the install) and the remove's
/// load and clear.
#[test]
fn a_records_port_that_fails_is_an_io_error() {
    let env = Env::new("apt-records-holder");
    let order = env.publish(&deb(V1));
    env.records.faults(|f| f.holder = true);
    let (result, _) = env.stage(&order);
    let failure = result.expect_err("holder fails");
    assert_io(&failure);
    assert!(failure.detail().starts_with("the installed record"));
    assert!(env.host.calls().is_empty());

    let env = Env::new("apt-records-load");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.records.faults(|f| f.load = true);
    assert_io(
        &env.installer()
            .activate(&order, &dir)
            .expect_err("load fails"),
    );
    assert_io(
        &env.installer()
            .remove(&service(SERVICE), 5)
            .expect_err("load fails"),
    );
    assert_io(
        &env.installer()
            .drift(&service(SERVICE), PACKAGE, 5)
            .expect_err("load fails"),
    );

    let env = Env::new("apt-records-save");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.records.faults(|f| f.save = true);
    assert_io(
        &env.installer()
            .activate(&order, &dir)
            .expect_err("save fails"),
    );
    assert!(
        !env.has_run("apt-get install"),
        "nothing installed before the pending record"
    );

    let env = Env::new("apt-records-clear");
    env.installed(&deb(V1), &["mdm.service"]);
    env.records.faults(|f| f.clear = true);
    assert_io(
        &env.installer()
            .remove(&service(SERVICE), 5)
            .expect_err("clear fails"),
    );
    assert!(env.host.installed(PACKAGE).is_none(), "the purge was done");
}

/// Proves a work directory that cannot be made is `io_error`, in the stage and in the activate.
#[test]
fn a_work_directory_that_cannot_be_made_is_an_io_error() {
    let mut env = Env::new("apt-work-file");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    let blocker = env.scratch.path().join("work-is-a-file");
    std::fs::write(&blocker, "x").expect("a file");
    env.dirs.work = blocker;
    let (result, _) = env.stage(&order);
    let failure = result.expect_err("no work directory");
    assert_io(&failure);
    assert!(failure.detail().starts_with("the work directory"));
    assert_io(
        &env.installer()
            .activate(&order, &dir)
            .expect_err("no work directory"),
    );
}

/// Proves an installer lock that cannot be opened is `io_error`, in the stage, the activate and
/// the remove, and nothing is installed.
#[test]
fn an_installer_lock_that_cannot_be_opened_is_an_io_error() {
    let mut env = Env::new("apt-lock-bad");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.lock = hostkeeper_install::repository::InstallerLock::new(
        &env.scratch.path().join("none/installer.lock"),
    );
    let (result, _) = env.stage(&order);
    assert_io(&result.expect_err("no lock"));
    assert_io(&env.installer().activate(&order, &dir).expect_err("no lock"));
    assert_io(
        &env.installer()
            .remove(&service(SERVICE), 5)
            .expect_err("no lock"),
    );
    assert!(env.records.get(SERVICE).is_none() & env.host.all_installed().is_empty());
}

/// Proves a staging directory that is not usable is `io_error`: one that is not there, and one
/// whose `apt` is a file; and a work directory that holds a `control` directory already.
#[test]
fn a_staging_directory_that_cannot_be_written_is_an_io_error() {
    let env = Env::new("apt-staging-bad");
    let order = env.publish(&deb(V1));
    let missing = env.scratch.path().join("nowhere");
    assert_io(
        &env.installer()
            .stage(&order, &missing)
            .expect_err("no staging directory"),
    );

    let staging = env.staging();
    std::fs::write(staging.join("apt"), "x").expect("a file");
    let failure = env
        .installer()
        .stage(&order, &staging)
        .expect_err("no copy");
    assert_io(&failure);
    assert!(failure.detail().contains("the version's copy of the index"));

    env.host
        .edit(|s| s.faults.litter = vec!["control".to_owned()]);
    env.installer()
        .stage(&order, &env.staging())
        .expect("a directory named control in the work directory is nothing to stage");
}

/// Proves a version directory that is missing is `io_error` at the activate, before anything is
/// recorded or installed, and one whose tree lost the run unit activates with no link for it; an
/// enablement directory that cannot be made is `io_error`; a service manager that
/// cannot reload is `io_error`.
#[test]
fn an_activate_over_a_damaged_version_directory_fails() {
    let env = Env::new("apt-activate-damaged");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    let missing = env.scratch.path().join("gone");
    assert_io(
        &env.installer()
            .activate(&order, &missing)
            .expect_err("no copy"),
    );
    assert!(env.records.saved().is_empty());

    std::fs::remove_file(dir.join("tree/usr/lib/systemd/system/mdm.service")).expect("removed");
    env.installer()
        .activate(&order, &dir)
        .expect("a run unit with no unit file activates");
    assert!(
        env.records
            .saved()
            .pop()
            .expect("a record")
            .links
            .is_empty()
    );

    let env = Env::new("apt-activate-links");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    std::fs::create_dir_all(env.dirs.root.join("etc")).expect("a directory");
    std::fs::write(env.dirs.root.join("etc/systemd"), "x").expect("a file");
    let failure = env
        .installer()
        .activate(&order, &dir)
        .expect_err("no links");
    assert_io(&failure);
    assert!(failure.detail().contains("enablement links"));

    let env = Env::new("apt-activate-reload");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.host
        .answer("systemctl daemon-reload", 0, Ok(ToolOutput::exit(1, "")));
    assert_io(
        &env.installer()
            .activate(&order, &dir)
            .expect_err("no reload"),
    );
}

/// Proves a retire whose directory cannot be inspected is `io_error`.
#[test]
fn a_retire_of_a_path_below_a_file_is_an_io_error() {
    let env = Env::new("apt-retire-bad");
    let file = env.scratch.path().join("a-file");
    std::fs::write(&file, "x").expect("a file");
    let failure = env
        .installer()
        .retire(&file.join("version"))
        .expect_err("not a directory");
    assert_io(&failure);
    assert!(failure.detail().starts_with("the version directory"));
}

/// Proves a work directory whose parent is a dangling link, so the directory cannot be made, is
/// `io_error` naming the work directory.
#[test]
fn a_work_directory_below_a_dangling_link_is_an_io_error() {
    let mut env = Env::new("apt-work-dangling");
    let order = env.publish(&deb(V1));
    let dangling = env.scratch.path().join("dangling");
    std::os::unix::fs::symlink(env.scratch.path().join("nowhere/at/all"), &dangling).expect("link");
    env.dirs.work = dangling;
    let (result, _) = env.stage(&order);
    let failure = result.expect_err("no work directory");
    assert_io(&failure);
    assert!(failure.detail().starts_with("the work directory"));
}

/// Proves a repository entry whose name cannot be a file name, so the work directory cannot
/// take its sources file, is `io_error`, and nothing is fetched.
#[test]
fn a_sources_file_that_cannot_be_written_is_an_io_error() {
    use hostkeeper_install::repository::Repositories;
    let mut env = Env::new("apt-sources-bad");
    let text = format!(
        "Types: deb\nURIs: {URI}\nSuites: stable\nComponents: main\nSigned-By: {}\n",
        env.scratch.path().join("keys/main.gpg").display()
    );
    let entry = hostkeeper_install::repository::config::parse(
        "a/b",
        &text,
        &env.scratch.path().join("keys"),
    )
    .expect("an entry");
    env.repositories = Repositories::new(vec![entry]);
    let mut order = env.publish(&deb(V1));
    order.repository = "a/b".to_owned();
    let (result, _) = env.stage(&order);
    let failure = result.expect_err("no sources file");
    assert_io(&failure);
    assert!(failure.detail().starts_with("the work directory"));
    assert!(env.host.calls().is_empty());
}

/// Proves the backend's copy of a package file that has no name is `io_error`.
#[test]
fn a_package_file_with_no_name_cannot_be_kept() {
    use hostkeeper_install::repository::Backend;
    let env = Env::new("apt-keep-nameless");
    let work = env.scratch.path().join("keep-work");
    std::fs::create_dir_all(work.join("sources")).expect("sources");
    std::fs::create_dir_all(work.join("lists")).expect("lists");
    std::fs::write(work.join("sources/main.sources"), "x").expect("a file");
    let meta = env.scratch.path().join("keep-meta");
    let error = hostkeeper_install::repository::apt::Apt
        .keep(&work, std::path::Path::new("/"), &meta, "main")
        .expect_err("no name");
    assert_eq!(error.to_string(), "the package file has no name");
}

/// Proves a seal that cannot be deleted (its directory is read-only) is `io_error` naming the
/// seal, after the version directory itself was found gone.
#[test]
fn a_seal_that_cannot_be_deleted_is_an_io_error() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("apt-retire-seal");
    let parent = env.scratch.path().join("sealed");
    std::fs::create_dir(&parent).expect("a directory");
    std::fs::write(parent.join("v.seal"), "seal").expect("a seal");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).expect("read-only");
    let failure = env
        .installer()
        .retire(&parent.join("v"))
        .expect_err("the seal stays");
    assert_io(&failure);
    assert!(failure.detail().starts_with("the version's seal"));
}
