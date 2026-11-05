//! The data layer (`ui/api.js`, `ui/session.js`) and the force confirmation, run in the
//! JavaScript engine against the fake gateway of `support`.

mod support;

use serde_json::{Value, json};
use support::Ui;

const LIST: &str = "hostkeeper.operator.v1.Hosts/List";
const FORCE: &str = "hostkeeper.operator.v1.Releases/Force";

/// A token no reply, message or page would show by chance. It is also a well-formed refusal
/// token (lower case, digits, `_`), so a gateway that echoed it as a reason would pass the
/// reason's grammar.
const TOKEN: &str = "tok_7f3a9c41e2b85d06";

/// A driver that signs in with `TOKEN`, calls `method` with `body` and returns what happened:
/// `{reply}` or `{error: {name, message, code, reason, httpStatus, signInRequired}}`, and
/// whether a token is still held.
fn call_driver(method: &str, body: &str) -> String {
    format!(
        r#"
import {{ api }} from "../test/kit.js";
import {{ getToken }} from "./session.js";
export async function main() {{
  const client = api("{TOKEN}");
  try {{
    const reply = await client.call("{method}", {body});
    return {{ reply, signedIn: getToken() !== null }};
  }} catch (e) {{
    return {{
      error: {{ name: e["name"], message: e.message, code: e.code, reason: e.reason,
               httpStatus: e.httpStatus, signInRequired: e.signInRequired, text: String(e) }},
      signedIn: getToken() !== null,
    }};
  }}
}}"#
    )
}

#[test]
fn call_sends_post_path_bearer_token_and_json_body() {
    // Catches: a wrong path, HTTP method or Authorization header, a missing content type, a body
    // that is not the request's JSON, and a reply not returned as sent.
    let mut ui = Ui::start();
    let hosts = json!({"hosts": [{"nodeId": "web-1", "role": "web", "live": true}]});
    ui.reply(LIST, hosts.clone());
    let out = ui.run(&call_driver(LIST, "{}"));
    assert_eq!(out["reply"], hosts);
    let requests = ui.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.url, "/hostkeeper.operator.v1.Hosts/List");
    assert_eq!(sent.http_method(), "POST");
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.body(), json!({}));
    assert_fetch_protections(sent);

    ui.reply(
        "hostkeeper.operator.v1.Hosts/Get",
        json!({"host": {"nodeId": "web-1"}}),
    );
    ui.run(&call_driver(
        "hostkeeper.operator.v1.Hosts/Get",
        r#"{ nodeId: "web-1" }"#,
    ));
    let requests = ui.requests();
    assert_eq!(requests[1].url, "/hostkeeper.operator.v1.Hosts/Get");
    assert_eq!(requests[1].body(), json!({"nodeId": "web-1"}));
    assert_fetch_protections(&requests[1]);
}

/// The options every request carries: no cookie or other credential, no cache, no redirect
/// followed, no referrer.
fn assert_fetch_protections(sent: &support::Request) {
    for (option, value) in [
        ("method", "POST"),
        ("credentials", "omit"),
        ("cache", "no-store"),
        ("redirect", "error"),
        ("referrerPolicy", "no-referrer"),
    ] {
        assert_eq!(sent.init[option], value, "{option}");
    }
    let headers: Vec<String> = sent.init["headers"]
        .as_object()
        .expect("headers")
        .keys()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    assert_eq!(headers, ["authorization", "content-type"]);
}

#[test]
fn a_redirect_is_an_error_that_carries_nothing_of_the_request() {
    // Catches: a redirect answer taken as a reply, or an error that names the request's headers.
    let mut ui = Ui::start();
    ui.reply_raw(LIST, 302, "Location: /elsewhere");
    let out = ui.run(&call_driver(LIST, "{}"));
    assert_eq!(
        out["error"],
        json!({"name": "ApiError", "message": "UNKNOWN", "code": "UNKNOWN", "reason": "",
               "httpStatus": 302, "signInRequired": false, "text": "ApiError: UNKNOWN"})
    );
    assert_eq!(out["signedIn"], true);
}

#[test]
fn a_refusal_that_holds_the_token_loses_its_reason() {
    // Catches: a reason or a code kept although it holds the token, or is part of it, so that
    // the token reaches the error and the page.
    let token = "tok_demo_42";
    for reason in [
        "failed_tok_demo_42",
        "tok_demo_42_again",
        "tok_demo_42",
        "demo",
        "tok",
    ] {
        let mut ui = Ui::start();
        ui.refuse(LIST, 412, "FAILED_PRECONDITION", reason);
        let out = ui.run(&format!(
            r#"
import {{ api }} from "../test/kit.js";
import {{ describeError }} from "./view.js";
export async function main() {{
  try {{ await api("{token}").call("{LIST}", {{}}); return "replied"; }}
  catch (e) {{ return {{ message: e.message, reason: e.reason, shown: describeError(e), text: String(e) }}; }}
}}"#
        ));
        assert_eq!(
            out,
            json!({"message": "FAILED_PRECONDITION", "reason": "",
                   "shown": "FAILED_PRECONDITION", "text": "ApiError: FAILED_PRECONDITION"}),
            "{reason}"
        );
    }
    let mut ui = Ui::start();
    ui.refuse(LIST, 400, "TOK_DEMO_42", "x_tok_demo_42");
    let out = ui.run(&format!(
        r#"
import {{ api }} from "../test/kit.js";
export async function main() {{
  try {{ await api("TOK_DEMO_42").call("{LIST}", {{}}); return "replied"; }}
  catch (e) {{ return [e.code, e.reason, e.message]; }}
}}"#
    ));
    assert_eq!(
        out,
        json!(["UNKNOWN", "x_tok_demo_42", "UNKNOWN: x_tok_demo_42"])
    );
}

#[test]
fn a_refusal_whose_body_cannot_be_read_still_says_what_its_status_says() {
    // Catches: a 401 or 403 whose body fails to read taken as a network failure, which would
    // keep the refused token.
    let cases: [(u16, &str, bool); 4] = [
        (401, "UNAUTHENTICATED", false),
        (403, "PERMISSION_DENIED", false),
        (500, "UNKNOWN", true),
        (200, "NETWORK", true),
    ];
    for (status, code, kept) in cases {
        let mut ui = Ui::start();
        ui.unreadable(LIST, status);
        let out = ui.run(&call_driver(LIST, "{}"));
        assert_eq!(
            json!([
                out["error"]["code"],
                out["error"]["reason"],
                out["error"]["httpStatus"],
                out["signedIn"]
            ]),
            json!([code, "", status, kept]),
            "{status}"
        );
    }
}

#[test]
fn call_before_sign_in_sends_nothing() {
    // Catches: a request that leaves without a token, or with an empty one.
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ api }} from "../test/kit.js";
import {{ signIn }} from "./session.js";
export async function main() {{
  const out = [];
  const client = api(null);
  for (const pasted of [null, "", "   "]) {{
    out.push(signIn(pasted));
    try {{ await client.call("{LIST}", {{}}); out.push("sent"); }}
    catch (e) {{ out.push([e.code, e.signInRequired, e.httpStatus]); }}
  }}
  return out;
}}"#
    ));
    let refused = json!(["SIGN_IN_REQUIRED", true, 0]);
    assert_eq!(out, json!([false, refused, false, refused, false, refused]));
    assert!(ui.requests().is_empty());
}

#[test]
fn a_bad_method_name_sends_nothing() {
    // Catches: a call that sends a name no gateway route could have.
    let mut ui = Ui::start();
    for name in [
        "Hosts/List",
        "hostkeeper.operator.v1.Hosts.List",
        "/hostkeeper.operator.v1.Hosts/List",
    ] {
        let out = ui.run(&call_driver(name, "{}"));
        assert_eq!(out["error"]["code"], "BAD_METHOD", "{name}");
    }
    assert!(ui.requests().is_empty());
}

#[test]
fn a_gateway_error_becomes_an_api_error_with_code_and_reason() {
    // Catches: a refusal whose code or reason is lost, or whose HTTP status is not kept, and a
    // refusal not about the token that signs the operator out.
    let mut ui = Ui::start();
    ui.refuse(FORCE, 412, "FAILED_PRECONDITION", "halted");
    let out = ui.run(&call_driver(FORCE, "{}"));
    assert_eq!(
        out,
        json!({
            "error": {"name": "ApiError", "message": "FAILED_PRECONDITION: halted",
                      "code": "FAILED_PRECONDITION", "reason": "halted", "httpStatus": 412,
                      "signInRequired": false, "text": "ApiError: FAILED_PRECONDITION: halted"},
            "signedIn": true,
        })
    );
}

#[test]
fn a_reply_that_breaks_the_gateway_shape_is_an_error_without_its_text() {
    // Catches: a body that is not a refusal read as one, text from it copied into the error, and
    // a success body that is not a JSON object returned as a reply.
    let cases: [(u16, &str, Value); 10] = [
        (500, "Internal Server Error", json!(["UNKNOWN", ""])),
        (
            503,
            r#"{"code":"unavailable now","reason":"Retry later"}"#,
            json!(["UNKNOWN", ""]),
        ),
        (
            400,
            r#"{"code":"INVALID_ARGUMENT","reason":"node_id must match [a-z]"}"#,
            json!(["INVALID_ARGUMENT", ""]),
        ),
        (404, r#"["NOT_FOUND"]"#, json!(["UNKNOWN", ""])),
        (200, "not json", json!(["BAD_REPLY", ""])),
        (200, "[1, 2]", json!(["BAD_REPLY", ""])),
        (200, "null", json!(["BAD_REPLY", ""])),
        (200, "5", json!(["BAD_REPLY", ""])),
        (200, "true", json!(["BAD_REPLY", ""])),
        (200, "\"hosts\"", json!(["BAD_REPLY", ""])),
    ];
    for (status, body, expected) in cases {
        let mut ui = Ui::start();
        ui.reply_raw(LIST, status, body);
        let out = ui.run(&call_driver(LIST, "{}"));
        assert_eq!(
            json!([out["error"]["code"], out["error"]["reason"]]),
            expected,
            "{status} {body}"
        );
        assert_eq!(out["error"]["httpStatus"], status, "{status} {body}");
        assert_eq!(out["signedIn"], true, "{status} {body}");
    }
    let mut ui = Ui::start();
    ui.reply_raw(LIST, 200, "");
    assert_eq!(ui.run(&call_driver(LIST, "{}"))["reply"], json!({}));
    let mut ui = Ui::start();
    ui.drop_request(LIST);
    let out = ui.run(&call_driver(LIST, "{}"));
    assert_eq!(
        json!([out["error"]["code"], out["error"]["httpStatus"]]),
        json!(["NETWORK", 0])
    );
}

#[test]
fn an_unauthenticated_reply_clears_the_token() {
    // Catches: a token that survives a 401 or 403, or an UNAUTHENTICATED or PERMISSION_DENIED
    // code, so that the next call still sends it.
    let refusals: [(u16, &str, &str); 5] = [
        (401, "UNAUTHENTICATED", "unauthorized"),
        (403, "PERMISSION_DENIED", "forbidden"),
        (400, "UNAUTHENTICATED", "unauthorized"),
        (400, "PERMISSION_DENIED", "forbidden"),
        (401, "", ""),
    ];
    for (status, code, reason) in refusals {
        let mut ui = Ui::start();
        if code.is_empty() {
            ui.reply_raw(LIST, status, "");
        } else {
            ui.refuse(LIST, status, code, reason);
        }
        let out = ui.run(&format!(
            r#"
import {{ api }} from "../test/kit.js";
export async function main() {{
  const client = api("{TOKEN}");
  const out = [];
  for (let i = 0; i < 2; i++) {{
    try {{ await client.call("{LIST}", {{}}); out.push("replied"); }}
    catch (e) {{ out.push([e.code, e.signInRequired]); }}
  }}
  return out;
}}"#
        ));
        let first = if code.is_empty() || status == 401 {
            "UNAUTHENTICATED"
        } else if status == 403 {
            "PERMISSION_DENIED"
        } else {
            code
        };
        assert_eq!(
            out,
            json!([[first, true], ["SIGN_IN_REQUIRED", true]]),
            "{status} {code}"
        );
        assert_eq!(
            ui.requests().len(),
            1,
            "{status} {code}: the second call sent nothing"
        );
    }
}

#[test]
fn the_token_is_not_in_any_error_message_or_rendered_text() {
    // Catches: an error that echoes the request's headers or the token, and a page or the frame
    // that shows it. Every failure path of the data layer is driven with a recognisable token,
    // and the whole app is run in a fake document through a refusal and a sign-out.
    let failures: [(u16, String); 9] = [
        (
            401,
            json!({"code": "UNAUTHENTICATED", "reason": format!("Bearer {TOKEN}")}).to_string(),
        ),
        (403, json!({"code": TOKEN, "reason": TOKEN}).to_string()),
        (
            412,
            json!({"code": "FAILED_PRECONDITION", "reason": TOKEN}).to_string(),
        ),
        (500, format!("authorization: Bearer {TOKEN}")),
        (501, support::UNIMPLEMENTED.to_owned()),
        (502, String::new()),
        (200, format!("Bearer {TOKEN}")),
        (200, "[]".to_owned()),
        (0, String::new()),
    ];
    for (status, body) in &failures {
        let mut ui = Ui::start();
        if *status == 0 {
            ui.drop_request(LIST);
        } else {
            ui.reply_raw(LIST, *status, body);
        }
        let out = ui.run(&call_driver(LIST, "{}"));
        assert!(out.get("error").is_some(), "{status} {body}: {out}");
        assert!(!out.to_string().contains(TOKEN), "{status} {body}: {out}");
    }
    let mut ui = Ui::start();
    let out = ui.run(&call_driver("not a method", "{}"));
    assert!(!out.to_string().contains(TOKEN), "{out}");

    // The app: sign in through the form, fail every page's first call, then be refused.
    let mut ui = Ui::start();
    for page in [
        "Hosts/List",
        "Hosts/Get",
        "Releases/Status",
        "Services/List",
        "Enrollment/List",
        "Audit/List",
    ] {
        ui.reply_raw(
            &format!("hostkeeper.operator.v1.{page}"),
            500,
            &json!({"code": "INTERNAL", "reason": TOKEN}).to_string(),
        );
    }
    ui.run(&format!(
        r#"
import {{ fakeDocument, domNamed, fire }} from "../test/kit.js";
import {{ startApp }} from "./app.js";
export async function main() {{
  const doc = fakeDocument();
  globalThis.doc = doc;
  startApp({{ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true }});
  fire(domNamed(doc.root, "token"), "input", {{ target: {{ value: "{TOKEN}" }} }});
  fire(doc.root.children[0], "submit", {{ preventDefault() {{}} }});
}}"#
    ));
    let shown = ui.run(
        r#"
import { domTexts, elements, domNamed, fire } from "../test/kit.js";
export async function main() {
  const doc = globalThis.doc;
  const pages = [];
  for (const name of ["host", "rollout", "holds", "enrollment", "inventory", "audit"]) {
    fire(domNamed(doc.root, `page-${name}`), "click", {});
    await null;
    pages.push(domTexts(doc.root).join(" "));
  }
  return pages;
}"#,
    );
    let after = ui.run(
        r#"
import { domTexts, elements } from "../test/kit.js";
export async function main() {
  const doc = globalThis.doc;
  const attributes = doc.created.map((e) => JSON.stringify(e.attributes));
  return { text: domTexts(doc.root).join(" "), attributes, values: doc.created.map((e) => String(e.value)) };
}"#,
    );
    assert!(
        after["text"].as_str().expect("text").contains("INTERNAL"),
        "{after}"
    );
    assert!(!shown.to_string().contains(TOKEN), "{shown}");
    assert!(!after.to_string().contains(TOKEN), "{after}");
    assert!(!ui.requests().is_empty());
    ui.refuse(
        "hostkeeper.operator.v1.Audit/List",
        401,
        "UNAUTHENTICATED",
        "unauthorized",
    );
    let refused = ui.run(
        r#"
import { domTexts, domNamed, fire } from "../test/kit.js";
export async function main() {
  fire(domNamed(globalThis.doc.root, "page-audit"), "click", {});
}"#,
    );
    assert_eq!(refused, Value::Null);
    let signed_out = ui.run(
        r#"
import { domTexts, domNamed } from "../test/kit.js";
import { getToken } from "./session.js";
export async function main() {
  const doc = globalThis.doc;
  return { token: getToken(), text: domTexts(doc.root).join(" "), form: domNamed(doc.root, "token").attributes };
}"#,
    );
    assert_eq!(signed_out["token"], Value::Null);
    let text = signed_out["text"].as_str().expect("text");
    assert!(text.contains("Paste your operator token"), "{text}");
    assert!(
        text.contains("The coordinator refused the token (UNAUTHENTICATED unauthorized)."),
        "{text}"
    );
    assert_eq!(signed_out["form"]["type"], "password");
    assert_eq!(signed_out["form"]["autocomplete"], "off");
    assert!(!signed_out.to_string().contains(TOKEN), "{signed_out}");
}

#[test]
fn a_page_that_fails_unexpectedly_says_so() {
    // Catches: a page whose rendering throws on a reply it did not expect, left on "Loading..."
    // with nothing shown.
    let mut ui = Ui::start();
    ui.reply_raw(LIST, 200, r#"{"hosts": 5}"#);
    ui.reply("hostkeeper.operator.v1.Releases/Status", json!({}));
    ui.run(&format!(
        r#"
import {{ fakeDocument, domNamed, fire }} from "../test/kit.js";
import {{ startApp }} from "./app.js";
export async function main() {{
  const doc = fakeDocument();
  globalThis.doc = doc;
  startApp({{ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true }});
  fire(domNamed(doc.root, "token"), "input", {{ target: {{ value: "{TOKEN}" }} }});
  fire(doc.root.children[0], "submit", {{ preventDefault() {{}} }});
}}"#
    ));
    let shown = ui.run(
        r#"
import { domTexts } from "../test/kit.js";
export async function main() {
  return domTexts(globalThis.doc.root).join(" ");
}"#,
    );
    let text = shown.as_str().expect("the frame's text");
    assert!(text.contains("Fleet the app failed unexpectedly"), "{text}");
    assert!(!text.contains("Loading..."), "{text}");
}

#[test]
fn signing_out_forgets_the_token_and_shows_sign_in() {
    // Catches: a Sign out that leaves the token held, or a frame that keeps showing a page.
    let mut ui = Ui::start();
    ui.reply(LIST, json!({"hosts": []}));
    ui.reply("hostkeeper.operator.v1.Releases/Status", json!({}));
    ui.run(&format!(
        r#"
import {{ fakeDocument, domNamed, fire }} from "../test/kit.js";
import {{ startApp }} from "./app.js";
export async function main() {{
  const doc = fakeDocument();
  globalThis.doc = doc;
  startApp({{ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true }});
  fire(domNamed(doc.root, "token"), "input", {{ target: {{ value: "{TOKEN}" }} }});
  fire(doc.root.children[0], "submit", {{ preventDefault() {{}} }});
}}"#
    ));
    let before = ui.run(
        r#"
import { domTexts, domNamed, fire } from "../test/kit.js";
import { getToken } from "./session.js";
export async function main() {
  const doc = globalThis.doc;
  const text = domTexts(doc.root).join(" ");
  const held = getToken() !== null;
  fire(domNamed(doc.root, "sign-out"), "click", {});
  return { text, held, after: getToken(), shown: domTexts(doc.root).join(" ") };
}"#,
    );
    assert!(
        before["text"].as_str().expect("text").contains("Hosts (0)"),
        "{before}"
    );
    assert_eq!(before["held"], true);
    assert_eq!(before["after"], Value::Null);
    let after = before["shown"].as_str().expect("text");
    assert!(after.contains("Paste your operator token"), "{after}");
    assert!(!after.contains("refused"), "{after}");
    assert_eq!(
        ui.methods(),
        [LIST, "hostkeeper.operator.v1.Releases/Status"]
    );
}

/// The hosts of the force tests: `api` installed on web-1, web-2 (held) and clone-1; web-3 and
/// db-1 do not report it installed.
fn hosts() -> Value {
    json!({"hosts": [
        {"nodeId": "web-1", "role": "web", "services": [{"service": "api", "installedVersion": "1.4.0"}]},
        {"nodeId": "web-2", "role": "web", "services": [{"service": "api", "installedVersion": "1.3.0",
            "holds": [{"scope": {"host": "web-2"}, "service": "api", "reason": "investigating"}]}]},
        {"nodeId": "web-3", "role": "web", "services": [{"service": "api"}]},
        {"nodeId": "db-1", "role": "db", "services": [{"service": "other", "installedVersion": "2.0.0"}]},
        {"nodeId": "clone-1", "role": "db", "services": [{"service": "api", "installedVersion": "1.4.0"}]},
    ]})
}

fn counts(ui: &mut Ui, include_held: bool) -> Value {
    ui.run(&format!(
        r#"
import {{ forceTargetCount }} from "./pages/rollout.js";
export async function main() {{
  const hosts = {hosts};
  const t = (target) => forceTargetCount(Object.assign({{ service: "api", includeHeld: {include_held} }}, target), hosts);
  return {{
    fleet: t({{ fleet: {{ confirmService: "api" }} }}),
    web: t({{ role: "web" }}),
    db: t({{ role: "db" }}),
    emptyRole: t({{ role: "cache" }}),
    blankRole: t({{ role: "" }}),
    listed: t({{ hosts: {{ nodeIds: ["web-2", "db-1", "ghost", "web-2", "web-3"] }} }}),
    noList: t({{ hosts: {{}} }}),
    none: t({{}}),
    otherService: forceTargetCount({{ service: "other", includeHeld: {include_held}, fleet: {{}} }}, hosts),
    noHosts: forceTargetCount({{ service: "api", fleet: {{}} }}, []),
  }};
}}"#,
        hosts = hosts()["hosts"]
    ))
}

#[test]
fn force_target_count_counts_fleet_role_and_listed_hosts() {
    // Catches: an off-by-one, a fleet or role count that includes hosts that do not run the
    // service, a list counted with duplicates or unknown hosts, and a count for no target.
    let mut ui = Ui::start();
    assert_eq!(
        counts(&mut ui, true),
        json!({"fleet": 3, "web": 2, "db": 1, "emptyRole": 0, "blankRole": 0, "listed": 3,
               "noList": 0, "none": 0, "otherService": 1, "noHosts": 0})
    );
}

#[test]
fn force_target_count_skips_held_hosts_unless_include_held() {
    // Catches: a held host counted without include_held, in each kind of target.
    let mut ui = Ui::start();
    assert_eq!(
        counts(&mut ui, false),
        json!({"fleet": 2, "web": 1, "db": 1, "emptyRole": 0, "blankRole": 0, "listed": 2,
               "noList": 0, "none": 0, "otherService": 1, "noHosts": 0})
    );
}

#[test]
fn force_is_not_sent_until_the_typed_count_matches() {
    // Catches: a force sent on a wrong, empty or non-numeric entry, one sent without a fresh
    // count, or one sent after the count failed.
    let mut ui = Ui::start();
    ui.reply(LIST, hosts());
    let reply = json!({"targeted": 1, "held": ["web-2"]});
    ui.reply(FORCE, reply.clone());
    ui.refuse(LIST, 503, "UNAVAILABLE", "store_failed");
    let out = ui.run(&format!(
        r#"
import {{ api }} from "../test/kit.js";
import {{ prepareForce, sendForce }} from "./pages/rollout.js";
export async function main() {{
  const client = api("{TOKEN}");
  const request = {{ service: "api", version: "1.3.0", reason: "bad build", role: "web", includeHeld: false }};
  const prepared = await prepareForce(client, request);
  const wrong = [];
  for (const typed of ["", " ", "abc", "2", "01", "1.0", "-1", "1 1", "+1", undefined, null]) {{
    wrong.push(await sendForce(client, prepared, typed));
  }}
  const right = await sendForce(client, prepared, " 1 ");
  const failed = await prepareForce(client, request);
  const afterFailure = [];
  for (const typed of ["", "0", "1", "undefined"]) {{
    afterFailure.push(await sendForce(client, failed, typed));
  }}
  const unprepared = await sendForce(client, null, "1");
  return {{ count: prepared.count, wrong, right, failedOk: failed.ok, afterFailure, unprepared }};
}}"#
    ));
    assert_eq!(out["count"], 1);
    assert!(
        out["wrong"]
            .as_array()
            .expect("a list")
            .iter()
            .all(Value::is_null),
        "{out}"
    );
    assert_eq!(out["right"], reply);
    assert_eq!(out["failedOk"], false);
    assert_eq!(out["afterFailure"], json!([null, null, null, null]));
    assert_eq!(out["unprepared"], Value::Null);
    assert_eq!(ui.methods(), [LIST, FORCE, LIST]);
    assert_eq!(
        ui.requests()[1].body(),
        json!({"service": "api", "version": "1.3.0", "reason": "bad build", "role": "web", "includeHeld": false})
    );
}
