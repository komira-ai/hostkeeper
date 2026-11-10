//! What every step refuses before it acts: a params file that is not right, and a boot
//! directory another step holds.

use std::fs;

use hostkeeper_hook::{Class, Outcome, Step};
use rustix::fs::{FlockOperation, flock};

use super::kit::{FakeSystemctl, Setup, plain, step, unreached};

/// A params file that is missing or not TOML, and each param that is missing or wrong, fail the
/// step as `bad_params` of class `environment`, naming the param, before anything is done.
///
/// Catches a hook that acts on a release URL that is not https, a relative directory, or a
/// version that names a path outside the boot directory.
#[test]
fn each_param_that_is_not_right_is_bad_params() {
    let setup = Setup::new("params");
    let boot = setup.boot.display().to_string();
    let settings = setup.settings.display().to_string();
    let good = |without: &str, line: &str| {
        let mut lines = vec![
            "release = \"https://releases.example/v1.2.3\"".to_owned(),
            format!("settings = \"{settings}\""),
            format!("boot_dir = \"{boot}\""),
            "unit = \"pxe.service\"".to_owned(),
            "\"updater.target_version\" = \"1.2.3\"".to_owned(),
        ];
        lines.retain(|l| !l.starts_with(without));
        lines.push(line.to_owned());
        lines.join("\n")
    };
    let cases = [
        (good("release", ""), "param release is missing"),
        (good("release", "release = 4"), "param release must be text"),
        (
            good("release", "release = \"http://releases.example\""),
            "param release: not an https:// URL without a query or fragment",
        ),
        (
            good("settings", "settings = \"pxe.toml\""),
            "param settings must be an absolute path",
        ),
        (
            good("boot_dir", "boot_dir = \"var/pxe\""),
            "param boot_dir must be an absolute path",
        ),
        (
            good("unit", "unit = \"pxe\""),
            "param unit must be a plain unit name ending .service",
        ),
        (
            good("unit", "unit = \"../x.service\""),
            "param unit must be a plain unit name ending .service",
        ),
        (
            good("unit", "unit = \"-x.service\""),
            "param unit must be a plain unit name ending .service",
        ),
        (
            good("\"updater.target_version\"", ""),
            "updater.target_version is missing",
        ),
        (
            good(
                "\"updater.target_version\"",
                "\"updater.target_version\" = \"../1\"",
            ),
            "updater.target_version must be one path component of letters, digits and . _ + -",
        ),
        (
            good(
                "\"updater.target_version\"",
                "\"updater.target_version\" = \"..\"",
            ),
            "updater.target_version must be one path component of letters, digits and . _ + -",
        ),
        (
            good(
                "\"updater.target_version\"",
                "\"updater.target_version\" = \".\"",
            ),
            "updater.target_version must be one path component of letters, digits and . _ + -",
        ),
        (
            good(
                "\"updater.target_version\"",
                "\"updater.target_version\" = \"\"",
            ),
            "updater.target_version must be one path component of letters, digits and . _ + -",
        ),
        (
            good(
                "zzz",
                &format!(
                    "\"updater.previous_version\" = \"{}\"\n\"updater.previous_release\" = \"/x\"",
                    "a/b"
                ),
            ),
            "updater.previous_version must be one path component of letters, digits and . _ + -",
        ),
    ];
    for (text, detail) in cases {
        let params = setup.params_text(&text);
        let systemctl = FakeSystemctl::default();
        for which in [Step::Prepare, Step::Cutover, Step::Rollback, Step::Cleanup] {
            let ended = step(&setup, which, &params, &systemctl, &unreached);
            assert_eq!(
                ended.refused(Outcome::Failure, Class::Environment, "bad_params"),
                detail,
                "{text}"
            );
        }
        assert!(systemctl.called().is_empty());
        assert!(!setup.boot.exists());
    }
    let long = "a".repeat(256);
    let params = setup.params(Some(&long), None, "");
    let ended = plain(&setup, Step::Cutover, &params, &FakeSystemctl::default());
    ended.refused(Outcome::Failure, Class::Environment, "bad_params");
    let params = setup.params(Some("1.2.3+build_4-rc.1"), None, "");
    setup.versions(&["1.2.3+build_4-rc.1"]);
    plain(&setup, Step::Cutover, &params, &FakeSystemctl::default()).succeeded();
}

/// `verify` and `uninstall` need no target version; a params file that is missing, is not TOML,
/// or holds a table is `bad_params` with the file named.
///
/// Catches a verify that fails for a missing version it never uses, and a params file read
/// leniently.
#[test]
fn only_the_steps_that_name_a_boot_set_need_a_target_version() {
    let setup = Setup::new("params-file");
    let params = setup.params(None, None, "");
    plain(&setup, Step::Verify, &params, &FakeSystemctl::default()).succeeded();
    plain(&setup, Step::Uninstall, &params, &FakeSystemctl::default()).succeeded();
    let missing = setup.path("none.toml");
    for file in [
        missing.clone(),
        setup.params_text("not toml ="),
        setup.params_text("[a]\nb = 1\n"),
    ] {
        let ended = plain(&setup, Step::Verify, &file, &FakeSystemctl::default());
        let detail = ended.refused(Outcome::Failure, Class::Environment, "bad_params");
        assert!(
            detail.starts_with(&format!("{}: ", file.display())),
            "{detail}"
        );
    }
}

/// Every step but `verify` that finds the boot directory locked fails with the reason `locked`
/// of class `release`, changes nothing, and runs once the lock is let go.
///
/// Catches two steps working in the boot directory at once.
#[test]
fn a_boot_directory_held_by_another_step_is_locked() {
    let setup = Setup::new("locked");
    setup.versions(&["1.2.3"]);
    let params = setup.params(Some("1.2.3"), None, "");
    let held = fs::File::open(&setup.boot).expect("open");
    flock(&held, FlockOperation::NonBlockingLockExclusive).expect("lock");
    for which in [
        Step::Prepare,
        Step::Cutover,
        Step::Rollback,
        Step::Cleanup,
        Step::Uninstall,
    ] {
        let systemctl = FakeSystemctl::default();
        let ended = step(&setup, which, &params, &systemctl, &unreached);
        let detail = ended.refused(Outcome::Failure, Class::Release, "locked");
        assert!(
            detail.starts_with(&format!(
                "{} is locked by another step: ",
                setup.boot.display()
            )),
            "{detail}"
        );
        assert!(systemctl.called().is_empty());
    }
    assert_eq!(setup.listing(), ["1.2.3"]);
    plain(&setup, Step::Verify, &params, &FakeSystemctl::default()).succeeded();
    drop(held);
    plain(&setup, Step::Cleanup, &params, &FakeSystemctl::default()).succeeded();
}

/// A boot directory that cannot be made (its parent is a file) fails with `io_failed`.
///
/// Catches a step that carries on without its directory.
#[test]
fn a_boot_directory_that_cannot_be_made_is_io_failed() {
    let setup = Setup::new("no-boot");
    fs::write(&setup.boot, "a file").expect("a file");
    let params = setup.params(Some("1.2.3"), None, "");
    let ended = plain(&setup, Step::Cleanup, &params, &FakeSystemctl::default());
    let detail = ended.refused(Outcome::Failure, Class::Environment, "io_failed");
    assert!(
        detail.starts_with(&format!("{}: ", setup.boot.display())),
        "{detail}"
    );
    let ended = plain(&setup, Step::Uninstall, &params, &FakeSystemctl::default());
    ended.succeeded();
}
