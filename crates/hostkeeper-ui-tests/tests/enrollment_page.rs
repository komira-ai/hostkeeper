//! The Enrollment page's view of the requests that expired, run in the JavaScript engine
//! against the fake gateway of `support`: the call it makes, what it shows of an unenrolled
//! host and why, and the buttons between the two views.

mod support;

use serde_json::json;
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

fn expired_reply() -> serde_json::Value {
    json!({"requests": [
        {"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32), "image": "debian-hk",
         "firstSeenUnix": "1800000000", "state": "ENROLLMENT_REQUEST_STATE_UNENROLLED",
         "unenrolledReason": "request_expired", "unenrolledUnix": "1800259201",
         "requestTtlSecs": "259200", "renewalOf": "web-3",
         "claims": {"machineId": "m-0001", "dmiSerial": "SN-42"}},
     ],
     "nextPageToken": "page-2"})
}

#[test]
fn the_expired_view_asks_for_unenrolled_requests_and_shows_why_without_approve() {
    // Catches: the view asking for the pending list, an unenrolled host shown without why or
    // when, an Approve offered for a request that can no longer be approved, and paging or the
    // way back that drops or keeps the view wrongly.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), expired_reply());
    let out = ui.run(&page_driver(
        "enrollment",
        r#"{ unenrolled: "yes" }"#,
        "[]",
        r#"
const approve = nodes(tree).filter((n) => ["approve", "host-id"].includes((n.attrs || {}).name)).length;
await click(tree, "next-page");
await click(tree, "show-pending");
return approve;
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [(
            m("Enrollment/List"),
            json!({"limit": 50, "pageToken": "", "unenrolled": true})
        )]
    );
    assert_eq!(out["out"], 0, "no Approve and no host id to type");
    let key = "ab".repeat(32);
    assert_shows(
        &out,
        &[
            "Enrollment: expired requests",
            "Unenrolled: the request expired. No operator approved it within 72 hours. \
             It cannot be approved; the host is listed pending again when it posts its key again.",
            key.as_str(),
            "First seen 2027-01-15 08:00:00 UTC",
            "Expired 2027-01-18 08:00:01 UTC",
            "Renewal of web-3",
            "m-0001",
            "SN-42",
            "Pending requests",
        ],
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([
            {"name": "enrollment", "params": {"unenrolled": "yes", "pageToken": "page-2"}},
            {"name": "enrollment", "params": {}},
        ])
    );
}

#[test]
fn the_pending_view_leads_to_the_expired_one_and_the_first_page_keeps_the_view() {
    // Catches: no way from the pending requests to the expired ones, the pending call changed by
    // the new view, and First page leaving the expired view.
    let mut ui = Ui::start();
    ui.reply(&m("Enrollment/List"), json!({}));
    ui.reply(&m("Enrollment/List"), json!({}));
    let pending = ui.run(&page_driver(
        "enrollment",
        "{}",
        "[]",
        r#"await click(tree, "show-unenrolled");"#,
    ));
    let expired = ui.run(&page_driver(
        "enrollment",
        r#"{ unenrolled: "yes", pageToken: "page-2" }"#,
        "[]",
        r#"await click(tree, "first-page");"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Enrollment/List"), json!({"limit": 50, "pageToken": ""})),
            (
                m("Enrollment/List"),
                json!({"limit": 50, "pageToken": "page-2", "unenrolled": true})
            ),
        ]
    );
    assert_shows(&pending, &["No host is waiting.", "Expired requests"]);
    assert_eq!(
        pending["seen"]["navigations"],
        json!([{"name": "enrollment", "params": {"unenrolled": "yes"}}])
    );
    assert_shows(&expired, &["No request expired."]);
    assert_eq!(
        expired["seen"]["navigations"],
        json!([{"name": "enrollment", "params": {"unenrolled": "yes"}}])
    );
}

#[test]
fn a_coordinator_that_does_not_expire_requests_shows_no_pending_request_as_expired() {
    // Catches: the pending requests of a coordinator that ignores `unenrolled` shown as expired
    // ones (it answers with its pending list, each request without a state).
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [{"requestId": "1f".repeat(32), "keySha256": "ab".repeat(32)}]}),
    );
    let out = ui.run(&page_driver(
        "enrollment",
        r#"{ unenrolled: "yes" }"#,
        "[]",
        "",
    ));
    assert_shows(&out, &["This coordinator does not expire requests."]);
    assert!(!shown(&out).contains(&"1f".repeat(32)), "{}", shown(&out));
}

#[test]
fn the_ttl_and_the_reason_read_as_words() {
    // Catches: a TTL shown in a unit it is not a whole number of, a singular unit pluralised, a
    // missing or bad TTL shown as a number, and an unknown reason dropped.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { ttlWords, unenrolledLine } from "./pages/enrollment.js";
export async function main() {
  return {
    ttl: ["259200", "3600", "5400", "60", "90", "1", "0", "", "x", undefined].map(ttlWords),
    other: unenrolledLine({ unenrolledReason: "something" }),
    none: unenrolledLine({}),
  };
}"#,
    );
    assert_eq!(
        out["ttl"],
        json!([
            "72 hours",
            "1 hour",
            "90 minutes",
            "1 minute",
            "90 seconds",
            "1 second",
            "its time",
            "its time",
            "its time",
            "its time"
        ])
    );
    assert_eq!(out["other"], "Unenrolled: something.");
    assert_eq!(out["none"], "Unenrolled: no reason given.");
}
