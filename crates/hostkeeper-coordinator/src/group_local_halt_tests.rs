//! A host's own halt of a package a host group lists: the host is held under `stale_halt_here`
//! while it holds a generation above the group's, and the operator's clear issues the group a
//! generation past it; and [`past_held`], the clear's count, on its own.

use hostkeeper_wire::wire::{LocalHalt, Token};
use serde_json::json;

use crate::decide::{Gate, Why};
use crate::group_decision_support::Farm;
use crate::host_groups::GroupName;
use crate::marks_support::{NEW_DAEMON, ask, beat_of, runs};
use crate::operate::{Lifted, past_held};
use crate::ops::{Halt, ServiceOps};
use crate::testutil::OPERATOR;

const GROUP: &str = "farm-linux";
const PACKAGE: &str = "tierstore";

/// Proves: in a group whose generation of the package is 0, a host that holds a halt of it under
/// 15 is `stale_halt_here`; the clear, with no halt of the group standing, is audited for the
/// group naming no host and issues the group 16; the host, still holding 15, is `halted_here`.
///
/// Catches: a clear in group mode refused `not halted`, or counted from the group's own
/// generation, which never reaches the one the host holds.
#[test]
fn a_clear_in_a_group_issues_past_a_hosts_own_halt() {
    let farm = Farm::new("gr-local-halt");
    farm.group(GROUP, &[PACKAGE], None);
    farm.host("n1", GROUP);
    farm.want(GROUP, PACKAGE, "1.0");
    let mut held = runs(PACKAGE, "1.0");
    held.local_halt = Some(LocalHalt {
        reason: Token::parse("disk_tainted"),
        generation: 15,
        age_secs: Some(60),
    });
    let beat = beat_of(NEW_DAEMON, vec![runs("hostkeeper", "1.0"), held]);
    ask(&farm.world.rig, "n1", "storage", beat.clone());
    assert_eq!(
        farm.why("n1", PACKAGE),
        Some(Why::Gate(Gate::StaleHaltHere))
    );

    let body = json!({"action": "clear_halt", "service": PACKAGE, "reason": "stale hold"});
    let entry = farm
        .world
        .coordinator()
        .operate(Some(OPERATOR), body.to_string().as_bytes())
        .expect("cleared");
    assert_eq!(entry.action, "clear_halt");
    assert_eq!(entry.host_group.as_deref(), Some(GROUP));
    assert_eq!(entry.node_id, None);
    let group: GroupName = GROUP.parse().expect("a group name");
    assert_eq!(
        farm.world
            .ops()
            .group_marks(&group, PACKAGE)
            .halt_generation,
        16
    );
    ask(&farm.world.rig, "n1", "storage", beat);
    assert_eq!(farm.why("n1", PACKAGE), Some(Why::Gate(Gate::HaltedHere)));
}

/// A halt standing in marks whose generation is `issued`.
fn standing(issued: u64) -> ServiceOps {
    let mut marks = ServiceOps {
        halt_generation: issued,
        ..ServiceOps::default()
    };
    marks.halt = Some(
        serde_json::from_value(json!({
            "node_id": "s1", "check": "disk_tainted", "target": "2.0"
        }))
        .expect("a halt"),
    );
    marks
}

/// Proves the count of a clear, case by case: with no halt standing and no host holding one,
/// nothing is lifted and the generation stays; a standing halt alone counts one; a host's halt
/// alone, under one at or below the generation issued, counts one too; under one above it, the
/// generation is one past it; and a standing halt with a host's halt above the generation is one
/// past the host's.
///
/// Catches: a clear that counts from either number alone, or that counts a generation for a halt
/// no one holds.
#[test]
fn a_clear_issues_one_past_the_generation_issued_and_the_one_held() {
    let mut nothing = ServiceOps {
        halt_generation: 3,
        ..ServiceOps::default()
    };
    assert!(past_held(&mut nothing, None).is_none());
    assert_eq!(nothing.halt_generation, 3);

    let mut alone = standing(3);
    assert!(matches!(
        past_held(&mut alone, None),
        Some(Lifted::Standing(Halt { .. }))
    ));
    assert_eq!((alone.halt_generation, alone.halt.is_none()), (4, true));

    for (held, issued) in [(2, 4), (3, 4), (15, 16)] {
        let mut marks = ServiceOps {
            halt_generation: 3,
            ..ServiceOps::default()
        };
        assert!(matches!(
            past_held(&mut marks, Some(held)),
            Some(Lifted::Held)
        ));
        assert_eq!(marks.halt_generation, issued, "held {held}");
    }

    let mut both = standing(3);
    assert!(matches!(
        past_held(&mut both, Some(15)),
        Some(Lifted::Standing(_))
    ));
    assert_eq!(both.halt_generation, 16);

    let mut top = ServiceOps {
        halt_generation: 3,
        ..ServiceOps::default()
    };
    past_held(&mut top, Some(u64::MAX));
    assert_eq!(top.halt_generation, u64::MAX);
}
