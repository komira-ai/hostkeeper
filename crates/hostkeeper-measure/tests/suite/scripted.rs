//! Watches driven by a coordinator that answers a script, one answer per call in order, for the
//! states a simulated fleet does not reach: a host listed without a heartbeat, a host back but
//! not live, an update installed but failed, a service missing from a host; and the slots of a
//! step filled outside a run.

use std::collections::VecDeque;
use std::rc::Rc;

use hostkeeper_measure::coordinator::{Answer, CallError, Coordinator};
use hostkeeper_measure::fill;
use hostkeeper_measure::params::Params;
use hostkeeper_measure::plan::Plan;
use hostkeeper_measure::record::Record;
use hostkeeper_measure::runner::{self, Io, Names, RunError, Summary};
use serde_json::{Value, json};

use crate::support::{self, FakeClock, ok};

/// Answers each call with the next answer of its script, and records the calls.
struct Script {
    answers: VecDeque<Answer>,
    calls: Vec<String>,
}

impl Coordinator for Script {
    fn call(&mut self, args: &[String], _: Option<&Value>) -> Result<Answer, CallError> {
        self.calls.push(args.join(" "));
        Ok(self
            .answers
            .pop_front()
            .unwrap_or_else(|| support::refused(1, "script_ended")))
    }
}

fn hosts(hosts: Value) -> Answer {
    ok(json!({ "hosts": hosts }))
}

fn fail() -> Answer {
    Answer {
        code: 1,
        stdout: String::new(),
        stderr: String::new(),
    }
}

/// Runs `plan` against the script with a line for every operator step, and gives the records as
/// `step event host` lines.
fn run(
    plan: &Plan,
    params: &Params,
    answers: Vec<Answer>,
) -> (Result<Summary, RunError>, Vec<String>, Vec<String>) {
    let clock = Rc::new(FakeClock::default());
    let mut script = Script {
        answers: answers.into(),
        calls: Vec::new(),
    };
    let mut records = Vec::new();
    let mut sink = |r: &Record| {
        records.push(format!(
            "{} {} {}",
            r.step,
            r.event,
            r.host.as_deref().unwrap_or("-")
        ));
        Ok(())
    };
    let mut operator = std::io::Cursor::new(b"\n\n\n".to_vec());
    let result = runner::run(
        plan,
        params,
        Io {
            coordinator: &mut script,
            clock: &*clock,
            operator: &mut operator,
            console: &mut Vec::new(),
            records: &mut sink,
            names: &mut Names::default(),
        },
    );
    (result, records, script.calls)
}

const WATCH: &str = r#"
id = "scripted"
title = "Scripted"
measures = "M."
records = "R."
repetitions = 1
states = ["{n.max}"]

[hosts]
says = "Every live host."
pick = "live"

[[step]]
id = "w"
do = "watch"
until = UNTIL
poll_secs = 1
timeout_secs = 30

[[measure]]
id = "n"
kind = "count"
says = "events"
unit = "events"
of = "w/timed-out"
"#;

fn plan(until: &str) -> Plan {
    support::plan(&WATCH.replace("UNTIL", until))
}

fn host(node: &str, live: bool, beat: Option<u64>, services: Value) -> Value {
    let mut h = json!({"nodeId": node, "live": live, "services": services});
    if let Some(b) = beat {
        h["lastHeartbeatUnix"] = b.to_string().into();
    }
    h
}

#[test]
fn a_restart_counts_a_host_back_only_live_with_a_newer_heartbeat() {
    let a = |live, beat| host("a", live, beat, json!([]));
    let b = |live, beat| host("b", live, beat, json!([]));
    let c = |live, beat| host("c", live, beat, json!([]));
    let answers = vec![
        // The hosts at the start.
        hosts(json!([
            a(true, Some(10)),
            b(true, Some(10)),
            c(true, Some(10))
        ])),
        // c is listed without a heartbeat.
        hosts(json!([a(true, Some(10)), b(true, Some(10)), c(true, None)])),
        fail(),
        // a beats but is not live; b is live with no newer heartbeat; c is not listed.
        hosts(json!([a(false, Some(20)), b(true, Some(10))])),
        // a is back; b and c are not yet.
        hosts(json!([a(true, Some(20)), b(true, Some(10))])),
        // a again (already back), b and c back.
        hosts(json!([
            a(true, Some(20)),
            b(true, Some(25)),
            c(true, Some(30))
        ])),
    ];
    let (result, records, _) = run(&plan("{ what = \"back\" }"), &Params::default(), answers);
    result.expect("the run ends");
    assert_eq!(
        records,
        [
            "repetition started -",
            "w heartbeat host-1",
            "w heartbeat host-2",
            "w call-failed -",
            "w restart-seen -",
            "w answered -",
            "w heartbeat host-1",
            "w back host-1",
            "w heartbeat host-2",
            "w back host-2",
            "w heartbeat host-3",
            "w back host-3",
            "repetition ended -",
        ]
    );
}

#[test]
fn an_update_counts_only_installed_by_a_succeeded_update_to_that_version() {
    let svc = |installed: &str, target: &str, outcome: &str, slot: bool| {
        json!([{"service": "s", "installedVersion": installed, "holdsSlot": slot,
                "lastUpdate": {"target": target, "outcome": outcome}}])
    };
    let answers = vec![
        hosts(json!([
            host(
                "a",
                true,
                Some(1),
                svc("v1", "v1", "OUTCOME_SUCCEEDED", false)
            ),
            host("b", true, Some(1), json!([]))
        ])),
        // a: the version installed, but its last update was to another: not updated. b has no s.
        hosts(json!([
            host(
                "a",
                true,
                Some(2),
                svc("v2", "v1", "OUTCOME_SUCCEEDED", false)
            ),
            host("b", true, Some(2), json!([]))
        ])),
        // a: the update to the version failed.
        hosts(json!([
            host("a", true, Some(3), svc("v2", "v2", "OUTCOME_FAILED", false)),
            host("b", true, Some(3), json!([]))
        ])),
        // a: succeeded. b still has no s: the watch times out.
        hosts(json!([host(
            "a",
            true,
            Some(4),
            svc("v2", "v2", "OUTCOME_SUCCEEDED", false)
        )])),
    ];
    let (result, records, _) = run(
        &plan("{ what = \"updated\", service = \"s\", version = \"v2\" }"),
        &Params::default(),
        answers,
    );
    let summary = result.expect("the run ends");
    assert_eq!(summary.ended, vec![(1, "w timed out".to_owned())]);
    assert_eq!(
        records[..3],
        [
            "repetition started -",
            "w updated host-1",
            "w call-failed -"
        ]
    );
}

#[test]
fn a_health_watch_waits_for_a_host_that_does_not_have_the_service_yet() {
    let answers = vec![
        hosts(json!([host("a", true, Some(1), json!([]))])),
        hosts(json!([host("a", true, Some(2), json!([]))])),
        hosts(json!([host(
            "a",
            true,
            Some(3),
            json!([{"service": "s", "health": "HEALTH_HEALTHY"}])
        )])),
        hosts(json!([host(
            "a",
            true,
            Some(4),
            json!([{"service": "s", "health": "HEALTH_UNHEALTHY"}])
        )])),
    ];
    let (result, records, calls) = run(
        &plan("{ what = \"health\", service = \"s\", health = \"unhealthy\" }"),
        &Params::default(),
        answers,
    );
    result.expect("the run ends");
    assert_eq!(
        records,
        [
            "repetition started -",
            "w health host-1",
            "repetition ended -"
        ]
    );
    assert_eq!(calls.len(), 4);
}

#[test]
fn a_new_host_is_beating_once_both_its_stamps_are_listed_each_recorded_once() {
    let plan = support::plan(
        &WATCH
            .replace("pick = \"live\"", "pick = \"none\"")
            .replace(
                "[[step]]\nid = \"w\"",
                "[[step]]\nid = \"a\"\ndo = \"watch\"\nuntil = { what = \"audit\", actions = [\"enroll_issued\"] }\npoll_secs = 1\ntimeout_secs = 5\n\n[[step]]\nid = \"w\"",
            )
            .replace("UNTIL", "{ what = \"beating\" }"),
    );
    let new = |first_serving: Option<&str>| {
        let mut h = json!({"nodeId": "n", "firstHeartbeatUnix": "100"});
        if let Some(s) = first_serving {
            h["firstServingUnix"] = s.into();
        }
        ok(json!({ "host": h }))
    };
    let answers = vec![
        ok(json!({"entries": [], "nextSeq": "7"})),
        ok(
            json!({"entries": [{"seq": "8", "action": "enroll_issued", "nodeId": "n", "atUnix": "99"}], "nextSeq": "8"}),
        ),
        new(None),
        new(None),
        new(Some("130")),
    ];
    let (result, records, calls) = run(&plan, &Params::default(), answers);
    result.expect("the run ends");
    assert_eq!(
        records,
        [
            "repetition started -",
            "a seen host-1",
            "w first-heartbeat host-1",
            "w first-serving host-1",
            "repetition ended -",
        ]
    );
    assert_eq!(
        calls,
        [
            "audit list --newest true --limit 1",
            "audit list --since-seq 7 --limit 256",
            "hosts get --node-id n",
            "hosts get --node-id n",
            "hosts get --node-id n",
        ]
    );
}

#[test]
fn the_slots_of_a_step_are_filled_from_the_parameters_or_refused() {
    let scratch = support::scratch("fill");
    let index = scratch.path().join("index.json");
    std::fs::write(&index, "abc").expect("write the index");
    let params = support::params(&["v=1", &format!("i={}", index.display())]);
    let fill = |slot: &str, dry: bool| fill::fill(&params, slot, 3, Some("node"), dry);
    assert_eq!(fill("host", false).expect("host"), "node");
    assert_eq!(fill("host", true).expect("host"), "<host>");
    assert_eq!(fill("rep", false).expect("rep"), "3");
    assert_eq!(fill("v", false).expect("v"), "1");
    assert_eq!(fill("file:i", false).expect("file"), "abc");
    assert_eq!(
        fill("file:i", true).expect("file"),
        format!("<the text of {}>", index.display())
    );
    assert_eq!(
        fill("sha256:i", false).expect("digest"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        fill("A.B", false).expect_err("not a slot").to_string(),
        "{A.B} is not a slot of a step"
    );
    let absent = scratch.path().join("absent.json");
    let missing = support::params(&[&format!("i={}", absent.display())]);
    for value in [json!(["{file:i}"]), json!({"k": "{sha256:i}"})] {
        let err = fill::render_json(&missing, &value, 1, None, false).expect_err("no file");
        assert!(
            err.to_string()
                .starts_with(&format!("parameter \"i\": reading {}: ", absent.display())),
            "{err}"
        );
    }
    assert_eq!(
        fill::render_json(&params, &json!({"a": ["{v}", 2, null]}), 1, None, false).expect("json"),
        json!({"a": ["1", 2, null]})
    );
}

#[test]
fn a_live_watch_waits_for_a_host_listed_but_not_live() {
    let answers = vec![
        hosts(json!([host("a", true, Some(1), json!([]))])),
        hosts(json!([host("a", false, Some(1), json!([]))])),
        hosts(json!([host("a", true, Some(40), json!([]))])),
    ];
    let (result, records, calls) = run(&plan("{ what = \"live\" }"), &Params::default(), answers);
    result.expect("the run ends");
    assert_eq!(
        records,
        [
            "repetition started -",
            "w live host-1",
            "repetition ended -"
        ]
    );
    assert_eq!(calls.len(), 3);
}
