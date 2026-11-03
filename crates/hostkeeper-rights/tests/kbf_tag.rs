//! The names a host, a hook or a file on disk sees, pinned as literals copied from the kbf source
//! of `update-rights` at the tag the crate was imported from (MIGRATION.md). The expected values
//! are written out here, never read back from the constants under test.

mod support;

use std::path::Path;

use hostkeeper_hook::Step;
use hostkeeper_rights::reserved::{HOOKS_ROOT, Kind, RESERVED, Violation, check};
use hostkeeper_rights::{
    APPROVALS_FILE, DECLARATION_DIR, FIRST_INSTALL, Launch, RIGHTS_CHANGE, command, unit_name,
};

/// The kbf tag the crate was imported from.
const KBF_TAG: &str = "v0.0.64";
/// The kbf commit of that tag.
const KBF_COMMIT: &str = "8f90d0604794f3fee057e71647c7e1c1e034fa2e";

/// Catches a rename or edit of a name that a host, a hook or a file on disk already sees:
/// the declaration directory, the approvals file, the default hook arguments, the unit name of a
/// step, the hooks root, the reserved updater directory and the two reasons a deferral reports.
/// The kbf source at the tag has no boot unit and no constant for `/var/lib/updater/state` or
/// `/var/lib/updater/releases`; those two paths appear only in the design document, so this test
/// asserts instead that the reserved-path rules refuse each as a write path or a record
/// directory of a hook, and refuse it with the reserved updater directory named.
#[test]
fn constants_match_kbf_at_the_tag() {
    let at = format!("kbf {KBF_TAG} ({KBF_COMMIT})");
    assert_eq!(DECLARATION_DIR, "/etc/updater.d", "{at}");
    assert_eq!(APPROVALS_FILE, "/etc/updater.d/approved", "{at}");
    assert_eq!(HOOKS_ROOT, "/var/lib/updater/hooks", "{at}");
    assert_eq!(FIRST_INSTALL, "first_install", "{at}");
    assert_eq!(RIGHTS_CHANGE, "rights_change", "{at}");
    assert_eq!(support::sample().hook_args, ["update-hook"], "{at}");

    assert!(RESERVED.contains(&"/var/lib/updater"), "{at}");
    assert!(RESERVED.contains(&"/etc/updater.d"), "{at}");
    assert!(RESERVED.contains(&"/run/updater"), "{at}");
    for path in ["/var/lib/updater/state", "/var/lib/updater/releases"] {
        for kind in [Kind::Write, Kind::State] {
            assert_eq!(
                check(kind, path),
                Err(Violation::Inside("/var/lib/updater")),
                "{path} as {kind:?} at {at}"
            );
        }
    }

    let expected = [
        (Step::Prepare, "updater-tierstore-prepare-7"),
        (Step::Cutover, "updater-tierstore-cutover-7"),
        (Step::Verify, "updater-tierstore-verify-7"),
        (Step::Rollback, "updater-tierstore-rollback-7"),
        (Step::Cleanup, "updater-tierstore-cleanup-7"),
    ];
    for (step, name) in expected {
        assert_eq!(unit_name("tierstore", step, 7), name, "{at}");
    }
}

/// Catches a change to the unit name that `systemd-run` is given for a step of the sample
/// declaration: for every step the command carries exactly one `--unit=` argument, and it is
/// `updater-tierstore-<step>-<generation>`.
#[test]
fn the_command_of_each_step_names_the_kbf_unit() {
    let declared = support::declared(support::sample(), 65534, &["tierstore"]);
    let release = Path::new("/var/lib/updater/releases/tierstore/abc");
    for step in Step::ALL {
        let launch = Launch::of(&declared, step, 7, release, None);
        let run = command(&launch).expect("the sample builds a command");
        let units: Vec<&str> = run
            .args
            .iter()
            .filter_map(|arg| arg.to_str()?.strip_prefix("--unit="))
            .collect();
        assert_eq!(
            units,
            [format!("updater-tierstore-{step}-7")],
            "{step} at kbf {KBF_TAG}"
        );
    }
}
