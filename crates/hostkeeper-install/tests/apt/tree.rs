//! The package's files, its units, its hook program and its maintainer scripts at `stage`
//! (packages-native.md section 10.2): the one rule on the files is that the hook program the
//! manifest names is an executable regular file of the package.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_install::repository::enable;
use hostkeeper_install::repository::{Class, Failure, Order, Outcome, Reason, Staged};

use crate::apt_fake::Deb;
use crate::harness::*;

use Class::Release;
use Outcome::Failed;

/// Stages `package` for `service` with `edit` applied to its order (the run unit `mdm.service`),
/// and answers the result.
fn stage_with(
    name: &str,
    service: &str,
    package: &Deb,
    edit: impl FnOnce(&mut Order),
) -> (Env, Result<Staged, Failure>, std::path::PathBuf) {
    let env = Env::new(name);
    env.host.publish(URI, package.clone());
    let mut order = order_of(service, package, &["mdm.service"]);
    edit(&mut order);
    let (result, dir) = env.stage(&order);
    (env, result, dir)
}

fn stage_package(name: &str, package: &Deb) -> (Env, Result<Staged, Failure>, std::path::PathBuf) {
    stage_with(name, SERVICE, package, |_| {})
}

/// [`stage_with`] for a package whose hook program is `program`.
fn stage_hooked(
    name: &str,
    package: &Deb,
    program: &str,
) -> (Env, Result<Staged, Failure>, std::path::PathBuf) {
    stage_with(name, SERVICE, package, |o| {
        o.hook = Some(program.to_owned());
    })
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

/// Proves each maintainer script, preinst, postinst, prerm, postrm, config and triggers, goes on:
/// the package stages, no control member is read, and it installs.
#[test]
fn each_maintainer_script_stages_and_installs() {
    for script in [
        "preinst", "postinst", "prerm", "postrm", "config", "triggers",
    ] {
        let env = Env::new("apt-script");
        env.installed(&deb(V1).member(script), &["mdm.service"]);
        assert!(!env.has_run("dpkg-deb --control"), "{script}");
        let found = env.host.installed(PACKAGE).expect(script);
        assert_eq!(found.version, V1, "{script}");
    }
}

/// Proves a package may install files anywhere: under `/opt`, `/etc/cron.d`, a drop-in directory
/// of a unit, a link in a unit directory, an enablement link, a unit of another name and kind, a
/// file in another package's directory and a program outside `usr/bin`.
#[test]
fn files_anywhere_stage() {
    let wants = "etc/systemd/system/multi-user.target.wants/mdm.service";
    let package = deb(V1)
        .file("opt/mdm/run", 0o755, "x")
        .file("etc/cron.d/mdm", 0o644, "x")
        .file("etc/sudoers.d/mdm", 0o440, "x")
        .file(&format!("{UNIT_DIR}/other.service"), 0o644, &unit(None))
        .file(&format!("{UNIT_DIR}/mdm.mount"), 0o644, &unit(None))
        .file(&format!("{UNIT_DIR}/mdm.service.d/x.conf"), 0o644, "x")
        .link(&format!("{UNIT_DIR}/mdm-link.service"), "mdm.service")
        .link(wants, "/usr/lib/systemd/system/mdm.service")
        .file("usr/lib/other/x", 0o644, "x");
    let (_env, result, dir) = stage_package("apt-anywhere", &package);
    assert!(!result.expect("every path stages").hook);
    for path in package.tree.keys() {
        assert!(
            std::fs::symlink_metadata(dir.join("tree").join(path)).is_ok(),
            "{path}"
        );
    }
    assert!(dir.join("apt").exists());
}

/// Proves hostkeeper's own package stages like any other, scripts and files anywhere included.
#[test]
fn hostkeepers_own_package_stages_like_any_other() {
    let package = Deb::new("hostkeeper", V1, "amd64")
        .file("usr/bin/hostkeeper", 0o755, "x")
        .file("usr/lib/tmpfiles.d/hostkeeper.conf", 0o644, "x")
        .link(
            "etc/issue.d/hostkeeper.issue",
            "/run/hostkeeper/enrollment.issue",
        )
        .member("preinst")
        .member("postinst");
    let (_, result, _) = stage_with("apt-own", "hostkeeper", &package, |o| {
        o.run = vec!["hostkeeper.service".to_owned()];
    });
    result.expect("hostkeeper's own package stages");
}

/// Proves a run unit with no `[Install]` section stages and activates with no enablement link,
/// and `enable::unlinked` lists it, with the run unit that has a link left out.
#[test]
fn a_run_unit_with_no_install_section_gets_no_link_and_is_listed() {
    let env = Env::new("apt-no-install");
    let package = deb(V1).file(&format!("{UNIT_DIR}/mdm-bare.service"), 0o644, &unit(None));
    let run = ["mdm.service", "mdm-bare.service"];
    let (_, dir) = env.installed(&package, &run);
    let record = env.records.saved().pop().expect("a record");
    assert_eq!(
        record.links.into_iter().collect::<Vec<_>>(),
        ["/etc/systemd/system/multi-user.target.wants/mdm.service"]
    );
    let run: Vec<String> = run.iter().map(|u| (*u).to_owned()).collect();
    assert_eq!(
        enable::unlinked(&dir.join("tree"), &run),
        ["mdm-bare.service"]
    );
}

/// Proves a run unit with no unit file in the tree stages and activates, gets no link, and
/// `enable::unlinked` lists it, in the order of the run list.
#[test]
fn a_run_unit_with_no_unit_file_gets_no_link_and_is_listed() {
    let env = Env::new("apt-no-unit-file");
    let run = ["mdm-ghost.service", "mdm.service", "mdm-late.service"];
    let (_, dir) = env.installed(&deb(V1), &run);
    let record = env.records.saved().pop().expect("a record");
    assert_eq!(record.links.len(), 1, "{:?}", record.links);
    let run: Vec<String> = run.iter().map(|u| (*u).to_owned()).collect();
    assert_eq!(
        enable::unlinked(&dir.join("tree"), &run),
        ["mdm-ghost.service", "mdm-late.service"]
    );
}

/// Proves a `WantedBy` target that is not a unit name yields no link and no refusal: the unit
/// stages, activates, is listed by `enable::unlinked`, and a good target beside it still links.
#[test]
fn a_wanted_by_target_that_is_not_a_unit_name_yields_no_link() {
    let env = Env::new("apt-bad-target");
    let text = "[Service]\nExecStart=/usr/bin/mdm\n[Install]\nWantedBy=not a name\nWantedBy=bad\n";
    let package = deb(V1).file(&format!("{UNIT_DIR}/mdm.service"), 0o644, text);
    let (_, dir) = env.installed(&package, &["mdm.service"]);
    assert!(
        env.records
            .saved()
            .pop()
            .expect("a record")
            .links
            .is_empty()
    );
    assert!(!env.dirs.root.join("etc/systemd/system").exists());
    assert_eq!(
        enable::unlinked(&dir.join("tree"), &["mdm.service".to_owned()]),
        ["mdm.service"]
    );
    let mixed = "[Install]\nWantedBy=bad multi-user.target\n";
    let package = deb(V2).file(&format!("{UNIT_DIR}/mdm.service"), 0o644, mixed);
    let (_, dir) = env.installed(&package, &["mdm.service"]);
    let links = env.records.saved().pop().expect("a record").links;
    assert_eq!(links.len(), 1, "{links:?}");
    assert!(enable::unlinked(&dir.join("tree"), &["mdm.service".to_owned()]).is_empty());
}

/// Proves the hook program check: a program that is a regular executable file stages with
/// `Staged.hook` true; with no command the package stages with it false.
#[test]
fn a_hook_program_that_is_executable_stages() {
    let with = deb(V1).file("usr/bin/mdm-hook", 0o755, "#!/bin/sh\n");
    let (_, result, _) = stage_hooked("apt-hook", &with, "/usr/bin/mdm-hook");
    assert!(result.expect("a hook").hook);
    let (_, result, _) = stage_package("apt-no-hook", &with);
    assert!(!result.expect("no hook").hook);
}

/// Proves a link to an executable file of the package passes, followed inside the tree: an
/// absolute target from the tree's root.
#[test]
fn a_hook_program_that_is_a_link_to_an_executable_stages() {
    let package = deb(V1)
        .file("opt/mdm/bin/mdm", 0o755, "#!/bin/sh\n")
        .link("usr/bin/mdm-hook", "/opt/mdm/bin/mdm");
    let (_, result, _) = stage_hooked("apt-hook-link", &package, "/usr/bin/mdm-hook");
    assert!(result.expect("a linked hook").hook);
}

/// Proves a hook program that is absent, not executable, a directory or in a link loop is
/// `manifest_invalid`, release, failed, naming the command and why, and nothing is kept.
#[test]
fn a_hook_program_that_is_not_an_executable_file_is_manifest_invalid() {
    let program = "/usr/bin/mdm-hook";
    let cases = [
        ("absent", deb(V1), "is not a file the package installs"),
        (
            "not executable",
            deb(V1).file("usr/bin/mdm-hook", 0o644, "x"),
            "is not executable",
        ),
        (
            "a directory",
            deb(V1).dir("usr/bin/mdm-hook", 0o755),
            "is not a regular file",
        ),
        (
            "a link loop",
            deb(V1)
                .link("usr/bin/mdm-hook", "/usr/bin/mdm-other")
                .link("usr/bin/mdm-other", "/usr/bin/mdm-hook"),
            "more than 40 links",
        ),
    ];
    for (what, package, why) in cases {
        let (env, result, dir) = stage_hooked("apt-hook-refused", &package, program);
        let failure = result.expect_err(what);
        assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
        assert!(failure.detail().contains(program), "{what}: {failure}");
        assert!(failure.detail().contains(why), "{what}: {failure}");
        assert!(!dir.join("apt").exists(), "{what}");
        assert!(env.records.saved().is_empty() & env.host.all_installed().is_empty());
    }
}

/// Proves the extracted tree is normalized: a file loses its setuid, setgid, group-write and
/// other-write bits and keeps its execute bits, a directory loses its group and other write
/// bits, a link is left alone, and the dpkg install still sees the kept package.
#[test]
fn the_extracted_tree_loses_setuid_setgid_and_group_and_other_write() {
    let package = deb(V1)
        .file("usr/bin/suid", 0o4755, "x")
        .file("usr/bin/sgid", 0o2775, "x")
        .file("usr/share/mdm/open", 0o666, "x")
        .file("usr/share/mdm/plain", 0o640, "x")
        .dir("srv/mdm", 0o777)
        .link("usr/bin/mdm-link", "/usr/bin/suid");
    let (_env, result, dir) = stage_package("apt-normalize", &package);
    result.expect("the package stages");
    let tree = dir.join("tree");
    assert_eq!(mode_of(&tree.join("usr/bin/suid")), 0o755);
    assert_eq!(mode_of(&tree.join("usr/bin/sgid")), 0o755);
    assert_eq!(mode_of(&tree.join("usr/share/mdm/open")), 0o644);
    assert_eq!(mode_of(&tree.join("usr/share/mdm/plain")), 0o640);
    assert_eq!(mode_of(&tree.join("srv/mdm")), 0o755);
    assert_eq!(
        std::fs::read_link(tree.join("usr/bin/mdm-link")).expect("a link"),
        Path::new("/usr/bin/suid")
    );
}

/// Proves the tree directory itself is normalized too (dpkg gives it the package's `./` entry,
/// here group-writable) and that a fifo of the package is left out of the copy, which the seal
/// cannot hold, while the package still stages. Catches a stage that accepts a package whose
/// sealed tree every step then refuses.
#[test]
fn the_tree_directory_is_normalized_and_a_fifo_is_left_out_of_the_copy() {
    let package = deb(V1)
        .dir("", 0o775)
        .fifo("run/mdm/pipe")
        .file("usr/bin/mdm", 0o755, "x");
    let (_env, result, dir) = stage_package("apt-normalize-root", &package);
    result.expect("the package stages");
    let tree = dir.join("tree");
    assert_eq!(mode_of(&tree), 0o755);
    assert!(std::fs::symlink_metadata(tree.join("run/mdm/pipe")).is_err());
    assert_eq!(mode_of(&tree.join("usr/bin/mdm")), 0o755);
}
