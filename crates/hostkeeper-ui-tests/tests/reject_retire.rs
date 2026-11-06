//! The Reject button of the Enrollment page and the Retire button of the Host page, run in the
//! JavaScript engine against the fake gateway of `support`: each asks first, sends one call with
//! the body the proto reads, says what the answer means, and sends nothing for a reason that
//! breaks its rule or a question answered no.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

/// Two pending requests, as `Enrollment.List` answers them.
fn two_pending() -> Value {
    json!({"requests": [
        {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "claims": {"machineId": "m-1"}},
        {"requestId": "2e".repeat(32), "keySha256": "cd".repeat(32), "claims": {"machineId": "m-2"}},
    ]})
}

#[test]
fn enrollment_reject_asks_first_and_sends_the_listed_request_id() {
    // Catches: a rejection sent without the operator's yes, a body that names another request
    // or a field the proto does not read, a reason with a control character sent, an empty
    // reason refused (it is optional), a reason not trimmed, and a refusal taken as success.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), two_pending());
    ui.reply(
        &m("Enrollment/Reject"),
        json!({"entry": {"action": "enroll_reject"}}),
    );
    ui.refuse(&m("Enrollment/Reject"), 404, "NOT_FOUND", "unknown_request");
    let out = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[false, true, true]",
        r#"
type(tree, "reject-reason", "bad\treason", 0);
await click(tree, "reject", 0);
type(tree, "reject-reason", "", 0);
await click(tree, "reject", 0);
await click(tree, "reject", 0);
type(tree, "reject-reason", "  not ours  ", 1);
await click(tree, "reject", 1);
"#,
    ));
    let (one, two) = ("1f".repeat(32), "2e".repeat(32));
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (
                m("Enrollment/Reject"),
                json!({"requestId": one, "reason": ""})
            ),
            (
                m("Enrollment/Reject"),
                json!({"requestId": two, "reason": "not ours"})
            ),
        ]
    );
    let seen = &out["seen"];
    let asked = |id: &str| format!("Reject request {id}? Its key is refused from then on.");
    assert_eq!(
        seen["questions"],
        json!([asked(&one), asked(&one), asked(&two)])
    );
    assert_eq!(
        seen["reports"],
        json!([
            "A reason has no control character (a tab or a line break, for example).",
            "Not rejected.",
            format!("Rejected request {one}."),
            "NOT_FOUND unknown_request",
        ])
    );
    assert_eq!(
        seen["refreshes"], 1,
        "a refused rejection refreshes nothing"
    );
    assert_shows(&out, &["Reject", "Reason (optional)"]);
}

#[test]
fn host_retire_asks_first_and_says_whether_the_slot_waits() {
    // Catches: a retirement sent without the operator's yes, a body without the host or with a
    // field the proto does not read, a reason over 256 bytes sent, an answer with an update in
    // flight reported as a slot freed, and a Retire button missing for a host the coordinator
    // has not heard from since it started (a dead host is the one most often retired).
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "web-2", "role": "web"}}),
    );
    ui.reply(&m("Hosts/Retire"), json!({"updateInFlight": true}));
    ui.reply(&m("Hosts/Retire"), json!({}));
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[false, true, true]",
        r#"
type(tree, "retire-reason", "x".repeat(257));
await click(tree, "retire");
type(tree, "retire-reason", "");
await click(tree, "retire");
await click(tree, "retire");
type(tree, "retire-reason", "  board failed  ");
await click(tree, "retire");
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Hosts/Get"), json!({"nodeId": "web-2"})),
            (m("Services/List"), json!({})),
            (m("Hosts/Retire"), json!({"nodeId": "web-2", "reason": ""})),
            (
                m("Hosts/Retire"),
                json!({"nodeId": "web-2", "reason": "board failed"})
            ),
        ]
    );
    let seen = &out["seen"];
    let asked = "Retire web-2? Its heartbeats and renewals are refused from then on.";
    assert_eq!(seen["questions"], json!([asked, asked, asked]));
    assert_eq!(
        seen["reports"],
        json!([
            "A reason is at most 256 bytes; this one is 257.",
            "Not retired.",
            "Retired web-2: its update slot is freed when it reports its update final.",
            "Retired web-2.",
        ])
    );
    assert_eq!(seen["refreshes"], 2);

    let mut gone = Ui::start();
    gone.refuse(&m("Hosts/Get"), 404, "NOT_FOUND", "unknown_host");
    gone.reply(&m("Hosts/Retire"), json!({}));
    let out = gone.run(&page_driver(
        "host",
        r#"{ nodeId: "web-9" }"#,
        "[true]",
        r#"await click(tree, "retire");"#,
    ));
    assert_eq!(
        bodies(&gone).last(),
        Some(&(m("Hosts/Retire"), json!({"nodeId": "web-9", "reason": ""})))
    );
    assert_eq!(out["seen"]["reports"], json!(["Retired web-9."]));
}
