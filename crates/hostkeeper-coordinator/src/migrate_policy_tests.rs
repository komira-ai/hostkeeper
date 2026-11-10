//! The policy of a group made of the rollout file's services: agreement, and the most careful
//! value of each key where the services disagree.

use crate::host_groups::{Policy, PolicyCanary, PolicyStage};
use crate::migrate_policy::{Limits, Merged, merge, policy_text, stages_text};
use crate::reconcile::Rollback;
use crate::services::{ServiceDef, Services};
use crate::stages::Advance;

/// The services of the rollout file text `text`, by name.
fn defs(text: &str) -> Vec<ServiceDef> {
    let services = Services::parse(text).expect("a rollout file");
    services.defs().cloned().collect()
}

/// The merge of the services of `text` over `hosts` hosts, with no limits and the defaults of a
/// frontend.
fn merged(text: &str, hosts: usize) -> Merged {
    let all = defs(text);
    let refs: Vec<&ServiceDef> = all.iter().collect();
    merge(&refs, hosts, &[], (1, 0))
}

/// Two services on `web` whose keys `a` and `b` add to the table.
fn pair(a: &str, b: &str) -> String {
    format!(
        "[[service]]\nname = \"a\"\nroles = [\"web\"]\n{a}\n[[service]]\nname = \"b\"\nroles = [\"web\"]\n{b}\n"
    )
}

/// Proves: services that agree on every key give the policy of that key with no choice said; and
/// the default plan, written out in a file, is the policy's none.
///
/// Catches: a choice said where nothing was chosen, or a default plan written into every group.
#[test]
fn services_that_agree_give_their_keys_and_say_no_choice() {
    let text = "\
[[service]]
name = \"a\"
roles = [\"web\"]
bake_beats = 3
rollback = \"automatic\"
stages = [1, 10, 50, 100]
rollback_after_failed = { percent = 10, min_hosts = 4 }
defer_backoff = { initial_secs = 60, factor = 3, max_secs = 4000 }
[[service]]
name = \"b\"
roles = [\"web\"]
bake_beats = 3
rollback = \"automatic\"
rollback_after_failed = { percent = 10, min_hosts = 4 }
defer_backoff = { initial_secs = 60, factor = 3, max_secs = 4000 }
";
    let made = merged(text, 10);
    assert_eq!(made.choices, Vec::<String>::new());
    assert_eq!(made.policy.bake_beats, 3);
    assert_eq!(made.policy.rollback, Rollback::Automatic);
    assert!(made.policy.stages.is_empty(), "the default plan is none");
    assert_eq!(made.policy.rollback_after_failed.percent, 10);
    assert_eq!(made.policy.rollback_after_failed.min_hosts, 4);
    assert_eq!(made.policy.defer_backoff.initial_secs, 60);
    assert_eq!(made.policy.defer_backoff.factor, 3);
    assert_eq!(made.policy.defer_backoff.max_secs, 4000);
    assert_eq!(made.policy.check(), Ok(()));
}

/// Proves: the stages of a plan that holds a canary are the percent stages alone, whether or not
/// the canary's stage is skipped by count, and a plan with none has all of its stages.
///
/// Catches: a canary of hosts written as a percent stage, which would roll a different share.
#[test]
fn a_canary_stage_is_no_stage_of_a_policy() {
    use crate::canary::Canary;
    use crate::migrate_policy::stages_of;
    use crate::stages::Plan;
    let plan = Plan::read(None, Some(Canary { count: 2 })).expect("a plan");
    assert_eq!(plan.defs().len(), 5);
    assert_eq!(stages_of(&plan, 0), stages_of(&plan, 1));
    assert_eq!(stages_of(&plan, 0).len(), 4);
    assert_eq!(stages_of(&Plan::default(), 0).len(), 4);
}

/// Proves: a group whose services the file does not define has the default policy but the
/// limits it is given, and one service gives its keys alone.
///
/// Catches: a merge that needs a service, or one that drops the limits of a group with none.
#[test]
fn no_service_gives_the_default_policy_and_one_gives_its_own() {
    let none = merge(&[], 4, &[], (2, 0));
    assert_eq!(
        none.policy,
        Policy {
            max_updating: 2,
            ..Policy::default()
        }
    );
    assert_eq!(none.choices, Vec::<String>::new());
    let one = merged(
        "[[service]]\nname = \"a\"\nroles = [\"web\"]\nbake_beats = 7\n",
        4,
    );
    assert_eq!(one.policy.bake_beats, 7);
    assert_eq!(one.choices, Vec::<String>::new());
}

/// Proves: `bake_beats` is the highest of the services', and the choice says each service's value.
///
/// Catches: a group that bakes for the shortest service's time.
#[test]
fn bake_beats_is_the_highest() {
    let made = merged(&pair("bake_beats = 5", "bake_beats = 0"), 4);
    assert_eq!(made.policy.bake_beats, 5);
    assert_eq!(made.choices, ["  bake_beats 5: the highest of a 5, b 0"]);
}

/// Proves: `rollback` is manual when any service is manual, automatic only when every service is,
/// and the choice says each service's value.
///
/// Catches: a group rolled back by itself where one of its services asked for an operator.
#[test]
fn rollback_is_manual_over_automatic() {
    let made = merged(&pair("rollback = \"automatic\"", ""), 4);
    assert_eq!(made.policy.rollback, Rollback::Manual);
    assert_eq!(
        made.choices,
        ["  rollback manual: manual over automatic, of a automatic, b manual"]
    );
    let both = merged(
        &pair("rollback = \"automatic\"", "rollback = \"automatic\""),
        4,
    );
    assert_eq!(both.policy.rollback, Rollback::Automatic);
    assert_eq!(both.choices, Vec::<String>::new());
}

/// Proves: `rollback_after_failed` takes the lowest percent and the lowest `min_hosts`, each
/// chosen on its own and said on its own.
///
/// Catches: the two numbers taken from one service, so that neither is the lowest.
#[test]
fn rollback_after_failed_takes_the_lowest_of_each_number() {
    let made = merged(
        &pair(
            "rollback_after_failed = { percent = 50, min_hosts = 3 }",
            "rollback_after_failed = { percent = 40, min_hosts = 5 }",
        ),
        4,
    );
    assert_eq!(made.policy.rollback_after_failed.percent, 40);
    assert_eq!(made.policy.rollback_after_failed.min_hosts, 3);
    assert_eq!(
        made.choices,
        [
            "  rollback_after_failed percent 40: the lowest of a 50, b 40",
            "  rollback_after_failed min_hosts 3: the lowest of a 3, b 5",
        ]
    );
}

/// Proves: `defer_backoff` takes the longest of each field, each chosen and said on its own.
///
/// Catches: a group that re-offers a deferred update sooner than a service allowed.
#[test]
fn defer_backoff_takes_the_longest_of_each_field() {
    let made = merged(
        &pair(
            "defer_backoff = { initial_secs = 600, factor = 2, max_secs = 3600 }",
            "defer_backoff = { initial_secs = 300, factor = 4, max_secs = 7200 }",
        ),
        4,
    );
    assert_eq!(made.policy.defer_backoff.initial_secs, 600);
    assert_eq!(made.policy.defer_backoff.factor, 4);
    assert_eq!(made.policy.defer_backoff.max_secs, 7200);
    assert_eq!(
        made.choices,
        [
            "  defer_backoff initial_secs 600: the longest of a 600, b 300",
            "  defer_backoff factor 4: the longest of a 2, b 4",
            "  defer_backoff max_secs 7200: the longest of a 3600, b 7200",
        ]
    );
}

/// Proves: `stages` and `canary` are those of the service whose first stage holds the fewest of
/// the group's hosts: a canary counts its hosts (no more than the group has), a percent as the
/// plan rounds it up; a tie goes to the first by name; the percent stages are kept with how each
/// is entered, the canary's own stage left to `canary`.
///
/// Catches: a first stage larger than the careful service's, or a canary written twice.
#[test]
fn stages_and_canary_are_those_of_the_smallest_first_stage() {
    // Over 10 hosts a: 1 host (10%), b: a canary of 2.
    let made = merged(
        &pair(
            "stages = [10, 100]",
            "canary = { count = 2 }\nstages = [50, 100]",
        ),
        10,
    );
    assert_eq!(made.policy.canary, None);
    assert_eq!(
        made.policy.stages,
        [
            PolicyStage {
                percent: 10,
                advance: Advance::Auto
            },
            PolicyStage {
                percent: 100,
                advance: Advance::Auto
            },
        ]
    );
    assert_eq!(
        made.choices,
        [
            "  stages 10/100, canary none: those of a, whose first stage holds the fewest hosts (a 1, b 2)"
        ]
    );
    // Over 100 hosts a: 10 hosts, b: a canary of 2, which goes first with its own count.
    let made = merged(
        &pair(
            "stages = [10, 100]",
            "canary = { count = 2 }\nstages = [{ percent = 50, advance = \"manual\" }, 100]",
        ),
        100,
    );
    assert_eq!(made.policy.canary, Some(PolicyCanary { count: 2 }));
    assert_eq!(
        made.policy.stages,
        [
            PolicyStage {
                percent: 50,
                advance: Advance::Manual
            },
            PolicyStage {
                percent: 100,
                advance: Advance::Auto
            },
        ]
    );
    assert_eq!(
        made.choices,
        [
            "  stages 50 (manual)/100, canary 2: those of b, whose first stage holds the fewest hosts (a 10, b 2)"
        ]
    );
    // A canary larger than the group counts the group's hosts; the tie is a's.
    let made = merged(&pair("stages = [100]", "canary = { count = 9 }"), 3);
    assert_eq!(made.policy.canary, None);
    assert_eq!(
        made.policy.stages,
        [PolicyStage {
            percent: 100,
            advance: Advance::Auto
        }]
    );
}

/// Proves: the limits are the lowest `max_updating` and the highest `min_up` of the packages that
/// have a desired state, each disagreement said; one package's limits are the policy's without a
/// choice; with none the fallback stands.
///
/// Catches: limits that a package with a desired state of its own does not lower.
#[test]
fn limits_are_the_lowest_max_updating_and_the_highest_min_up() {
    let all = defs("[[service]]\nname = \"a\"\nroles = [\"web\"]\n");
    let refs: Vec<&ServiceDef> = all.iter().collect();
    let two = [
        Limits {
            package: "a",
            max_updating: 2,
            min_up: 1,
        },
        Limits {
            package: "b",
            max_updating: 3,
            min_up: 1,
        },
    ];
    let made = merge(&refs, 4, &two, (9, 9));
    assert_eq!((made.policy.max_updating, made.policy.min_up), (2, 1));
    assert_eq!(made.choices, ["  max_updating 2: the lowest of a 2, b 3"]);
    let up = [
        Limits {
            package: "a",
            max_updating: 2,
            min_up: 1,
        },
        Limits {
            package: "b",
            max_updating: 2,
            min_up: 4,
        },
    ];
    let made = merge(&refs, 4, &up, (9, 9));
    assert_eq!(made.choices, ["  min_up 4: the highest of a 1, b 4"]);
    let one = merge(&refs, 4, &up[..1], (9, 9));
    assert_eq!((one.policy.max_updating, one.policy.min_up), (2, 1));
    assert_eq!(one.choices, Vec::<String>::new());
}

/// Proves: the policy is said by its non-default keys, in the policy's order, and a policy that
/// changes none is `default`; the stages are said with how each is entered.
///
/// Catches: a policy line that hides a key the group will roll by.
#[test]
fn the_policy_is_said_by_its_non_default_keys() {
    assert_eq!(policy_text(&Policy::default()), "default");
    assert_eq!(stages_text(&[]), "default");
    let made = merged(
        "[[service]]\nname = \"a\"\nroles = [\"web\"]\nbake_beats = 2\nrollback = \"automatic\"\n\
         canary = { count = 1 }\nstages = [{ percent = 50, advance = \"manual\" }, 100]\n\
         rollback_after_failed = { percent = 10, min_hosts = 3 }\n\
         defer_backoff = { initial_secs = 60, factor = 2, max_secs = 3600 }\n",
        4,
    );
    let policy = Policy {
        max_updating: 2,
        min_up: 1,
        ..made.policy
    };
    assert_eq!(
        policy_text(&policy),
        "stages 50 (manual)/100, canary 1, bake_beats 2, rollback automatic, \
         rollback_after_failed 10%/3, defer_backoff 60/2/3600, max_updating 2, min_up 1"
    );
}
