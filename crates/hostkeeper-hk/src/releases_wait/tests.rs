//! [`wait_for`] against a simulated [`Fleet`]: each call's answer, how long it held and every pause
//! are the script's, on a clock of the script's own, so no test waits or reads a clock.

use std::io::Write;
use std::time::Duration;

use serde_json::{Value, json};

use super::{
    Asked, Ended, Fleet, Plan, RETRY, SLICE_SECS, Seen, host_line, number, span, table, utc,
    wait_for, words,
};
use crate::Flow;
use crate::call::Refused;

/// When the stand-in coordinator answers, by its clock: 2027-01-15T08:01:00Z.
const ANSWERED: u64 = 1_800_000_060;

/// A fleet on a simulated clock: `reply` answers each call from the clock and the seconds asked,
/// with how long the call took; a pause moves the clock by itself.
struct Sim<F: FnMut(Duration, u32) -> (Asked, Duration)> {
    clock: Duration,
    reply: F,
    asked: Vec<u32>,
    paused: Vec<Duration>,
    said: Vec<u8>,
}

impl<F: FnMut(Duration, u32) -> (Asked, Duration)> Sim<F> {
    fn new(reply: F) -> Self {
        Sim {
            clock: Duration::ZERO,
            reply,
            asked: Vec::new(),
            paused: Vec::new(),
            said: Vec::new(),
        }
    }

    fn said(&self) -> String {
        String::from_utf8_lossy(&self.said).into_owned()
    }
}

impl<F: FnMut(Duration, u32) -> (Asked, Duration)> Fleet for Sim<F> {
    fn ask(&mut self, secs: u32) -> Flow<Asked> {
        self.asked.push(secs);
        let (asked, took) = (self.reply)(self.clock, secs);
        self.clock += took;
        Ok(asked)
    }

    fn now(&mut self) -> Duration {
        self.clock
    }

    fn pause(&mut self, time: Duration) {
        self.paused.push(time);
        self.clock += time;
    }

    fn stderr(&mut self) -> &mut dyn Write {
        &mut self.said
    }
}

/// A wait for tierstore 0.3.212 in mac-build for `secs`.
fn plan(secs: u64) -> Plan<'static> {
    Plan {
        service: "tierstore",
        version: "0.3.212",
        host_group: Some("mac-build"),
        bound: Duration::from_secs(secs),
    }
}

/// The answer of `outcome` as the gateway writes it, with `hosts` not yet and the plan updating.
fn answer(outcome: &str, hosts: &[Value]) -> Vec<u8> {
    json!({
        "outcome": format!("WAIT_OUTCOME_{outcome}"),
        "convergence": {
            "desired": "0.3.212",
            "state": "CONVERGENCE_STATE_IN_PROGRESS",
            "roles": [{"role": "mac-build", "atDesired": 1, "updating": 1}],
        },
        "notYet": {
            "hosts": hosts,
            "plans": [{"hostGroup": "mac-build", "waiting": "PLAN_WAIT_UPDATING"}],
            "answeredUnix": ANSWERED.to_string(),
        },
    })
    .to_string()
    .into_bytes()
}

/// The Mac whose update to 0.3.212 never finishes, as the gateway writes it.
fn stuck() -> Value {
    json!({
        "nodeId": "mac-2",
        "role": "mac-build",
        "state": "WAITING_STATE_UPDATING",
        "installedVersion": "0.3.211",
        "updatingTo": "0.3.212",
        "step": "verify",
        "stepAttempt": 1,
    })
}

/// A not yet, with the stuck Mac.
fn not_yet() -> Vec<u8> {
    answer("DEADLINE", &[stuck()])
}

/// The converged answer.
fn converged() -> Vec<u8> {
    json!({"outcome": "WAIT_OUTCOME_CONVERGED", "convergence": {"state": "CONVERGENCE_STATE_CONVERGED"}})
        .to_string()
        .into_bytes()
}

/// The table the stuck Mac's not yet prints, under `head`.
fn block(head: &str) -> String {
    format!(
        "hk: releases wait: tierstore 0.3.212 in mac-build: {head}\n  state: in progress\n  mac-2 \
         (mac-build): updating to 0.3.212, step verify (try 1), on 0.3.211, seen so since \
         2027-01-15T08:01:00Z\n  the plan of mac-build waits: updating\n"
    )
}

/// The body `ended` answers with; a failure for any other end.
fn body(ended: Ended) -> Vec<u8> {
    match ended {
        Ended::Answered(body) => body,
        other => panic!("not an answer: {other:?}"),
    }
}

/// Proves: a fleet that converges after 25 minutes of simulated time is answered converged: 25
/// calls of 60 seconds held to their deadline, then one answered converged; the table is printed
/// once, since it did not change, and nothing more.
///
/// Catches: a wait that gives up before its bound, one call held past 60 seconds, or a table
/// printed at every call.
#[test]
fn a_fleet_that_converges_after_25_minutes_is_answered_converged() {
    let mut sim = Sim::new(|clock, secs| {
        if clock < Duration::from_secs(1500) {
            (Asked::Answer(not_yet()), Duration::from_secs(secs.into()))
        } else {
            (Asked::Answer(converged()), Duration::ZERO)
        }
    });
    let ended = wait_for(&mut sim, &plan(1800)).expect("ended");
    assert_eq!(body(ended), converged());
    assert_eq!(sim.asked, [vec![SLICE_SECS; 25], vec![SLICE_SECS]].concat());
    assert_eq!(sim.said(), block("not converged after 1m00s of 30m00s:"));
}

/// Proves: a fleet with one host stuck updating is answered, when the bound is spent, with the
/// last not yet, and the table that names the host, its state and since when is printed under a
/// line that says the fleet did not converge in time; the calls fill the bound exactly.
///
/// Catches: a wait that ends at its bound with nothing that names what it waited on.
#[test]
fn a_host_stuck_updating_is_named_with_its_state_and_since_when_at_the_bound() {
    let mut sim = Sim::new(|_, secs| (Asked::Answer(not_yet()), Duration::from_secs(secs.into())));
    let ended = wait_for(&mut sim, &plan(1800)).expect("ended");
    assert_eq!(body(ended), not_yet());
    assert_eq!(sim.asked, vec![SLICE_SECS; 30]);
    assert_eq!(sim.clock, Duration::from_secs(1800));
    assert_eq!(
        sim.said(),
        [
            block("not converged after 1m00s of 30m00s:"),
            block("did not converge within 30m00s; it still waits on:"),
        ]
        .concat()
    );
}

/// Proves: calls cut in mid-wait do not end it: each is said, the same question is asked again
/// after [`RETRY`], and the wait ends converged; the table printed before the cuts is not printed
/// again.
///
/// Catches: a cut connection that fails the step, a retry with no pause, or a retry that starts
/// the wait's time over.
#[test]
fn calls_cut_in_mid_wait_are_asked_again_and_the_wait_ends_converged() {
    let mut calls = 0;
    let mut sim = Sim::new(|clock, secs| {
        calls += 1;
        if calls == 3 || calls == 4 {
            let why =
                "no answer from https://coordinator.example (connection reset by peer)".to_owned();
            (Asked::Cut(why), Duration::from_secs(5))
        } else if clock < Duration::from_secs(1500) {
            (Asked::Answer(not_yet()), Duration::from_secs(secs.into()))
        } else {
            (Asked::Answer(converged()), Duration::ZERO)
        }
    });
    let ended = wait_for(&mut sim, &plan(1800)).expect("ended");
    assert_eq!(body(ended), converged());
    assert_eq!(sim.paused, [RETRY, RETRY]);
    assert_eq!(
        sim.said(),
        [
            block("not converged after 1m00s of 30m00s:"),
            "hk: releases wait: no answer after 2m05s (no answer from https://coordinator.example (connection reset \
             by peer)); asking again in 10s\n"
                .to_owned(),
            "hk: releases wait: no answer after 2m20s (no answer from https://coordinator.example (connection reset \
             by peer)); asking again in 10s\n"
                .to_owned(),
        ]
        .concat()
    );
}

/// Proves: when no call is answered the wait ends with the last call's reason once the bound is
/// spent, each call held for the time left and each pause cut to it.
///
/// Catches: a wait that retries past its bound, or one that ends with an answer it never had.
#[test]
fn a_wait_no_call_answers_ends_with_the_last_reason_at_the_bound() {
    let mut calls = 0;
    let mut sim = Sim::new(|_, _| {
        calls += 1;
        (Asked::Cut(format!("cut {calls}")), Duration::ZERO)
    });
    let ended = wait_for(&mut sim, &plan(25)).expect("ended");
    assert!(
        matches!(&ended, Ended::NoAnswer(why) if why == "cut 3"),
        "{ended:?}"
    );
    assert_eq!(sim.asked, [25, 15, 5]);
    assert_eq!(
        sim.paused,
        [RETRY, RETRY, Duration::from_secs(5)],
        "the last pause ends at the bound"
    );
}

/// Proves: the last call of a bound that is not a whole number of slices is held for the time
/// left; and an answer, then a cut call at the bound, ends with the answer, the final table saying
/// that the last call had none.
///
/// Catches: a wait that overruns its bound by up to a slice, or a final table that hides the cut.
#[test]
fn the_last_call_holds_the_time_left_and_a_last_cut_is_said() {
    let mut calls = 0;
    let mut sim = Sim::new(|_, secs| {
        calls += 1;
        if calls == 1 {
            (Asked::Answer(not_yet()), Duration::from_secs(secs.into()))
        } else {
            (
                Asked::Cut("reset".to_owned()),
                Duration::from_secs(secs.into()),
            )
        }
    });
    let ended = wait_for(&mut sim, &plan(90)).expect("ended");
    assert_eq!(body(ended), not_yet());
    assert_eq!(sim.asked, [60, 30]);
    assert_eq!(
        sim.paused,
        Vec::<Duration>::new(),
        "no pause when no time is left"
    );
    assert_eq!(
        sim.said(),
        [
            block("not converged after 1m00s of 1m30s:"),
            "hk: releases wait: no answer after 1m30s (reset)\n".to_owned(),
            block(
                "did not converge within 1m30s (the last call had no answer: reset); it still \
                 waits on:"
            ),
        ]
        .concat()
    );
}

/// Proves: a not yet answered at once (a coordinator that is stopping answers every held call so)
/// is followed by a pause of [`RETRY`], cut to the time left, before the next call.
///
/// Catches: a wait that calls as fast as it can for its whole bound.
#[test]
fn a_not_yet_answered_at_once_is_asked_again_after_a_pause() {
    let mut sim = Sim::new(|_, _| (Asked::Answer(not_yet()), Duration::ZERO));
    let ended = wait_for(&mut sim, &plan(25)).expect("ended");
    assert_eq!(body(ended), not_yet());
    assert_eq!(sim.asked, [25, 15, 5]);
    assert_eq!(sim.paused, [RETRY, RETRY, Duration::from_secs(5)]);
}

/// Proves: a refusal ends the wait at once with that refusal.
///
/// Catches: a refusal (a wrong token, too many waits) retried until the bound.
#[test]
fn a_refusal_ends_the_wait_at_once() {
    let mut sim = Sim::new(|_, _| {
        let refused = Refused {
            code: b"FAILED_PRECONDITION".to_vec(),
            reason: b"too_many_waits".to_vec(),
            status: 9,
        };
        (Asked::Refused(refused), Duration::ZERO)
    });
    let ended = wait_for(&mut sim, &plan(1800)).expect("ended");
    assert!(
        matches!(&ended, Ended::Refused(r) if r.reason == b"too_many_waits"),
        "{ended:?}"
    );
    assert_eq!(sim.asked, [60]);
    assert_eq!(sim.said(), "");
}

/// Proves: an end other than converged (halted here) is answered at once, its table printed under
/// what ended it; an answer that is no JSON ends the wait too, as an outcome not known.
///
/// Catches: a halted service waited on until the bound, or an end that names no host.
#[test]
fn an_end_that_is_not_converged_prints_its_table() {
    let failed = json!({
        "nodeId": "mac-1", "role": "mac-build", "state": "WAITING_STATE_FAILED",
        "installedVersion": "0.3.211", "reason": "disk_tainted", "sinceUnix": "1800000000",
    });
    let halted = answer("HALTED", &[failed]);
    let given = halted.clone();
    let mut sim = Sim::new(move |_, _| (Asked::Answer(given.clone()), Duration::ZERO));
    let ended = wait_for(&mut sim, &plan(1800)).expect("ended");
    assert_eq!(body(ended), halted);
    assert_eq!(
        sim.said(),
        "hk: releases wait: tierstore 0.3.212 in mac-build: ended halted after 0m00s:\n  state: \
         in progress\n  mac-1 (mac-build): failed (disk_tainted), on 0.3.211, its update started \
         2027-01-15T08:00:00Z by its clock\n  the plan of mac-build waits: updating\n"
    );

    let mut sim = Sim::new(|_, _| (Asked::Answer(b"<html>".to_vec()), Duration::ZERO));
    let plan = Plan {
        host_group: None,
        ..plan(60)
    };
    let ended = wait_for(&mut sim, &plan).expect("ended");
    assert_eq!(body(ended), b"<html>");
    assert_eq!(
        sim.said(),
        "hk: releases wait: tierstore 0.3.212: ended unspecified after 0m00s:\n  state: \
         unspecified\n"
    );
}

/// Proves: the table is printed each time it differs from the last one printed, and only then.
///
/// Catches: a table printed at every call, or a change of a host's state left unsaid.
#[test]
fn the_table_is_printed_when_it_changes() {
    let silent = json!({
        "nodeId": "mac-2", "role": "mac-build", "state": "WAITING_STATE_SILENT",
        "installedVersion": "0.3.211", "sinceUnix": "1800000000",
    });
    let answers = [
        not_yet(),
        not_yet(),
        answer("DEADLINE", std::slice::from_ref(&silent)),
        answer("DEADLINE", &[silent]),
        not_yet(),
    ];
    let mut calls = 0;
    let mut sim = Sim::new(|_, secs| {
        calls += 1;
        let at = (calls - 1).min(answers.len() - 1);
        (
            Asked::Answer(answers[at].clone()),
            Duration::from_secs(secs.into()),
        )
    });
    wait_for(&mut sim, &plan(300)).expect("ended");
    let said = sim.said();
    let heads: Vec<&str> = said.lines().filter(|l| l.starts_with("hk: ")).collect();
    assert_eq!(
        heads,
        [
            "hk: releases wait: tierstore 0.3.212 in mac-build: not converged after 1m00s of 5m00s:",
            "hk: releases wait: tierstore 0.3.212 in mac-build: not converged after 3m00s of 5m00s:",
            "hk: releases wait: tierstore 0.3.212 in mac-build: not converged after 5m00s of 5m00s:",
            "hk: releases wait: tierstore 0.3.212 in mac-build: did not converge within 5m00s; it \
             still waits on:",
        ],
        "printed at the first answer, at each change, and at the bound"
    );
    assert!(
        sim.said()
            .contains("  mac-2 (mac-build): silent since 2027-01-15T08:00:00Z, on 0.3.211\n")
    );
}

/// Proves: from a coordinator that does not list the hosts (no `notYet`), the table is each
/// role's counts, said to be all this coordinator gives.
///
/// Catches: an older coordinator's answer printed as a table with nothing in it.
#[test]
fn an_older_coordinators_answer_is_tabled_by_its_roles() {
    let older = json!({
        "outcome": "WAIT_OUTCOME_DEADLINE",
        "convergence": {"state": "CONVERGENCE_STATE_BLOCKED",
            "roles": [{"role": "mac-build", "atDesired": 2, "failed": 1, "down": 1}]},
    });
    assert_eq!(
        table(&older, &mut Seen::default()),
        [
            "state: blocked",
            "mac-build: 2 at the version, 0 updating, 1 failed, 1 silent (this coordinator does \
             not list its hosts)",
        ]
    );
}

/// Proves: what each state of a host says, with its since: the coordinator's when it gives one,
/// else the answer that first showed the host so, which a change of state moves; a plan's waiting
/// services and pause; the hosts left out; and an answer that lists none.
///
/// Catches: a host line that does not say the state, the reason or since when.
#[test]
fn each_state_of_a_host_says_what_it_is_and_since_when() {
    let mut seen = Seen::default();
    let line = |host: Value, seen: &mut Seen| host_line(&host, ANSWERED, seen);
    assert_eq!(
        line(
            json!({"nodeId": "a", "role": "g", "state": "WAITING_STATE_UPDATING", "updatingTo": "2"}),
            &mut seen
        ),
        "a (g): updating to 2, nothing installed, seen so since 2027-01-15T08:01:00Z"
    );
    assert_eq!(
        host_line(
            &json!({"nodeId": "a", "role": "g", "state": "WAITING_STATE_BEHIND",
                "installedVersion": "1", "reason": "no_slot"}),
            ANSWERED + 60,
            &mut seen
        ),
        "a (g): not ordered yet (no_slot), on 1, seen so since 2027-01-15T08:02:00Z",
        "a new state is seen since this answer"
    );
    for (host, want) in [
        (
            json!({"nodeId": "b", "role": "g", "state": "WAITING_STATE_FAILED", "installedVersion": "1"}),
            "b (g): failed, on 1, seen so since 2027-01-15T08:01:00Z",
        ),
        (
            json!({"nodeId": "c", "role": "g", "state": "WAITING_STATE_EXCLUDED", "reason": "disk swap",
                "installedVersion": "1"}),
            "c (g): excluded (disk swap), on 1",
        ),
        (
            json!({"nodeId": "d", "role": "g", "state": "WAITING_STATE_SILENT"}),
            "d (g): never heard since the coordinator started, nothing installed",
        ),
        (
            json!({"nodeId": "e", "role": "g", "state": "WAITING_STATE_SETTLING", "reason": "baking",
                "installedVersion": "2"}),
            "e (g): settling (baking), on 2, seen so since 2027-01-15T08:01:00Z",
        ),
        (
            json!({"nodeId": "f", "role": "g", "state": "WAITING_STATE_LATER", "installedVersion": "1"}),
            "f (g): later, on 1",
        ),
    ] {
        assert_eq!(line(host, &mut seen), want);
    }

    let answer = json!({
        "outcome": "WAIT_OUTCOME_DEADLINE",
        "convergence": {"state": "CONVERGENCE_STATE_IN_PROGRESS"},
        "notYet": {
            "hostsLeftOut": 3,
            "plans": [
                {"hostGroup": "a", "waiting": "PLAN_WAIT_ROLLOUT_ELSEWHERE", "waitingOn": ["web", "db"]},
                {"waiting": "PLAN_WAIT_PAUSED", "pauseReason": "freeze"},
            ],
        },
    });
    assert_eq!(
        table(&answer, &mut Seen::default()),
        [
            "state: in progress",
            "no host is listed",
            "and 3 more hosts not listed",
            "the plan of a waits: rollout elsewhere on web, db",
            "the plan waits: paused (paused: freeze)",
        ]
    );
}

/// Proves: the spellings of the table: a time span, a UTC time (a leap day among them), an enum
/// value's words, and a number written as a number or as a string.
#[test]
fn spans_times_words_and_numbers_are_spelled_as_the_table_says_them() {
    assert_eq!(span(Duration::from_secs(1244)), "20m44s");
    assert_eq!(span(Duration::from_secs(3661)), "1h01m01s");
    assert_eq!(utc(1_798_761_599), "2026-12-31T23:59:59Z");
    assert_eq!(utc(1_800_000_000), "2027-01-15T08:00:00Z");
    assert_eq!(utc(1_835_395_199), "2028-02-28T23:59:59Z");
    assert_eq!(utc(1_835_395_200), "2028-02-29T00:00:00Z");
    assert_eq!(words(None, "PLAN_WAIT_"), "unspecified");
    assert_eq!(words(Some("OTHER"), "PLAN_WAIT_"), "other");
    let value = json!({"a": 7, "b": "8", "c": true, "d": "x"});
    assert_eq!(
        ["a", "b", "c", "d", "e"].map(|key| number(&value, key)),
        [7, 8, 0, 0, 0]
    );
}
