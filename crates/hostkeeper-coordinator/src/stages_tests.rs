//! The update plan as the decision reads it ([`crate::stages`]): the stages a rollout file
//! gives, their sizes over the eligible hosts, and the hosts ordered round by round over a fleet
//! of fake hosts at a clock the test sets.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::wire::Outcome;

use crate::decide::{Decision, Gate};
use crate::desired::Desired;
use crate::fleet::{Fleet, Marks};
use crate::services::{Services, ServicesError};
use crate::stages::{Advance, HostState, View};
use crate::testutil::{beat, decide_why, desired, id, last, native, role, unhealthy, updating};

/// The service of these tests: `app` on the role `farm`, which baking `bake_beats` beats, with
/// `extra` lines of its definition.
fn services(bake_beats: u32, extra: &str) -> Services {
    Services::parse(&format!(
        "[[service]]\nname = \"app\"\nroles = [\"farm\"]\nbake_beats = {bake_beats}\n{extra}\n"
    ))
    .expect("the services")
}

fn refusal(extra: &str) -> String {
    match Services::parse(&format!(
        "[[service]]\nname = \"app\"\nroles = [\"farm\"]\n{extra}\n"
    )) {
        Err(ServicesError::Invalid(why) | ServicesError::Parse(why)) => why,
        other => panic!("{extra}: refused, not {other:?}"),
    }
}

/// The desired state 2.0 of `app` with `max_updating` and `min_up` of the role `farm`.
fn want(max_updating: u32, min_up: u32) -> Desired {
    let mut want = desired("2.0", 0, 0);
    want.max_updating.set("farm", max_updating);
    want.min_up.set("farm", min_up);
    want
}

/// The host names `h01` to `h{n}`, in node-id order.
fn names(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("h{i:02}")).collect()
}

/// A host of the farm as the fake updater runs it.
#[derive(Clone, Debug, Default)]
struct Sim {
    running: String,
    /// The version it is updating to, and how many more beats it updates for.
    updating: Option<(String, u32)>,
    unhealthy: bool,
    /// The version it last failed, after its commit.
    failed: Option<String>,
}

/// A fleet of fake hosts of `app`, asked one round at a time.
struct Farm {
    services: Services,
    all: BTreeMap<String, Desired>,
    fleet: Fleet,
    hosts: BTreeMap<String, Sim>,
    /// How many beats an ordered host spends updating.
    takes: u32,
    at: u64,
}

impl Farm {
    fn new(services: Services, want: Desired, hosts: &[String]) -> Self {
        Self {
            services,
            all: BTreeMap::from([("app".to_owned(), want)]),
            fleet: Fleet::new(Duration::ZERO, crate::fleet::QUIET_START),
            hosts: hosts
                .iter()
                .map(|h| {
                    let sim = Sim {
                        running: "1.0".to_owned(),
                        ..Sim::default()
                    };
                    (h.clone(), sim)
                })
                .collect(),
            takes: 1,
            at: 100,
        }
        .heard()
    }

    /// The farm once every host has beaten, as a coordinator past its quiet start has heard them.
    fn heard(mut self) -> Self {
        let hosts: Vec<String> = self.hosts.keys().cloned().collect();
        for host in hosts {
            self.beat_of(&host);
        }
        self
    }

    fn now(&self) -> Duration {
        Duration::from_secs(self.at)
    }

    /// One heartbeat of `host` at the farm's clock, as it runs now.
    fn beat_of(&mut self, host: &str) {
        let sim = self.hosts.get_mut(host).expect("a host");
        if let Some((target, left)) = sim.updating.take() {
            if left == 0 {
                sim.running = target;
            } else {
                sim.updating = Some((target, left - 1));
            }
        }
        let mut b = beat(&sim.running);
        if let Some((target, _)) = &sim.updating {
            b = updating(b, target);
        }
        if let Some(failed) = &sim.failed {
            b = last(b, failed, Outcome::Failed);
        }
        if sim.unhealthy {
            b = unhealthy(b);
        }
        let now = self.now();
        self.fleet.record_in("app", id(host), role("farm"), b, now);
    }

    fn why(&self, host: &str) -> Decision {
        decide_why(
            &self.services,
            &self.all,
            "app",
            &self.fleet,
            &id(host),
            self.now(),
        )
    }

    /// Asks `host` after its beat, and notes an order as the coordinator does.
    fn ask(&mut self, host: &str) -> Decision {
        self.beat_of(host);
        let decision = self.why(host);
        if let Decision::Update { version } = &decision {
            let now = self.now();
            self.fleet.order("app", id(host), version.clone(), now);
            let takes = self.takes;
            let sim = self.hosts.get_mut(host).expect("a host");
            sim.updating = Some((version.as_str().to_owned(), takes));
        }
        decision
    }

    /// One round ten seconds after the last: every host beats and is asked, in node-id order;
    /// the hosts ordered.
    fn round(&mut self) -> Vec<String> {
        self.at += 10;
        let hosts: Vec<String> = self.hosts.keys().cloned().collect();
        hosts
            .into_iter()
            .filter(|h| matches!(self.ask(h), Decision::Update { .. }))
            .collect()
    }

    /// The plan as the decision reads it now.
    fn view(&self) -> View {
        let def = self.services.get("app").expect("app");
        View::effective(
            def,
            &self.all["app"],
            None,
            &native(&self.fleet, &self.all),
            self.now(),
        )
    }

    /// The hosts that are not up for `app` now: updating, ordered or unhealthy.
    fn not_up(&self) -> usize {
        let now = self.now();
        self.fleet
            .hosts_of("app")
            .filter(|(o, h)| h.is_live(now) && !self.fleet.is_up("app", o, h, now))
            .count()
    }

    fn running(&self, version: &str) -> usize {
        self.hosts.values().filter(|s| s.running == version).count()
    }
}

/// Proves: a plan of percents is read with the stages it says, a table stage with how it is
/// entered, and a definition with none has the default plan, 1, 10, 50 and 100, each entered by
/// itself.
#[test]
fn the_stages_of_the_rollout_file_and_the_default() {
    let plan = |extra: &str| -> Vec<(Option<u32>, Advance)> {
        services(0, extra)
            .get("app")
            .expect("app")
            .plan
            .defs()
            .iter()
            .map(|s| match s.share {
                crate::stages::Share::Percent(p) => (Some(p), s.advance),
                crate::stages::Share::Hosts(_) => (None, s.advance),
            })
            .collect()
    };
    let auto = |p| (Some(p), Advance::Auto);
    assert_eq!(plan(""), [auto(1), auto(10), auto(50), auto(100)]);
    assert_eq!(
        plan("stages = [5, { percent = 20, advance = \"manual\" }, { percent = 100 }]"),
        [auto(5), (Some(20), Advance::Manual), auto(100)]
    );
    assert_eq!(plan("stages = [100]"), [auto(100)]);
    assert_eq!(
        plan("canary = { count = 3 }\nstages = [50, 100]"),
        [(None, Advance::Auto), auto(50), auto(100)]
    );
}

/// Proves: the longest plan is 16 stages: sixteen percents, 1 to 15 and 100, are accepted and
/// give sixteen stages over 100 hosts, and seventeen are refused naming the limit.
/// Catches: a limit off by one in either direction.
#[test]
fn sixteen_stages_are_accepted_and_seventeen_refused() {
    let list = |upto: u32| -> String {
        let rising: String = (1..upto).map(|p| format!("{p}, ")).collect();
        format!("stages = [{rising}100]")
    };
    let def = services(0, &list(16));
    let plan = &def.get("app").expect("app").plan;
    assert_eq!(plan.defs().len(), 16);
    let sizes: Vec<usize> = plan.stages(100).iter().map(|s| s.hosts).collect();
    let mut want: Vec<usize> = (1..16).collect();
    want.push(100);
    assert_eq!(sizes, want);
    assert_eq!(refusal(&list(17)), "app: stages lists 1 to 16 stages");
}

/// Proves: a plan that cannot roll is refused, naming the service and what is wrong: no stage,
/// too many, a percent of 0 or over 100, percents that do not rise, a last stage short of 100,
/// a stage that is neither a percent nor a table of one, a table with a key it does not know,
/// and an `advance` that is neither `auto` nor `manual`.
#[test]
fn a_plan_that_cannot_roll_is_refused() {
    let many = format!(
        "stages = [{}100]",
        (1..=16).map(|p| format!("{p}, ")).collect::<String>()
    );
    let cases = [
        ("stages = []", "app: stages lists 1 to 16 stages"),
        (many.as_str(), "app: stages lists 1 to 16 stages"),
        (
            "stages = [0, 100]",
            "app: stages are percents from 1 to 100 that rise",
        ),
        (
            "stages = [50, 101]",
            "app: stages are percents from 1 to 100 that rise",
        ),
        (
            "stages = [50, 50, 100]",
            "app: stages are percents from 1 to 100 that rise",
        ),
        (
            "stages = [60, 10, 100]",
            "app: stages are percents from 1 to 100 that rise",
        ),
        ("stages = [10, 50]", "app: the last stage is 100"),
    ];
    for (extra, why) in cases {
        assert_eq!(refusal(extra), why, "{extra}");
    }
    let shape = "app: a stage is a percent, 10, or a table, { percent = 10, advance = \"auto\" } \
                 or \"manual\"";
    for extra in [
        "stages = [\"half\", 100]",
        "stages = [{ percent = 50, hosts = 3 }, 100]",
        "stages = [{ percent = 50, advance = \"later\" }, 100]",
        "stages = [-5, 100]",
    ] {
        assert_eq!(refusal(extra), shape, "{extra}");
    }
    assert!(
        refusal("stages = 100").contains("invalid type"),
        "not a list"
    );
}

/// Proves: a stage holds the ceiling of its percent of the eligible hosts, at least one and at
/// least the stage before, and stages of the same size collapse: on 8 hosts the default plan is
/// 1, 4 and 8; on 100 hosts 1, 10, 50 and 100; on 1 host one stage; on none, no stage. A canary of
/// 3 on 8 hosts is the stage of 3 before 50 and 100 percent: 3, 4 and 8.
#[test]
fn the_stage_sizes_over_the_eligible_hosts() {
    let sizes = |extra: &str, n: usize| -> Vec<usize> {
        services(0, extra)
            .get("app")
            .expect("app")
            .plan
            .stages(n)
            .iter()
            .map(|s| s.hosts)
            .collect()
    };
    assert_eq!(sizes("", 8), [1, 4, 8]);
    assert_eq!(sizes("", 100), [1, 10, 50, 100]);
    assert_eq!(sizes("", 2), [1, 2]);
    assert_eq!(sizes("", 6), [1, 3, 6]);
    assert_eq!(sizes("", 1), [1]);
    assert_eq!(sizes("", 0), Vec::<usize>::new());
    assert_eq!(sizes("canary = { count = 3 }", 8), [3, 4, 8]);
    assert_eq!(sizes("stages = [30, 50, 100]", 8), [3, 4, 8]);
    assert_eq!(sizes("canary = { count = 20 }", 8), [8]);
}

/// Proves: a manual stage that comes out the size of the stage before it adds no host and is
/// folded into it, and the next stage that adds hosts waits for the operator instead; a manual
/// stage that adds hosts is manual itself; the stages entered by entering a collapsed stage are
/// all those folded into it.
#[test]
fn a_manual_stage_folded_into_the_one_before_holds_the_next() {
    let def = services(
        0,
        "stages = [1, { percent = 10, advance = \"manual\" }, 50, 100]",
    );
    let plan = &def.get("app").expect("app").plan;
    let on_8: Vec<(usize, Advance, usize)> = plan
        .stages(8)
        .iter()
        .map(|s| (s.hosts, s.advance, s.through))
        .collect();
    assert_eq!(
        on_8,
        [
            (1, Advance::Auto, 2),
            (4, Advance::Manual, 3),
            (8, Advance::Auto, 4)
        ],
        "on 8 hosts the 10 percent stage adds none: the 50 percent stage waits"
    );
    let on_100: Vec<(usize, Advance)> = plan
        .stages(100)
        .iter()
        .map(|s| (s.hosts, s.advance))
        .collect();
    assert_eq!(
        on_100,
        [
            (1, Advance::Auto),
            (10, Advance::Manual),
            (50, Advance::Auto),
            (100, Advance::Auto)
        ]
    );
}

/// Proves (8 hosts, the default plan): the stages are 1, 4 and 8; no host of the second stage
/// is ordered before the first stage's host has baked; then the 3 hosts new in the second stage
/// are ordered in the same round, and the 4 of the third once those have baked.
/// Catches: a next stage started before the bake of the one before, and a stage's hosts ordered
/// one at a time.
#[test]
fn eight_hosts_roll_by_one_then_three_then_four_each_stage_after_the_bake() {
    let mut farm = Farm::new(services(2, ""), want(8, 0), &names(8));
    assert_eq!(farm.round(), ["h01"], "the first stage holds one host");
    let sizes: Vec<usize> = farm.view().stages.iter().map(|s| s.hosts).collect();
    assert_eq!(sizes, [1, 4, 8]);
    // h01 updates, then beats healthy at 2.0 twice: until then no host of stage 2 is ordered.
    assert_eq!(farm.round(), Vec::<String>::new(), "h01 is updating");
    assert_eq!(farm.why("h02"), Decision::Gate(Gate::StageWait));
    assert_eq!(
        farm.round(),
        Vec::<String>::new(),
        "h01 runs 2.0 and has beaten once"
    );
    assert_eq!(farm.why("h02"), Decision::Gate(Gate::Baking));
    assert_eq!(farm.view().current, 1);
    assert_eq!(
        farm.round(),
        ["h02", "h03", "h04"],
        "h01 baked: the second stage at once"
    );
    assert_eq!(farm.view().current, 2);
    assert_eq!(farm.why("h05"), Decision::Gate(Gate::StageWait));
    assert_eq!(farm.round(), Vec::<String>::new());
    assert_eq!(farm.round(), Vec::<String>::new());
    assert_eq!(farm.round(), ["h05", "h06", "h07", "h08"]);
    for _ in 0..3 {
        farm.round();
    }
    assert_eq!(farm.running("2.0"), 8);
    assert!(farm.view().finished());
}

/// Proves (100 hosts, the default plan): the stages are 1, 10, 50 and 100, and inside a stage
/// no more hosts hold an update slot at once than the role's `max_updating`, 4: the 9 hosts new
/// in the second stage go 4, 4 and 1, each set once the set before has finished.
/// Catches: a stage that orders all its new hosts past `max_updating`.
#[test]
fn a_hundred_hosts_roll_by_the_default_plan_capped_by_max_updating() {
    let mut farm = Farm::new(services(0, ""), want(4, 0), &names(100));
    let sizes: Vec<usize> = {
        farm.round();
        farm.view().stages.iter().map(|s| s.hosts).collect()
    };
    assert_eq!(sizes, [1, 10, 50, 100]);
    let mut per_round = Vec::new();
    for _ in 0..30 {
        if farm.running("2.0") >= 10 {
            break;
        }
        per_round.push(farm.round().len());
        assert!(
            farm.not_up() <= 4,
            "at most max_updating hosts updating: {}",
            farm.not_up()
        );
    }
    assert!(
        farm.running("2.0") >= 10,
        "the first two stages did not roll within 30 rounds"
    );
    // h01 updates, then the 9 go 4, 4 and 1, each set once the set before runs 2.0; the beat
    // that ends the bake of the tenth enters the third stage, whose first 4 go that round.
    assert_eq!(per_round, [0, 4, 0, 4, 0, 1, 0, 4]);
    assert_eq!(
        farm.why("h99"),
        Decision::Gate(Gate::NoSlot),
        "the third stage is entered"
    );
    let mut most = 0;
    for _ in 0..100 {
        if farm.running("2.0") >= 100 {
            break;
        }
        let ordered = farm.round().len();
        most = most.max(ordered);
        assert!(farm.not_up() <= 4);
    }
    assert_eq!(
        farm.running("2.0"),
        100,
        "the roll did not finish within 100 more rounds"
    );
    assert_eq!(most, 4);
}

/// Proves: with `min_up = 6` of 8 live hosts of the role, never more than 2 hosts are updating
/// or unhealthy at once, and a host whose order would leave fewer up is told `below_min_up`;
/// the roll still finishes.
/// Catches: `min_up` unread, or counted without the hosts that are unhealthy.
#[test]
fn min_up_holds_the_hosts_updating_or_unhealthy_to_live_less_min_up() {
    let mut farm = Farm::new(services(0, "stages = [100]"), want(8, 6), &names(8));
    farm.takes = 2;
    let mut held = false;
    for _ in 0..40 {
        farm.round();
        assert!(farm.not_up() <= 2, "{} hosts down", farm.not_up());
        held |= farm.not_up() == 2
            && farm.hosts.iter().any(|(h, s)| {
                s.running == "1.0"
                    && s.updating.is_none()
                    && farm.why(h) == Decision::Gate(Gate::BelowMinUp)
            });
    }
    assert!(held, "a host waited on below_min_up");
    assert_eq!(farm.running("2.0"), 8);
    // An unhealthy host counts as not up: with h08 sick, one order leaves 6 up and a second
    // would leave 5. Ordering h08 itself leaves the up count as it is, so it goes too.
    let mut farm = Farm::new(services(0, "stages = [100]"), want(8, 6), &names(8));
    farm.hosts.get_mut("h08").expect("h08").unhealthy = true;
    let mut farm = farm.heard();
    assert_eq!(farm.round(), ["h01", "h08"]);
    assert_eq!(farm.not_up(), 2);
    assert_eq!(farm.why("h02"), Decision::Gate(Gate::BelowMinUp));
}

/// Proves: a host excluded mid-stage no longer counts towards the stage (its place goes to the
/// next host to ask) nor towards the eligible hosts; a host newly assigned mid-plan is counted at
/// once, and joins the current stage at the size the stage has with it.
/// Catches: a plan that keeps counting a host it no longer orders, so the stage waits forever.
#[test]
fn the_eligible_hosts_change_mid_stage() {
    let mut farm = Farm::new(services(0, ""), want(8, 0), &names(8));
    farm.round();
    farm.round();
    assert_eq!(farm.round(), ["h02", "h03", "h04"], "stage 2 of 4 hosts");
    // h03 is excluded mid-update: the stage counts 3 hosts started and has a place again.
    farm.fleet.set_marks(Marks {
        excluded: [("app".to_owned(), "h03".to_owned())].into(),
        ..Marks::default()
    });
    let view = farm.view();
    assert_eq!(view.members.len(), 7);
    assert!(view.members.iter().all(|m| m.id.as_str() != "h03"));
    assert_eq!(
        view.stages.iter().map(|s| s.hosts).collect::<Vec<_>>(),
        [1, 4, 7]
    );
    assert_eq!(farm.round(), ["h05"], "h05 takes h03's place");
    // Two hosts are assigned: h09 makes 8 eligible and the stage of 50 percent still 4; h10
    // makes 9 and the stage 5, a place h10 itself takes as it asks.
    farm.hosts.insert(
        "h09".to_owned(),
        Sim {
            running: "1.0".to_owned(),
            ..Sim::default()
        },
    );
    farm.hosts.insert(
        "h10".to_owned(),
        Sim {
            running: "1.0".to_owned(),
            ..Sim::default()
        },
    );
    assert_eq!(farm.round(), ["h10"]);
    assert_eq!(farm.view().size(), 5);
}

/// Proves: `canary = { count = 3 }` gives the same orders, round by round, as the same plan
/// written as stages, `stages = [30, 50, 100]`, on 8 hosts; and so does a canary of one with the
/// default plan as the default plan alone.
/// Catches: a canary read as anything but a first stage of its hosts.
#[test]
fn the_canary_form_gives_the_same_orders_as_the_equivalent_stages() {
    let orders = |extra: &str| -> Vec<Vec<String>> {
        let mut farm = Farm::new(services(1, extra), want(8, 0), &names(8));
        (0..12).map(|_| farm.round()).collect()
    };
    let canary = orders("canary = { count = 3 }");
    assert_eq!(canary, orders("stages = [30, 50, 100]"));
    assert_eq!(canary[0], ["h01", "h02", "h03"]);
    assert_eq!(orders("canary = { count = 1 }"), orders(""));
}

/// Proves: a host that failed the desired version below the rollback threshold leaves the rest
/// of its stage going, but holds the next stage (`stage_failed`) until it is resolved: once an
/// operator holds it, the plan no longer counts it and the stage takes another host.
#[test]
fn a_failed_host_holds_the_next_stage_until_it_is_resolved() {
    let mut farm = Farm::new(services(0, ""), want(8, 0), &names(8));
    farm.round();
    farm.round();
    assert_eq!(farm.round(), ["h02", "h03", "h04"]);
    // h03 fails 2.0 and stays on 1.0; h02 and h04 finish.
    let sim = farm.hosts.get_mut("h03").expect("h03");
    sim.updating = None;
    sim.failed = Some("2.0".to_owned());
    farm.round();
    farm.round();
    assert_eq!(farm.why("h05"), Decision::Gate(Gate::StageFailed));
    let view = farm.view();
    let h03 = view
        .members
        .iter()
        .find(|m| m.id.as_str() == "h03")
        .expect("h03");
    assert_eq!(h03.state, HostState::Failed);
    farm.fleet.set_marks(Marks {
        holds: vec![crate::holds::HoldMark {
            service: "app".to_owned(),
            scope: crate::holds::HoldScope::Host("h03".to_owned()),
            reason: "bad disk".to_owned(),
            by: "operator".to_owned(),
            at_unix: 0,
        }],
        ..Marks::default()
    });
    assert_eq!(farm.round(), ["h05"], "the stage takes h05 in h03's place");
}
