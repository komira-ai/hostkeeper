//! The previous release and the previous version of an order go together: an update names both,
//! a first install neither, and an order with one alone is refused before anything is written.

mod support;

use hostkeeper_hook::Verdict;
use hostkeeper_steps::{Loaded, RunError};
use support::Rig;

/// Catches an order with only one of the previous release and the previous version starting an
/// update: the params file it would write holds one key of the pair, which `Params::parse`
/// refuses, so every step would fail on it. The order is `BadOrder`, naming the pair, and no
/// params file, no step record and no step comes of it.
#[test]
fn an_order_with_half_of_the_previous_release_is_refused_before_anything_is_written() {
    let rig = Rig::new("run-previous-half");
    let release_alone = {
        let mut order = rig.order();
        order.previous_version = None;
        order
    };
    let version_alone = {
        let mut order = rig.order();
        order.previous = None;
        order
    };
    for (name, order) in [
        ("release alone", release_alone),
        ("version alone", version_alone),
    ] {
        let err = rig.runner().run(&order).unwrap_err();
        assert!(
            matches!(
                err,
                RunError::BadOrder("the previous release or version alone")
            ),
            "{name}: {err}"
        );
        assert_eq!(
            err.to_string(),
            "the order is not valid: the previous release or version alone"
        );
        assert!(!rig.store().params_path().exists(), "{name}");
        assert!(
            matches!(rig.store().load().unwrap(), Loaded::Absent),
            "{name}"
        );
    }
    assert!(rig.fake.events().is_empty());
}

/// Catches the pair check refusing the two orders it must let through: an update with both and a
/// first install with neither run to success.
#[test]
fn an_update_with_both_and_a_first_install_with_neither_run() {
    let rig = Rig::new("run-previous-both");
    let finished = rig.runner().run(&rig.order()).unwrap();
    assert!(matches!(finished.verdict, Verdict::Succeeded { .. }));

    let rig = Rig::new("run-previous-neither");
    let mut first = rig.order();
    first.previous = None;
    first.previous_version = None;
    let finished = rig.runner().run(&first).unwrap();
    assert!(matches!(finished.verdict, Verdict::Succeeded { .. }));
}
