//! The names an order carries, as the runner reads them: a service name outside the rule of
//! design 11.8 or too long for the units its steps run in is refused before anything runs, and
//! one inside it carries into the unit names.

mod support;

use std::fs;

use hostkeeper_hook::Verdict;
use hostkeeper_steps::{Finished, RunError, Staged};
use support::Rig;

fn ok() -> Finished {
    Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    }
}

/// Catches an order whose names could reach a unit name or a path unchecked: a service name
/// outside the rule of design 11.8 (empty, white space, a `/`, a non-ASCII letter, a leading `-`
/// or `.`, a `+`, 256 bytes), and a release path that is not UTF-8 (the params file and the
/// artifact argument must be the same string).
#[test]
fn an_order_with_unusable_names_is_refused() {
    let rig = Rig::new("run-bad-order");
    let long = "a".repeat(256);
    for name in ["", "a b", "a/b", "ünï", "-a", ".a", "..", "a+b", &long] {
        let mut order = rig.order();
        order.service = name.to_owned();
        let err = rig.runner().run(&order).unwrap_err();
        assert!(matches!(err, RunError::BadOrder(_)), "{name}");
        assert!(err.to_string().contains("not valid"));
    }
    use std::os::unix::ffi::OsStrExt;
    let odd = rig
        .scratch
        .path()
        .join(std::ffi::OsStr::from_bytes(&[0xff]));
    fs::create_dir(&odd).unwrap();
    let odd = Staged::seal(&odd).unwrap();
    let mut order = rig.order();
    order.staged = odd.clone();
    assert!(matches!(
        rig.runner().run(&order),
        Err(RunError::BadOrder(_))
    ));
    let mut order = rig.order();
    order.previous = Some(odd);
    assert!(matches!(
        rig.runner().run(&order),
        Err(RunError::BadOrder(_))
    ));
    assert!(rig.fake.events().is_empty());
}

/// Catches a service name that keeps its rule but is too long for the units its steps run in:
/// `updater-<service>-<step>-<generation>.service` is at most 255 bytes for the longest step
/// (`rollback`) and the largest generation, so 209 bytes run and 210 bytes, or 255, are
/// `UnitNameTooLong` before anything runs.
#[test]
fn a_service_name_too_long_for_its_step_units_is_refused() {
    let rig = Rig::new("run-long-name");
    for service in ["a".repeat(210), "a".repeat(255)] {
        let mut order = rig.order();
        order.service = service.clone();
        let err = rig.runner().run(&order).unwrap_err();
        assert!(
            matches!(&err, RunError::UnitNameTooLong { service: s } if *s == service),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!(
                "the service name {service:?} is too long for the units its steps run in: \
                 `updater-<service>-<step>-<generation>.service` would pass 255 bytes"
            )
        );
    }
    assert!(rig.fake.events().is_empty());
    let longest = format!("updater-{}-rollback-{}.service", "a".repeat(209), u64::MAX);
    assert_eq!(longest.len(), 255);
    let rig = Rig::new("run-longest-name");
    let mut order = rig.order();
    order.service = "a".repeat(209);
    assert_eq!(rig.runner().run(&order).unwrap(), ok());
}

/// Catches a service name with `-`, `_` or `.` being refused: each is in the name rule of design
/// 11.8 and carries into the unit names.
#[test]
fn a_service_name_may_hold_dashes_underscores_and_dots() {
    for (n, (name, unit)) in [
        ("my-svc_1", "updater-my-svc_1-prepare-1"),
        ("my.svc", "updater-my.svc-prepare-1"),
    ]
    .into_iter()
    .enumerate()
    {
        let rig = Rig::new(&format!("run-name-{n}"));
        let mut order = rig.order();
        order.service = name.to_owned();
        assert_eq!(rig.runner().run(&order).unwrap(), ok());
        let support::Event::Run { unit: ran, .. } = &rig.fake.runs()[0] else {
            unreachable!()
        };
        assert_eq!(ran, unit);
    }
}
