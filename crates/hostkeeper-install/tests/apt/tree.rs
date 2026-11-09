//! The package's tree, its hook and its control members at `stage`: the paths a service's
//! package may install, the hook's rules and the maintainer scripts (packages-native.md section
//! 10.2).

use hostkeeper_install::repository::{Class, Failure, Order, Outcome, Reason};

use crate::apt_fake::Deb;
use crate::harness::*;

use Class::Release;
use Outcome::Failed;

/// Stages `package` for `service` with `run` and `edit` applied to its order, and answers the
/// result.
fn stage_with(
    name: &str,
    service: &str,
    package: &Deb,
    edit: impl FnOnce(&mut Order),
) -> (
    Env,
    Result<hostkeeper_install::repository::Staged, Failure>,
    std::path::PathBuf,
) {
    let env = Env::new(name);
    env.host.publish(URI, package.clone());
    let mut order = order_of(service, package, &["mdm.service"]);
    edit(&mut order);
    let (result, dir) = env.stage(&order);
    (env, result, dir)
}

fn stage_package(
    name: &str,
    package: &Deb,
) -> (
    Env,
    Result<hostkeeper_install::repository::Staged, Failure>,
    std::path::PathBuf,
) {
    stage_with(name, SERVICE, package, |_| {})
}

/// Proves each path kind a service's package may not install is `manifest_invalid`, failed,
/// release, naming the path, with nothing kept: an enablement link under `etc/systemd/system`,
/// a cron file, a `sudoers.d` file, a unit of another name, a drop-in directory of a unit, a link
/// in the unit directory (of either unit directory), a unit of a kind that is not service,
/// socket or timer, a unit whose name only starts like the service's, a program outside the
/// allowed directories, and a named pipe.
#[test]
fn every_refused_path_kind_is_manifest_invalid() {
    let wants = "etc/systemd/system/multi-user.target.wants/mdm.service";
    let cases: Vec<(&str, Deb, &str)> = vec![
        (
            "an enablement link",
            deb(V1).link(wants, "/usr/lib/systemd/system/mdm.service"),
            "etc/systemd",
        ),
        (
            "a cron file",
            deb(V1).file("etc/cron.d/mdm", 0o644, "x"),
            "etc/cron.d",
        ),
        (
            "a sudoers file",
            deb(V1).file("etc/sudoers.d/mdm", 0o440, "x"),
            "etc/sudoers.d",
        ),
        (
            "a unit of another name",
            deb(V1).file(&format!("{UNIT_DIR}/other.service"), 0o644, &unit(None)),
            "other.service",
        ),
        (
            "a drop-in directory",
            deb(V1).file(&format!("{UNIT_DIR}/mdm.service.d/x.conf"), 0o644, "x"),
            "mdm.service.d",
        ),
        (
            "a link in the unit directory",
            deb(V1).link(&format!("{UNIT_DIR}/mdm-link.service"), "mdm.service"),
            "mdm-link.service",
        ),
        (
            "a link in the other unit directory",
            deb(V1).link("lib/systemd/system/mdm-link.service", "x"),
            "mdm-link.service",
        ),
        (
            "a unit of kind mount",
            deb(V1).file(&format!("{UNIT_DIR}/mdm.mount"), 0o644, &unit(None)),
            "mdm.mount",
        ),
        (
            "a unit named like the service's",
            deb(V1).file(&format!("{UNIT_DIR}/mdmx.service"), 0o644, &unit(None)),
            "mdmx.service",
        ),
        (
            "a unit with no kind",
            deb(V1).file(&format!("{UNIT_DIR}/mdm"), 0o644, &unit(None)),
            "system/mdm",
        ),
        (
            "a program in /opt",
            deb(V1).file("opt/mdm/run", 0o755, "x"),
            "opt",
        ),
        (
            "a file in another package's directory",
            deb(V1).file("usr/lib/other/x", 0o644, "x"),
            "usr/lib/other",
        ),
        ("a named pipe", deb(V1).fifo("usr/bin/pipe"), "usr/bin/pipe"),
    ];
    for (what, package, named) in cases {
        let (env, result, dir) = stage_package("apt-refused-paths", &package);
        let failure = result.expect_err(what);
        assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
        assert!(failure.detail().contains(named), "{what}: {failure}");
        assert!(!dir.join("apt").exists(), "{what}");
        assert!(env.records.saved().is_empty() & env.host.all_installed().is_empty());
    }
}

/// Proves every allowed path goes on: the program in `usr/bin`, files under `usr/lib/<package>`,
/// `usr/share/<package>`, `usr/share/doc/<package>` and `etc/<service>`, and the eight units of
/// the package `mdm` (a socket, a timer, a template, a worker service) in either unit directory,
/// run units or not; the checksum and conffile members go on.
#[test]
fn every_allowed_path_goes_on() {
    let package = mdm_deb(V1)
        .file("usr/lib/mdm/libmdm.so", 0o644, "x")
        .file("usr/share/mdm/data", 0o644, "x")
        .file("usr/share/doc/mdm/copyright", 0o644, "x")
        .file("etc/mdm/mdm.conf", 0o644, "x")
        .file("lib/systemd/system/mdm-late.service", 0o644, &unit(None))
        .member("conffiles");
    let (_env, result, dir) = stage_package("apt-allowed", &package);
    let staged = result.expect("every allowed path stages");
    assert!(!staged.hook);
    for path in package.tree.keys() {
        assert!(dir.join("tree").join(path).exists(), "{path}");
    }
    let units = listing(&dir.join("tree/usr/lib/systemd/system"));
    assert_eq!(units.len(), 8, "{units:?}");
}

/// Proves the unit-name rule: `<service>.<kind>`, `<service>-*.<kind>` and `<service>@*.<kind>`
/// of the kinds service, socket and timer go on, in the stage itself.
#[test]
fn units_named_for_the_service_stage() {
    for name in [
        "mdm.service",
        "mdm.socket",
        "mdm.timer",
        "mdm-a.b_c.service",
        "mdm@x.service",
        "mdm@.timer",
    ] {
        let package = deb(V1).file(&format!("{UNIT_DIR}/{name}"), 0o644, &unit(None));
        let (_, result, _) = stage_package("apt-unit-names", &package);
        result.expect(name);
    }
}

/// Proves a hook, `usr/lib/<package>/hostkeeper-hook`, that is a regular executable file stages
/// with `Staged.hook` true and that a package without one stages with it false.
#[test]
fn a_hook_is_found_and_its_absence_is_not_an_error() {
    let with = deb(V1).file("usr/lib/mdm/hostkeeper-hook", 0o755, "#!/bin/sh\n");
    let (_, result, _) = stage_package("apt-hook", &with);
    assert!(result.expect("a hook").hook);
    let (_, result, _) = stage_package("apt-no-hook", &deb(V1));
    assert!(!result.expect("no hook").hook);
}

/// Proves a hook that is a link, a directory or not executable is `manifest_invalid`, and a
/// package whose `usr/lib/<package>` is a link or a file is refused by the path rule.
#[test]
fn a_hook_that_is_not_a_plain_executable_is_refused() {
    let hook = "usr/lib/mdm/hostkeeper-hook";
    let cases = [
        ("a link", deb(V1).link(hook, "/bin/sh"), "a link"),
        (
            "a directory",
            deb(V1).dir(hook, 0o755),
            "not a regular file",
        ),
        (
            "not executable",
            deb(V1).file(hook, 0o644, "x"),
            "not executable",
        ),
    ];
    for (what, package, why) in cases {
        let (_, result, _) = stage_package("apt-hook-refused", &package);
        let failure = result.expect_err(what);
        assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
        assert!(failure.detail().contains(why), "{what}: {failure}");
    }
    for (what, package) in [
        (
            "a linked package directory",
            deb(V1).link("usr/lib/mdm", "../share/mdm"),
        ),
        (
            "a file for the package directory",
            deb(V1).file("usr/lib/mdm", 0o644, "x"),
        ),
    ] {
        let (_, result, _) = stage_package("apt-hook-parent", &package);
        let failure = result.expect_err(what);
        assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
        assert!(
            failure
                .detail()
                .contains("installs /usr/lib/mdm, outside the paths"),
            "{what}: {failure}"
        );
    }
}

/// Proves a manifest that names a boot step, or has params, needs a hook: without one the
/// stage is `manifest_invalid`, with one it goes on.
#[test]
fn a_boot_step_and_params_need_a_hook() {
    let plain = deb(V1);
    let hooked = deb(V1).file("usr/lib/mdm/hostkeeper-hook", 0o755, "x");
    for (what, edit) in [
        (
            "boot",
            (|o: &mut Order| o.boot = Some("migrate".to_owned())) as fn(&mut Order),
        ),
        ("params", |o: &mut Order| o.params = true),
    ] {
        let (_, result, _) = stage_with("apt-boot-plain", SERVICE, &plain, edit);
        let failure = result.expect_err(what);
        assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
        let (_, result, _) = stage_with("apt-boot-hooked", SERVICE, &hooked, edit);
        assert!(result.expect(what).hook);
    }
}

/// Proves each maintainer script, preinst, postinst, prerm, postrm, config and triggers, is
/// `package_scripts`, failed, release, naming the script, and that nothing is extracted or kept.
#[test]
fn each_maintainer_script_is_refused() {
    for script in [
        "preinst", "postinst", "prerm", "postrm", "config", "triggers",
    ] {
        let (env, result, dir) = stage_package("apt-script", &deb(V1).member(script));
        let failure = result.expect_err(script);
        assert_failure(&failure, Reason::PackageScripts, Release, Failed);
        assert!(failure.detail().contains(script), "{failure}");
        assert!(
            !env.has_run("dpkg-deb --extract"),
            "{script}: the package was extracted"
        );
        assert!(!dir.join("tree").exists() & !dir.join("apt").exists());
    }
}

/// Proves the control file, the checksums and the conffiles are not scripts and go on.
#[test]
fn control_checksums_and_conffiles_go_on() {
    let package = deb(V1).member("conffiles");
    let (_, result, _) = stage_package("apt-members", &package);
    result.expect("no script");
}

/// Proves scripts are allowed for the package `hostkeeper` of the service `hostkeeper` only: they
/// go on there, and are refused for hostkeeper's package under another service and for another
/// package under the service `hostkeeper`.
#[test]
fn scripts_are_allowed_for_hostkeepers_own_package_alone() {
    let own = |package: &str| {
        Deb::new(package, V1, "amd64")
            .file("usr/bin/hostkeeper", 0o755, "x")
            .file(
                &format!("{UNIT_DIR}/hostkeeper.service"),
                0o644,
                &unit(None),
            )
            .member("preinst")
            .member("postinst")
    };
    let (_, result, _) = stage_with("apt-own", "hostkeeper", &own("hostkeeper"), |o| {
        o.run = vec!["hostkeeper.service".to_owned()];
    });
    result.expect("hostkeeper's own package may carry scripts");
    let (_, result, _) = stage_with("apt-own-elsewhere", SERVICE, &own("hostkeeper"), |_| {});
    let failure = result.expect_err("a script under another service");
    assert_eq!(failure.reason(), Reason::PackageScripts);
    let (_, result, _) = stage_with("apt-other-own", "hostkeeper", &own("hostkeeperd"), |o| {
        o.run = vec!["hostkeeper.service".to_owned()];
    });
    let failure = result.expect_err("another package under hostkeeper");
    assert_eq!(failure.reason(), Reason::PackageScripts);
}
