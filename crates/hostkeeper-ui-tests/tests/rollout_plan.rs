//! The update plan on the Rollout page: the rows that say what the plan is, where it stands and
//! why it waits, the table of its stages and hosts, and the Advance, Pause and Resume buttons,
//! run in the JavaScript engine against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{api_plan, bodies, m, page_driver, status_reply};

/// The names of the buttons that each service section offers, in section order.
const OFFERED: &str = r#"
const sections = nodes(tree).filter((n) => n.attrs["class"] === "service");
return sections.map((s) => ["advance", "pause", "resume"].filter(
  (name) => nodes(s).some((n) => n.attrs["name"] === name)));"#;

/// A plan of `stages` (percent, hosts, manual), in stage `current` of `eligible` hosts, with the
/// wait `waiting`; no stage has a host.
fn plan(current: u32, eligible: u32, waiting: &str, stages: &[(Option<u32>, u32, bool)]) -> Value {
    let stages: Vec<Value> = stages
        .iter()
        .enumerate()
        .map(|(at, (percent, hosts, manual))| {
            let mut stage = json!({
                "number": at + 1,
                "hosts": hosts,
                "advance": if *manual { "STAGE_ADVANCE_MANUAL" } else { "STAGE_ADVANCE_AUTO" },
            });
            if let Some(percent) = percent {
                stage["percent"] = json!(percent);
            }
            stage
        })
        .collect();
    json!({"version": "2.0.0", "current": current, "eligible": eligible, "waiting": waiting,
           "stages": stages})
}

/// A Status of `plans`, one service each, named `s0`, `s1` and so on; a null plan is a service
/// with none.
fn status_of(plans: &[Value]) -> Value {
    let services: Vec<Value> = plans
        .iter()
        .enumerate()
        .map(|(at, plan)| {
            let mut service = json!({"service": format!("s{at}"), "desired": {"version": "2.0.0"}});
            if !plan.is_null() {
                service["plan"] = plan.clone();
            }
            service
        })
        .collect();
    json!({"services": services})
}

fn run(ui: &mut Ui, then: &str, answers: &str) -> Value {
    ui.reply(&m("Services/List"), json!({"services": []}));
    ui.run(&page_driver("rollout", "{}", answers, then))
}

#[test]
fn rollout_page_lists_each_stage_with_its_hosts_and_their_states() {
    // Catches: a stage table that drops a host, shows a host under another stage, leaves out a
    // stage's percent, size or how it is entered, loses the stage the plan is in, or shows a
    // deferring host as an ordinary waiting one.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    let out = run(
        &mut ui,
        r#"return rows(tree).filter((r) => r.length === 7);"#,
        "[]",
    );
    assert_eq!(
        out["out"],
        json!([
            [
                "Stage", "Percent", "Hosts", "Enters", "Host", "Role", "State"
            ],
            ["1 (current)", "1%", "1", "auto", "web-1", "web", "done"],
            ["2", "50%", "4", "manual", "web-4", "web", "waiting"],
            ["", "", "", "", "web-5", "web", "waiting (deferring)"],
            ["3", "100%", "8", "auto", "-", "-", "-"],
            [
                "Stage", "Percent", "Hosts", "Enters", "Host", "Role", "State"
            ],
            ["1 (current)", "100%", "1", "auto", "web-9", "web", "baking"],
        ])
    );
}

#[test]
fn rollout_page_says_what_the_plan_is_where_it_stands_and_why_it_waits() {
    // Catches: a plan row that leaves out a stage's size, percent or manual mark, or the eligible
    // count; a canary stage given a percent; a stage count that is wrong before a manual first
    // stage is entered or when every host has rolled; a wait that names no stage where it has
    // one, or names one where it has none; a wait on other services' rollouts that does not name
    // them, or names some where the coordinator named none, or leaves out why each is live and on
    // which host where the coordinator says it; a pause shown without who and why; a
    // service with no plan shown with a table or a made-up row.
    let mut ui = Ui::start();
    let mut halted_and_paused = plan(
        2,
        8,
        "PLAN_WAIT_HALTED",
        &[(Some(50), 4, false), (Some(100), 8, false)],
    );
    halted_and_paused["paused"] = json!(true);
    halted_and_paused["pausedBy"] = json!("ops");
    halted_and_paused["pauseReason"] = json!("hold the roll");
    let mut finished = plan(
        2,
        3,
        "PLAN_WAIT_NONE",
        &[(Some(50), 2, false), (Some(100), 3, false)],
    );
    finished["finished"] = json!(true);
    let mut waiting_on = plan(
        1,
        4,
        "PLAN_WAIT_ROLLOUT_ELSEWHERE",
        &[(Some(100), 4, false)],
    );
    waiting_on["waitingOn"] = json!(["js-agent", "tierstore"]);
    let mut waiting_on_why = waiting_on.clone();
    waiting_on_why["waitingOnWhy"] = json!([
        {"service": "js-agent", "reason": "LIVE_REASON_BAKING", "nodeId": "db-5"},
        {"service": "tierstore", "reason": "LIVE_REASON_PLAN_UNDER_WAY"},
    ]);
    ui.reply(
        &m("Releases/Status"),
        status_of(&[
            plan(
                0,
                8,
                "PLAN_WAIT_MANUAL_ADVANCE",
                &[(Some(1), 1, true), (Some(100), 8, false)],
            ),
            plan(
                2,
                8,
                "PLAN_WAIT_BAKING",
                &[(None, 3, false), (Some(50), 4, true), (Some(100), 8, false)],
            ),
            plan(
                1,
                4,
                "PLAN_WAIT_FAILED_HOST",
                &[(Some(50), 2, false), (Some(100), 4, false)],
            ),
            plan(1, 4, "PLAN_WAIT_BELOW_MIN_UP", &[(Some(100), 4, false)]),
            plan(1, 4, "PLAN_WAIT_UPDATING", &[(Some(100), 4, false)]),
            plan(1, 4, "PLAN_WAIT_DEFERRING", &[(Some(100), 4, false)]),
            plan(
                1,
                4,
                "PLAN_WAIT_ROLLOUT_ELSEWHERE",
                &[(Some(100), 4, false)],
            ),
            waiting_on,
            waiting_on_why,
            plan(1, 4, "PLAN_WAIT_QUIET", &[(Some(100), 4, false)]),
            halted_and_paused,
            finished,
            plan(1, 4, "PLAN_WAIT_NONE", &[(Some(100), 4, false)]),
            Value::Null,
        ]),
    );
    let out = run(
        &mut ui,
        r#"
const sections = nodes(tree).filter((n) => n.attrs["class"] === "service");
return sections.map((s) => [cell(s, "Plan"), cell(s, "Stage"), cell(s, "Waiting")]);"#,
        "[]",
    );
    assert_eq!(
        out["out"],
        json!([
            [
                "1 (1%, manual), 8 (100%) hosts of 8 eligible",
                "0 of 2",
                "manual_advance, stage 1"
            ],
            [
                "3 (canary), 4 (50%, manual), 8 (100%) hosts of 8 eligible",
                "2 of 3",
                "baking, stage 2"
            ],
            [
                "2 (50%), 4 (100%) hosts of 4 eligible",
                "1 of 2",
                "failed_host, stage 1"
            ],
            [
                "4 (100%) hosts of 4 eligible",
                "1 of 1",
                "below_min_up, stage 1"
            ],
            [
                "4 (100%) hosts of 4 eligible",
                "1 of 1",
                "updating, stage 1"
            ],
            [
                "4 (100%) hosts of 4 eligible",
                "1 of 1",
                "deferring, stage 1"
            ],
            [
                "4 (100%) hosts of 4 eligible",
                "1 of 1",
                "rollout_elsewhere"
            ],
            [
                "4 (100%) hosts of 4 eligible",
                "1 of 1",
                "rollout_elsewhere on js-agent, tierstore"
            ],
            [
                "4 (100%) hosts of 4 eligible",
                "1 of 1",
                "rollout_elsewhere on js-agent (baking on db-5), tierstore (plan_under_way)"
            ],
            ["4 (100%) hosts of 4 eligible", "1 of 1", "quiet"],
            [
                "4 (50%), 8 (100%) hosts of 8 eligible",
                "2 of 2",
                "halted; paused by ops: hold the roll"
            ],
            [
                "2 (50%), 3 (100%) hosts of 3 eligible",
                "2 of 2, every host rolled",
                "none"
            ],
            ["4 (100%) hosts of 4 eligible", "1 of 1", "none"],
            ["none", "-", "-"],
        ])
    );
}

#[test]
fn a_service_with_no_plan_has_no_stage_table() {
    // Catches: a page that fails, or draws an empty stage table, for a service whose Status has
    // no plan.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_of(&[Value::Null]));
    let out = run(
        &mut ui,
        r#"return rows(tree).filter((r) => r[0] === "Stage" && r.length === 7).length;"#,
        "[]",
    );
    assert_eq!(out["out"], 0);
}

#[test]
fn each_plan_offers_the_buttons_that_fit_where_it_stands() {
    // Catches: an Advance offered to a plan that does not wait for an operator, a Pause offered
    // to a paused plan or a Resume to a running one, and a plan with no button to pause it.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    let out = run(&mut ui, OFFERED, "[]");
    assert_eq!(out["out"], json!([["advance", "pause"], ["resume"]]));

    let mut ui = Ui::start();
    ui.reply(
        &m("Releases/Status"),
        status_of(&[
            plan(1, 4, "PLAN_WAIT_BAKING", &[(Some(100), 4, false)]),
            Value::Null,
        ]),
    );
    let out = run(&mut ui, OFFERED, "[]");
    assert_eq!(out["out"], json!([["pause"], ["pause"]]));
}

#[test]
fn advance_asks_first_and_sends_the_service_and_the_reason() {
    // Catches: an Advance sent without a reason or after the operator said no, one that names
    // another method, service or body, and a page that does not refresh after it landed.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    let mut after = api_plan();
    after["current"] = json!(2);
    after["waiting"] = json!("PLAN_WAIT_UPDATING");
    ui.reply(&m("Rollouts/Advance"), json!({"plan": after}));
    let out = run(
        &mut ui,
        r#"
await click(tree, "advance");
type(tree, "reason", "canary looks fine");
await click(tree, "advance");
await click(tree, "advance");"#,
        "[false, true]",
    );
    assert_eq!(
        bodies(&ui)[3..],
        [(
            m("Rollouts/Advance"),
            json!({"service": "api", "reason": "canary looks fine"})
        )]
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Advance api into stage 2 of 3? Its hosts are ordered at their next heartbeats.",
            "Advance api into stage 2 of 3? Its hosts are ordered at their next heartbeats."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "Not advanced.",
            "Advanced api into stage 2 of 3."
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn advance_of_a_manual_first_stage_names_stage_one() {
    // Catches: a question that names stage 1 as 0 or 2 when no stage is entered yet.
    let mut ui = Ui::start();
    ui.reply(
        &m("Releases/Status"),
        status_of(&[plan(
            0,
            8,
            "PLAN_WAIT_MANUAL_ADVANCE",
            &[(Some(1), 1, true), (Some(100), 8, false)],
        )]),
    );
    let out = run(
        &mut ui,
        r#"
type(tree, "reason", "go");
await click(tree, "advance");"#,
        "[false]",
    );
    assert_eq!(
        out["seen"]["questions"],
        json!(["Advance s0 into stage 1 of 2? Its hosts are ordered at their next heartbeats."])
    );
    assert_eq!(bodies(&ui).len(), 3);
}

#[test]
fn pause_asks_first_and_sends_the_service_and_the_reason() {
    // Catches: a Pause sent without a reason or after the operator said no, or with another
    // method, service or body, and a page that does not refresh after it landed.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Rollouts/Pause"), json!({}));
    let out = run(
        &mut ui,
        r#"
await click(tree, "pause");
type(tree, "reason", "freeze");
await click(tree, "pause");
await click(tree, "pause");"#,
        "[false, true]",
    );
    assert_eq!(
        bodies(&ui)[3..],
        [(
            m("Rollouts/Pause"),
            json!({"service": "api", "reason": "freeze"})
        )]
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Pause the plan of api? No host that has not started is ordered; the updates under way finish.",
            "Pause the plan of api? No host that has not started is ordered; the updates under way finish."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "Not paused.",
            "Paused the plan of api."
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn resume_asks_first_and_sends_the_service_and_the_reason() {
    // Catches: a Resume sent without a reason or after the operator said no, or with another
    // method, service or body (the paused service is the second section), and a page that does
    // not refresh after it landed.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.reply(&m("Rollouts/Resume"), json!({}));
    let out = run(
        &mut ui,
        r#"
await click(tree, "resume");
type(tree, "reason", "audit done", 1);
await click(tree, "resume");
await click(tree, "resume");"#,
        "[false, true]",
    );
    assert_eq!(
        bodies(&ui)[3..],
        [(
            m("Rollouts/Resume"),
            json!({"service": "web", "reason": "audit done"})
        )]
    );
    assert_eq!(
        out["seen"]["questions"],
        json!([
            "Resume the plan of web? Its hosts are ordered again from where it stands.",
            "Resume the plan of web? Its hosts are ordered again from where it stands."
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give a reason first.",
            "Not resumed.",
            "Resumed the plan of web."
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 1);
}

#[test]
fn a_refusal_of_a_plan_call_is_shown_as_it_comes() {
    // Catches: a refusal that is reworded or hidden, and a page that refreshes as if the call
    // had landed.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.refuse(
        &m("Rollouts/Advance"),
        412,
        "FAILED_PRECONDITION",
        "not_waiting",
    );
    ui.refuse(
        &m("Rollouts/Pause"),
        412,
        "FAILED_PRECONDITION",
        "already_paused",
    );
    ui.refuse(
        &m("Rollouts/Resume"),
        412,
        "FAILED_PRECONDITION",
        "not_paused",
    );
    let out = run(
        &mut ui,
        r#"
type(tree, "reason", "go", 0);
type(tree, "reason", "go", 1);
await click(tree, "advance");
await click(tree, "pause");
await click(tree, "resume");"#,
        "[true, true, true]",
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "FAILED_PRECONDITION not_waiting",
            "FAILED_PRECONDITION already_paused",
            "FAILED_PRECONDITION not_paused"
        ])
    );
    assert_eq!(out["seen"]["refreshes"], 0);
}
