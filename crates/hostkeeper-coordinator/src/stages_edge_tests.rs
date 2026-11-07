//! Edges of the update plan ([`crate::stages`]): a manual stage that comes apart from the stage
//! the plan entered as hosts join, a host the plan can never order, `min_up` and a host that is
//! not up, and a failed host of a stage that shrank.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::wire::Outcome;

use crate::assignment::ServiceAssignments;
use crate::decide::{Decision, Gate, decide_why};
use crate::desired::Desired;
use crate::fleet::{Fleet, Marks};
use crate::services::Services;
use crate::stages::{View, reach};
use crate::testutil::{beat, desired, id, last, role, secs, unhealthy, version};

const NOW: Duration = Duration::from_secs(100);

fn services(extra: &str) -> Services {
    Services::parse(&format!(
        "[[service]]\nname = \"app\"\nroles = [\"farm\"]\n{extra}\n"
    ))
    .expect("the services")
}

fn want(max_updating: u32, min_up: u32) -> Desired {
    let mut want = desired("2.0", 0, 0);
    want.max_updating.set("farm", max_updating);
    want.min_up.set("farm", min_up);
    want
}

/// A fleet whose hosts `h01`.. of `app` run what `running` says, healthy, heard at `NOW`.
fn fleet(running: &[&str]) -> Fleet {
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    for (i, v) in running.iter().enumerate() {
        let host = format!("h{:02}", i + 1);
        f.record_in("app", id(&host), role("farm"), beat(v), NOW);
    }
    f
}

fn why(s: &Services, w: &Desired, f: &Fleet, host: &str) -> Decision {
    let all = BTreeMap::from([("app".to_owned(), w.clone())]);
    decide_why(s, &all, "app", f, &id(host), NOW)
}

/// Proves: with `[{ percent = 1, manual }, { percent = 10, manual }, 100]` the 10 percent stage
/// adds no host on 10 hosts and is folded into the first; once an operator enters the first
/// stage and it is done, an eleventh host makes the 10 percent stage come apart (2 hosts), and
/// it waits for its own Advance: the plan does not count it as entered with the first.
/// Catches: a position kept as the end of the stages folded into the one entered, which lets a
/// manual stage go without an operator once hosts join.
#[test]
fn a_manual_stage_that_comes_apart_as_hosts_join_waits_for_its_advance() {
    let s = services(
        "stages = [{ percent = 1, advance = \"manual\" }, { percent = 10, advance = \"manual\" }, 100]",
    );
    let def = s.get("app").expect("app");
    let w = want(10, 0);
    let ten = vec!["1.0"; 10];
    let f = fleet(&ten);
    let none = View::at(def, &w, 0, BTreeSet::new(), &f, NOW);
    assert_eq!(none.current, 0, "the first stage waits for an operator");
    let entered = reach(def, &w, none, true, &f, NOW)
        .expect("entered")
        .entered;
    // h01 rolled; an eleventh host joins.
    let mut running = vec!["2.0"];
    running.extend(vec!["1.0"; 10]);
    let f = fleet(&running);
    let view = View::at(def, &w, entered, BTreeSet::new(), &f, NOW);
    let sizes: Vec<usize> = view.stages.iter().map(|st| st.hosts).collect();
    assert_eq!(sizes, [1, 2, 11]);
    assert_eq!(view.current, 1, "only the first stage is entered");
    let mut marks = Marks::default();
    marks.rollouts.insert(
        "app".to_owned(),
        crate::rollouts::Rollout {
            version: Some(version("2.0")),
            entered: u32::try_from(entered).expect("small"),
            ..Default::default()
        },
    );
    let mut f = f;
    f.set_marks(marks);
    assert_eq!(why(&s, &w, &f, "h11"), Decision::Gate(Gate::AdvanceWait));
}

/// Proves: a host the desired version is below the minimum of is not in the plan: the other
/// hosts roll, the plan is finished without it, and its rollout no longer counts as live.
/// Catches: a host never ordered kept in the plan, which then never finishes and holds every
/// other service's rollout.
#[test]
fn a_host_below_its_minimum_version_is_not_in_the_plan() {
    let s = services("");
    let def = s.get("app").expect("app");
    let w = want(10, 0);
    let mut f = fleet(&["2.0", "2.0", "1.0"]);
    let mut entries = ServiceAssignments::default();
    entries
        .host_min_versions
        .insert("h03".to_owned(), version("3.0"));
    f.set_marks(Marks {
        assignments: BTreeMap::from([("app".to_owned(), entries)]),
        ..Marks::default()
    });
    assert_eq!(
        why(&s, &w, &f, "h03"),
        Decision::Gate(Gate::BelowMinVersion)
    );
    let view = View::effective(def, &w, None, &f, NOW);
    assert_eq!(view.members.len(), 2);
    assert!(view.finished());
    assert!(!crate::stages::is_live(def, Some(&w), &f, NOW));
}

/// Proves: with `min_up = 3` of 3 hosts of the role, a host that is unhealthy is still ordered
/// (its order takes no host that is up away), while a host that is up is not.
/// Catches: `min_up` counting the asker's own order against hosts that are up when it is down.
#[test]
fn an_unhealthy_host_is_ordered_under_a_min_up_its_peers_meet_without_it() {
    let s = services("stages = [100]");
    let w = want(3, 3);
    let mut f = fleet(&["1.0", "1.0", "1.0"]);
    f.record_in("app", id("h01"), role("farm"), unhealthy(beat("1.0")), NOW);
    assert_eq!(
        why(&s, &w, &f, "h01"),
        Decision::Update {
            version: version("2.0")
        }
    );
    assert_eq!(why(&s, &w, &f, "h02"), Decision::Gate(Gate::BelowMinUp));
}

/// Proves: a failed host keeps its stage undone even when the stage shrinks to as many done
/// hosts as it holds: on 8 hosts with h01, h02 and h04 done and h03 failed, excluding h07 and
/// h08 makes stage 2 three hosts, and h05 is still told `stage_failed`.
/// Catches: a stage counted done over a failed host once hosts leave.
#[test]
fn a_failed_host_keeps_a_shrunk_stage_undone() {
    let s = services("");
    let w = want(8, 0);
    let mut f = fleet(&["2.0", "2.0", "1.0", "2.0", "1.0", "1.0", "1.0", "1.0"]);
    f.record_in(
        "app",
        id("h03"),
        role("farm"),
        last(beat("1.0"), "2.0", Outcome::Failed),
        secs(100),
    );
    assert_eq!(why(&s, &w, &f, "h05"), Decision::Gate(Gate::StageFailed));
    f.set_marks(Marks {
        excluded: [
            ("app".to_owned(), "h07".to_owned()),
            ("app".to_owned(), "h08".to_owned()),
        ]
        .into(),
        ..Marks::default()
    });
    assert_eq!(why(&s, &w, &f, "h05"), Decision::Gate(Gate::StageFailed));
}
