//! The names a host, a hook or a file on disk sees, pinned as literals copied from the kbf source
//! of `update-rights` at the tag the crate was imported from (MIGRATION.md). The expected values
//! are written out here, never read back from the constants under test.

mod support;

use hostkeeper_rights::load::{HOOKS_ROOT, Kind, RESERVED, Violation, check};

/// The kbf tag the crate was imported from.
const KBF_TAG: &str = "v0.0.64";
/// The kbf commit of that tag.
const KBF_COMMIT: &str = "8f90d0604794f3fee057e71647c7e1c1e034fa2e";

/// Catches a rename or edit of a name that a host, a hook or a file on disk already sees: the
/// default hook arguments, the hooks root and the reserved updater directories. (The unit name
/// of a step moved with the launcher: `constants_match_kbf_at_the_tag` of `hostkeeper-steps`.)
/// The kbf source at the tag has no boot unit and no constant for `/var/lib/updater/state` or
/// `/var/lib/updater/releases`; those two paths appear only in the design document, so this test
/// asserts instead that the reserved-path rules refuse each as a write path or a record
/// directory of a hook, and refuse it with the reserved updater directory named.
#[test]
fn constants_match_kbf_at_the_tag() {
    let at = format!("kbf {KBF_TAG} ({KBF_COMMIT})");
    assert_eq!(HOOKS_ROOT, "/var/lib/updater/hooks", "{at}");
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
}
