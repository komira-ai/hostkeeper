//! The whole app (`ui/app.js`) in a fake document, driven through its elements: what arrives
//! late from a previous sign-in, paging through the frame, and what the elements show after the
//! frame draws again.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{m, status_reply};

/// A driver that starts the app in a fake document kept as `globalThis.doc`, signs in with
/// `token` through the form, and opens `page` from the page list.
fn start(token: &str, page: &str) -> String {
    format!(
        r#"
import {{ fakeDocument, domNamed, fire }} from "../test/kit.js";
import {{ startApp }} from "./app.js";
export async function main() {{
  const doc = fakeDocument();
  globalThis.doc = doc;
  startApp({{ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true }});
  fire(domNamed(doc.root, "token"), "input", {{ target: {{ value: "{token}" }} }});
  fire(doc.root.children[0], "submit", {{ preventDefault() {{}} }});
  fire(domNamed(doc.root, "page-{page}"), "click", {{}});
}}"#
    )
}

/// A driver that runs `steps` (JavaScript with `doc`, `fire`, `domNamed`, `domTexts`, `elements`
/// and `getToken` in scope) and returns what they return.
fn steps(steps: &str) -> String {
    format!(
        r#"
import {{ domNamed, domTexts, elements, fire }} from "../test/kit.js";
import {{ getToken }} from "./session.js";
export async function main() {{
  const doc = globalThis.doc;
  const text = () => domTexts(doc.root).join(" ");
  const status = () => domTexts(elements(doc.root).find((e) => e.attributes["role"] === "status")).join("");
  const click = (name) => fire(domNamed(doc.root, name), "click", {{}});
  const type = (name, value) => fire(domNamed(doc.root, name), "input", {{ target: {{ value }} }});
  {steps}
}}"#
    )
}

/// The bodies sent to `method`, in order.
fn bodies_of(ui: &Ui, method: &str) -> Vec<Value> {
    ui.requests()
        .iter()
        .filter(|r| r.method() == method)
        .map(support::Request::body)
        .collect()
}

#[test]
fn a_late_answer_from_a_previous_sign_in_changes_nothing() {
    // Catches: a refusal of the old token that signs out the operator who has since signed in
    // with another, and an old page's success reported, or refreshed, under the new sign-in.
    let converged =
        json!({"convergence": {"desired": "1.4.0", "state": "CONVERGENCE_STATE_IN_PROGRESS"}});
    for (status, body) in [
        (
            401,
            json!({"code": "UNAUTHENTICATED", "reason": "unauthorized"}),
        ),
        (200, converged),
    ] {
        let mut ui = Ui::start();
        ui.reply(&m("Releases/Status"), status_reply());
        ui.reply(&m("Releases/Status"), status_reply());
        ui.hold(&m("Releases/Converge"), status, &body.to_string());
        ui.run(&start("tok_a", "rollout"));
        ui.run(&steps(
            r#"
click("converge");
click("sign-out");
type("token", "tok_b");
fire(doc.root.children[0], "submit", { preventDefault() {} });"#,
        ));
        let released = ui.run(&steps(
            r#"return gatewayRelease("hostkeeper.operator.v1.Releases/Converge");"#,
        ));
        assert_eq!(released, true);
        let after = ui.run(&steps(
            r#"return { token: getToken(), text: text(), status: status() };"#,
        ));
        assert_eq!(after["token"], "tok_b", "{status}");
        assert_eq!(after["status"], "", "{status}");
        let shown = after["text"].as_str().expect("the frame's text");
        assert!(
            shown.contains("Rollout") && shown.contains("Desired 1.4.0"),
            "{shown}"
        );
        assert!(!shown.contains("refused"), "{status}: {shown}");
        assert_eq!(
            bodies_of(&ui, &m("Releases/Status")).len(),
            2,
            "{status}: no extra refresh"
        );
    }
}

#[test]
fn audit_pages_follow_the_cursor_through_the_frame() {
    // Catches: a cursor rounded through a JavaScript number (sequence numbers above 2^53), a
    // filter dropped from a later page, entries shown out of order, a Newer control on the last
    // page, and a refused page shown as anything but its refusal.
    let mut ui = Ui::start();
    let (first, second) = ("9007199254740993", "9007199254740995");
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [{"seq": "4", "action": "hold"}], "nextSeq": "4"}),
    );
    ui.reply(
        &m("Audit/List"),
        json!({"entries": [
            {"seq": first, "atUnix": "1800000000", "by": "ACTOR_OPERATOR", "byName": "alice",
             "action": "hold", "service": "api", "nodeId": "web-1"},
            {"seq": second, "by": "ACTOR_CI", "action": "unhold", "service": "api", "nodeId": "web-1"},
        ], "nextSeq": second}),
    );
    ui.reply(&m("Audit/List"), json!({"nextSeq": second}));
    ui.refuse(&m("Audit/List"), 503, "UNAVAILABLE", "store_failed");
    ui.run(&start("tok_a", "audit"));
    let filtered = ui.run(&steps(
        r#"
type("service", "api");
click("show");"#,
    ));
    assert_eq!(filtered, Value::Null);
    let page = ui.run(&steps(
        r#"
const rows = elements(doc.root).filter((e) => e.tagName === "TR").map((r) => domTexts(r).join("|"));
return rows;"#,
    ));
    assert_eq!(
        page,
        json!([
            "Seq|Time|By|Who|Action|Service|Host|Reason",
            format!("{first}|2027-01-15 08:00:00 UTC|operator|alice|hold|api|web-1|-"),
            format!("{second}|-|ci|-|unhold|api|web-1|-"),
        ])
    );
    let last = ui.run(&steps(
        r#"
click("next-page");
await null;
return null;"#,
    ));
    assert_eq!(last, Value::Null);
    let terminal = ui.run(&steps(
        r#"
const names = elements(doc.root).map((e) => e.attributes["name"]);
return { text: text(), next: names.includes("next-page"), first: names.includes("first-page") };"#,
    ));
    assert!(
        terminal["text"]
            .as_str()
            .expect("text")
            .contains("No entry."),
        "{terminal}"
    );
    assert_eq!(terminal["next"], false);
    assert_eq!(terminal["first"], true);
    ui.run(&steps(r#"click("first-page");"#));
    let refused = ui.run(&steps(r#"return text();"#));
    assert!(
        refused
            .as_str()
            .expect("text")
            .contains("Audit UNAVAILABLE store_failed"),
        "{refused}"
    );
    assert_eq!(
        bodies_of(&ui, &m("Audit/List")),
        [
            json!({"sinceSeq": "0", "limit": 100}),
            json!({"sinceSeq": "0", "limit": 100, "service": "api"}),
            json!({"sinceSeq": second, "limit": 100, "service": "api"}),
            json!({"sinceSeq": "0", "limit": 100, "service": "api"}),
        ]
    );
}

fn pending(id: &str, key: &str) -> Value {
    json!({"requestId": id.repeat(32), "keySha256": key.repeat(32), "claims": {"machineId": format!("m-{id}")}})
}

#[test]
fn enrollment_pages_follow_the_token_to_the_last_page() {
    // Catches: a page token not sent, a Next page control on the last page, and a refused page
    // shown as anything but its refusal.
    let mut ui = Ui::start();
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [pending("1f", "ab")], "nextPageToken": "p2"}),
    );
    ui.reply(
        &m("Enrollment/List"),
        json!({"requests": [pending("2e", "cd")]}),
    );
    ui.refuse(&m("Enrollment/List"), 500, "INTERNAL", "store_failed");
    ui.run(&start("tok_a", "enrollment"));
    let first = ui.run(&steps(
        r#"
const shown = text();
click("next-page");
return shown;"#,
    ));
    assert!(first.as_str().expect("text").contains("m-1f"), "{first}");
    let last = ui.run(&steps(
        r#"
const names = elements(doc.root).map((e) => e.attributes["name"]);
const shown = { text: text(), next: names.includes("next-page"), first: names.includes("first-page") };
click("first-page");
return shown;"#,
    ));
    let text = last["text"].as_str().expect("text");
    assert!(text.contains("m-2e") && !text.contains("m-1f"), "{text}");
    assert_eq!(last["next"], false);
    assert_eq!(last["first"], true);
    let refused = ui.run(&steps(r#"return text();"#));
    assert!(
        refused
            .as_str()
            .expect("text")
            .contains("Enrollment INTERNAL store_failed"),
        "{refused}"
    );
    assert_eq!(
        bodies_of(&ui, &m("Enrollment/List")),
        [
            json!({"limit": 50, "pageToken": ""}),
            json!({"limit": 50, "pageToken": "p2"}),
            json!({"limit": 50, "pageToken": ""}),
        ]
    );
}

#[test]
fn the_elements_show_what_was_typed_after_the_frame_draws_again() {
    // Catches: a field, a checkbox or a select whose element, mounted again after a status
    // message, shows the old value while the page would send the new one.
    let mut ui = Ui::start();
    ui.reply(&m("Releases/Status"), status_reply());
    ui.run(&start("tok_a", "rollout"));
    let shown = ui.run(&steps(
        r#"
type("reason", "crash loop");
click("mark-bad");
fire(domNamed(doc.root, "force-include-held"), "change", { target: { checked: true } });
fire(domNamed(doc.root, "force-target"), "change", { target: { value: "role" } });
type("force-role", "web");
click("force-prepare");
await null;
const target = domNamed(doc.root, "force-target");
return {
  status: status(),
  reason: domNamed(doc.root, "reason").value,
  includeHeld: domNamed(doc.root, "force-include-held").checked,
  role: domNamed(doc.root, "force-role").value,
  target: target.children.map((o) => o.selected),
};"#,
    ));
    assert_eq!(
        shown,
        json!({
            "status": "Name the service, the version and a reason.",
            "reason": "crash loop",
            "includeHeld": true,
            "role": "web",
            "target": [false, true, false],
        })
    );
}
