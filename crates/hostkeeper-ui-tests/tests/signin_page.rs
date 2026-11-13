//! The sign-in and the forced change of password (`ui/app.js`, `ui/signin.js`, the sign-in routes
//! of `ui/api.js`), the whole app in a fake document over the fake gateway: what the forms send
//! and to which route, what they show for each refusal, and that nothing a person types is shown
//! or sent anywhere else.

mod support;

use serde_json::{Value, json};
use support::pages::m;
use support::{LOGIN, LOGOUT, PASSWORD, SESSION, Ui};

const PASSWORD_TYPED: &str = "correct horse battery staple";
const NEW_PASSWORD: &str = "a newer password, also long";
const CSRF: &str = "csrf_5b2e91c7d4a06f38";

/// A driver that starts the app in a fake document kept as `globalThis.doc` and lets it read the
/// session the browser holds.
const START: &str = r#"
import { fakeDocument, settle } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  globalThis.doc = doc;
  globalThis.theApp = startApp({ doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000 });
  await settle();
}"#;

/// A driver that runs `steps` (JavaScript with `doc`, `fire`, `domNamed`, `elements`, `settle`,
/// `text`, `names`, `click`, `type` and `submit` in scope) and returns what they return.
fn steps(steps: &str) -> String {
    format!(
        r#"
import {{ domNamed, domTexts, elements, fire, settle }} from "../test/kit.js";
export async function main() {{
  const doc = globalThis.doc;
  const text = () => domTexts(doc.root).join(" ");
  const names = () => elements(doc.root).map((e) => e.attributes["name"]).filter((n) => n !== undefined);
  const click = (name) => fire(domNamed(doc.root, name), "click", {{}});
  const type = (name, value) => fire(domNamed(doc.root, name), "input", {{ target: {{ value }} }});
  const submit = () => fire(doc.root.children[0], "submit", {{ preventDefault() {{}} }});
  {steps}
}}"#
    )
}

fn session_body(csrf: &str, must_change: bool, until: Option<u64>) -> String {
    let mut body = json!({"username": "alice", "csrf": csrf, "must_change": must_change});
    if let Some(until) = until {
        body["must_change_until"] = json!(until);
    }
    body.to_string()
}

/// Starts the app with no session and signs in as alice with `PASSWORD_TYPED`.
fn sign_in(ui: &mut Ui) {
    ui.run(START);
    ui.run(&steps(
        r#"
type("username", "alice");
type("password", "correct horse battery staple");
submit();
await settle();"#,
    ));
}

fn shown(ui: &mut Ui) -> Value {
    ui.run(&steps(r"return { text: text(), names: names() };"))
}

fn text_of(out: &Value) -> &str {
    out["text"].as_str().expect("the frame's text")
}

fn has(out: &Value, name: &str) -> bool {
    out["names"]
        .as_array()
        .expect("names")
        .iter()
        .any(|n| n == name)
}

/// Whether anything of the fake document (text, attributes, values) holds `secret`.
fn document_holds(ui: &mut Ui, secret: &str) -> bool {
    let all = ui.run(
        r#"
import { domTexts, elements } from "../test/kit.js";
export async function main() {
  const doc = globalThis.doc;
  return JSON.stringify([domTexts(doc.root), elements(doc.root).map((e) => [e.attributes, String(e.value)])]);
}"#,
    );
    all.as_str().expect("the document as text").contains(secret)
}

#[test]
fn a_reload_with_a_live_session_shows_the_pages_without_a_form() {
    // Catches: a reload that asks for the password again although the browser holds a session,
    // a session read with the wrong route or method, and a CSRF token that does not reach the
    // calls the pages make.
    let mut ui = Ui::start();
    ui.session(CSRF);
    ui.reply(&m("Hosts/List"), json!({"hosts": []}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.run(START);
    let out = shown(&mut ui);
    assert!(has(&out, "home") && has(&out, "sign-out"), "{out}");
    assert!(!has(&out, "username") && !has(&out, "password"), "{out}");
    assert!(text_of(&out).contains("operator"), "{out}");
    let requests = ui.requests();
    assert_eq!(requests[0].url, "/v1/session");
    assert_eq!(requests[0].http_method(), "GET");
    assert_eq!(requests[0].header("x-hk-csrf"), None);
    assert_eq!(requests[0].init["credentials"], "same-origin");
    for page_call in &requests[1..] {
        assert_eq!(page_call.header("x-hk-csrf"), Some(CSRF));
        assert_eq!(page_call.header("authorization"), None);
    }
}

#[test]
fn a_reload_with_no_session_shows_the_sign_in_form_and_no_token_box() {
    // Catches: the pasted-token form still shown, a refusal of the session read shown as an
    // error to a person who has only not signed in yet, and a call sent before anyone signed in.
    let mut ui = Ui::start();
    ui.no_session();
    ui.run(START);
    let out = shown(&mut ui);
    let text = text_of(&out);
    assert!(
        text.contains("Sign in with your user name and password."),
        "{text}"
    );
    assert!(!text.contains("token") && !text.contains("Paste"), "{text}");
    assert!(has(&out, "username") && has(&out, "password") && has(&out, "sign-in"));
    assert!(!has(&out, "token") && !has(&out, "home"), "{out}");
    assert!(!has(&out, "sign-out"), "{out}");
    assert!(!text.contains("session"), "no error is shown: {text}");
    assert_eq!(ui.methods(), [SESSION]);
}

#[test]
fn a_session_read_that_gets_no_answer_says_so_on_the_form() {
    // Catches: a coordinator that is down shown as a plain sign-in form, or as a blank page.
    let mut ui = Ui::start();
    ui.drop_request(SESSION);
    ui.run(START);
    let out = shown(&mut ui);
    assert!(
        text_of(&out).contains("The coordinator did not answer."),
        "{out}"
    );
    assert!(has(&out, "password"));
}

#[test]
fn signing_in_posts_the_name_and_password_and_shows_the_pages() {
    // Catches: a login sent to the wrong route or with a bearer, the CSRF token of a session
    // that does not exist yet, a password in any other request, a session that is not kept from
    // the answer, and a form that stays after the sign-in.
    let mut ui = Ui::start();
    ui.no_session();
    ui.reply_raw(LOGIN, 200, &session_body(CSRF, false, None));
    ui.reply(&m("Hosts/List"), json!({"hosts": []}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    sign_in(&mut ui);
    let out = shown(&mut ui);
    assert!(has(&out, "home") && !has(&out, "password"), "{out}");
    assert!(text_of(&out).contains("alice"), "{out}");
    let requests = ui.requests();
    let login = requests
        .iter()
        .find(|r| r.url == "/v1/login")
        .expect("login");
    assert_eq!(login.http_method(), "POST");
    assert_eq!(login.header("content-type"), Some("application/json"));
    assert_eq!(login.header("x-hk-csrf"), None);
    assert_eq!(login.header("authorization"), None);
    assert_eq!(login.init["credentials"], "same-origin");
    assert_eq!(
        login.body(),
        json!({"username": "alice", "password": PASSWORD_TYPED})
    );
    for other in requests.iter().filter(|r| r.url != "/v1/login") {
        let sent = other.init.to_string();
        assert!(!sent.contains(PASSWORD_TYPED), "{}: {sent}", other.url);
    }
    let page_call = requests
        .iter()
        .find(|r| r.url.ends_with("Hosts/List"))
        .expect("a page call");
    assert_eq!(page_call.header("x-hk-csrf"), Some(CSRF));
    assert!(!document_holds(&mut ui, PASSWORD_TYPED));
    assert!(!document_holds(&mut ui, CSRF));
}

#[test]
fn each_refusal_of_a_sign_in_is_worded_and_the_name_is_kept() {
    // Catches: a refusal shown with the coordinator's own text, one that empties the user name,
    // a password left in the form, a session made from a refused login, and a locked or expired
    // account worded as a wrong password.
    let cases: [(u16, &str, &str); 7] = [
        (
            401,
            "invalid_credentials",
            "The user name or password is wrong.",
        ),
        (
            401,
            "initial_password_expired",
            "The initial password has expired. Ask an admin for a new one.",
        ),
        (
            429,
            "try_later",
            "Too many attempts, or the coordinator is busy. Try again in a few minutes.",
        ),
        (400, "bad_request", "Enter a user name and a password."),
        (
            403,
            "csrf",
            "The coordinator did not take the request as coming from this page. Reload the page.",
        ),
        (
            503,
            "store_unreachable",
            "The coordinator could not do that. Try again.",
        ),
        (
            500,
            "something_new",
            "The coordinator could not do that. Try again.",
        ),
    ];
    for (status, token, words) in cases {
        let mut ui = Ui::start();
        ui.no_session();
        ui.refuse_route(LOGIN, status, token);
        sign_in(&mut ui);
        let out = shown(&mut ui);
        assert!(text_of(&out).contains(words), "{token}: {out}");
        assert!(!text_of(&out).contains(token), "{token}: {out}");
        assert!(has(&out, "password") && !has(&out, "home"), "{token}");
        let kept = ui.run(&steps(r#"return domNamed(doc.root, "username").value;"#));
        assert_eq!(kept, "alice", "{token}");
        assert!(!document_holds(&mut ui, PASSWORD_TYPED), "{token}");
    }
    let mut ui = Ui::start();
    ui.no_session();
    ui.drop_request(LOGIN);
    sign_in(&mut ui);
    let out = shown(&mut ui);
    assert!(
        text_of(&out).contains("The coordinator did not answer."),
        "{out}"
    );
}

#[test]
fn an_answer_that_is_not_a_session_signs_nobody_in() {
    // Catches: a 200 with no CSRF token taken as a sign-in, which would leave pages that send
    // nothing.
    let mut ui = Ui::start();
    ui.no_session();
    ui.reply_raw(LOGIN, 200, r#"{"username":"alice"}"#);
    sign_in(&mut ui);
    let out = shown(&mut ui);
    assert!(
        text_of(&out).contains("The coordinator's answer was not a session."),
        "{out}"
    );
    assert!(has(&out, "password") && !has(&out, "home"));
}

#[test]
fn a_second_submit_while_the_first_is_on_its_way_sends_nothing() {
    // Catches: two logins from one press repeated, which would count a wrong password twice.
    let mut ui = Ui::start();
    ui.no_session();
    ui.hold(LOGIN, 401, r#"{"error":"invalid_credentials"}"#);
    ui.run(START);
    ui.run(&steps(
        r#"
type("username", "alice");
type("password", "wrong password here");
submit();
submit();
await settle();"#,
    ));
    let sent = ui.methods().iter().filter(|m| *m == LOGIN).count();
    assert_eq!(sent, 1);
    let released = ui.run(&steps(
        r#"return [gatewayRelease("v1/login"), gatewayRelease("v1/login")];"#,
    ));
    assert_eq!(released, json!([true, false]));
}

#[test]
fn a_first_sign_in_shows_only_the_form_of_a_new_password() {
    // Catches: pages, or any call to the operator API, before the password is changed; a form
    // that does not say until when the temporary password works; and a form with no way out.
    let mut ui = Ui::start();
    ui.no_session();
    ui.reply_raw(LOGIN, 200, &session_body(CSRF, true, Some(1_800_003_600)));
    sign_in(&mut ui);
    let out = shown(&mut ui);
    let text = text_of(&out);
    assert!(text.contains("Choose a new password"), "{text}");
    assert!(
        text.contains("alice is signed in with a temporary password"),
        "{text}"
    );
    assert!(
        text.contains("until 2027-01-15 09:00:00 UTC"),
        "the deadline is shown: {text}"
    );
    for name in ["current", "new", "again", "change-password", "sign-out"] {
        assert!(has(&out, name), "{name}");
    }
    assert!(!has(&out, "home") && !has(&out, "username"), "{out}");
    assert_eq!(ui.methods(), [SESSION, LOGIN]);
}

#[test]
fn a_new_password_is_sent_with_the_csrf_token_and_the_pages_follow() {
    // Catches: a change sent without the CSRF token or to another route, a body that is not
    // {current, new}, a confirmation field sent along, a session that is not replaced by the
    // answer (the pages would stay shut), and a password left in the document.
    let mut ui = Ui::start();
    ui.no_session();
    ui.reply_raw(LOGIN, 200, &session_body(CSRF, true, Some(1_800_003_600)));
    ui.reply_raw(PASSWORD, 200, &session_body(CSRF, false, None));
    ui.reply(&m("Hosts/List"), json!({"hosts": []}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    sign_in(&mut ui);
    ui.run(&steps(
        r#"
type("current", "correct horse battery staple");
type("new", "a newer password, also long");
type("again", "a newer password, also long");
submit();
await settle();"#,
    ));
    let out = shown(&mut ui);
    assert!(has(&out, "home") && !has(&out, "change-password"), "{out}");
    let requests = ui.requests();
    let change = requests
        .iter()
        .find(|r| r.url == "/v1/password")
        .expect("a change");
    assert_eq!(change.http_method(), "POST");
    assert_eq!(change.header("x-hk-csrf"), Some(CSRF));
    assert_eq!(
        change.body(),
        json!({"current": PASSWORD_TYPED, "new": NEW_PASSWORD})
    );
    assert!(!document_holds(&mut ui, NEW_PASSWORD));
    assert!(!document_holds(&mut ui, PASSWORD_TYPED));
}

#[test]
fn two_new_passwords_that_differ_send_nothing() {
    // Catches: a typo in the confirmation that sets a password nobody typed.
    let mut ui = Ui::start();
    ui.no_session();
    ui.reply_raw(LOGIN, 200, &session_body(CSRF, true, None));
    sign_in(&mut ui);
    ui.run(&steps(
        r#"
type("current", "correct horse battery staple");
type("new", "a newer password, also long");
type("again", "a newer password, also lonG");
submit();
await settle();"#,
    ));
    let out = shown(&mut ui);
    assert!(
        text_of(&out).contains("The two new passwords are not the same."),
        "{out}"
    );
    assert!(has(&out, "change-password"));
    assert_eq!(ui.methods(), [SESSION, LOGIN]);
    assert!(
        !text_of(&out).contains("can be exchanged until"),
        "no deadline was sent"
    );
}

#[test]
fn a_refused_change_says_why_and_stays_on_the_form() {
    // Catches: a wrong current password worded as a wrong user name, a policy rule dropped or
    // shown with the password in it, and a refusal that signs the person out.
    let cases: [(u16, Value, &str); 4] = [
        (
            401,
            json!({"error": "invalid_credentials"}),
            "The current password is wrong.",
        ),
        (
            400,
            json!({"error": "password_policy", "reason": "at least 12 characters"}),
            "The new password is not allowed: at least 12 characters.",
        ),
        (
            400,
            json!({"error": "password_policy", "reason": format!("not {NEW_PASSWORD}")}),
            "The new password is not allowed.",
        ),
        (
            429,
            json!({"error": "try_later"}),
            "Too many attempts, or the coordinator is busy. Try again in a few minutes.",
        ),
    ];
    for (status, body, words) in cases {
        let mut ui = Ui::start();
        ui.no_session();
        ui.reply_raw(LOGIN, 200, &session_body(CSRF, true, None));
        ui.reply_raw(PASSWORD, status, &body.to_string());
        sign_in(&mut ui);
        ui.run(&steps(
            r#"
type("current", "correct horse battery staple");
type("new", "a newer password, also long");
type("again", "a newer password, also long");
submit();
await settle();"#,
        ));
        let out = shown(&mut ui);
        assert!(text_of(&out).contains(words), "{body}: {out}");
        assert!(
            has(&out, "change-password") && has(&out, "sign-out"),
            "{body}"
        );
        assert!(!document_holds(&mut ui, NEW_PASSWORD), "{body}");
        assert!(!document_holds(&mut ui, PASSWORD_TYPED), "{body}");
    }
}

#[test]
fn an_expired_initial_password_ends_the_session_and_says_so() {
    // Catches: a form that stays after the coordinator will no longer take the change, and a
    // sign-out that is not told to the coordinator.
    let mut ui = Ui::start();
    ui.no_session();
    ui.reply_raw(LOGIN, 200, &session_body(CSRF, true, Some(1_800_003_600)));
    ui.refuse_route(PASSWORD, 401, "initial_password_expired");
    ui.reply_raw(LOGOUT, 200, "{}");
    sign_in(&mut ui);
    ui.run(&steps(
        r#"
type("current", "correct horse battery staple");
type("new", "a newer password, also long");
type("again", "a newer password, also long");
submit();
await settle();"#,
    ));
    let out = shown(&mut ui);
    assert!(
        text_of(&out).contains("The initial password has expired. Ask an admin for a new one."),
        "{out}"
    );
    assert!(
        has(&out, "username") && !has(&out, "change-password"),
        "{out}"
    );
    let requests = ui.requests();
    let logout = requests.last().expect("a request");
    assert_eq!(logout.url, "/v1/logout");
    assert_eq!(logout.header("x-hk-csrf"), Some(CSRF));
}

#[test]
fn a_session_that_ended_during_the_change_returns_to_sign_in() {
    // Catches: a form left on screen for a session the coordinator no longer holds.
    for token in ["sign_in_required", "csrf"] {
        let mut ui = Ui::start();
        ui.no_session();
        ui.reply_raw(LOGIN, 200, &session_body(CSRF, true, None));
        ui.refuse_route(PASSWORD, if token == "csrf" { 403 } else { 401 }, token);
        sign_in(&mut ui);
        ui.run(&steps(
            r#"
type("current", "correct horse battery staple");
type("new", "a newer password, also long");
type("again", "a newer password, also long");
submit();
await settle();"#,
        ));
        let out = shown(&mut ui);
        assert!(
            has(&out, "username") && !has(&out, "change-password"),
            "{token}"
        );
    }
}

#[test]
fn a_call_refused_for_a_new_password_shows_that_form_not_the_sign_in() {
    // Catches: a session the coordinator holds but gates, such as one whose password an admin
    // has since reset, sent back to the sign-in form where the same login would only loop.
    let mut ui = Ui::start();
    ui.session(CSRF);
    ui.refuse(
        &m("Hosts/List"),
        403,
        "PERMISSION_DENIED",
        "password_change_required",
    );
    ui.run(START);
    let out = shown(&mut ui);
    let text = text_of(&out);
    assert!(text.contains("Choose a new password"), "{text}");
    assert!(
        has(&out, "change-password") && !has(&out, "username"),
        "{out}"
    );
    assert!(
        !text.contains("can be exchanged until"),
        "no deadline is known: {text}"
    );
}

#[test]
fn a_sign_out_the_coordinator_cannot_be_told_of_says_so() {
    // Catches: a sign-out that looks complete when the session still lives on the coordinator,
    // and a message for a session that was already gone.
    let mut ui = Ui::start();
    ui.session(CSRF);
    ui.drop_request(LOGOUT);
    ui.run(START);
    ui.run(&steps(r#"click("sign-out"); await settle();"#));
    let out = shown(&mut ui);
    assert!(has(&out, "username") && !has(&out, "sign-out"), "{out}");
    assert!(
        text_of(&out).contains("The coordinator did not confirm the sign-out"),
        "{out}"
    );

    let mut ui = Ui::start();
    ui.session(CSRF);
    ui.refuse_route(LOGOUT, 401, "sign_in_required");
    ui.run(START);
    ui.run(&steps(r#"click("sign-out"); await settle();"#));
    let out = shown(&mut ui);
    assert!(has(&out, "username"), "{out}");
    assert!(!text_of(&out).contains("did not confirm"), "{out}");
}

#[test]
fn the_session_module_keeps_what_the_coordinator_said_and_nothing_more() {
    // Catches: a deadline kept for a session that owes no change, a change noted for nobody, a
    // CSRF token in what the page may read about the session, and a stale sign-in that forgets a
    // newer one.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import * as session from "./session.js";
export async function main() {
  const log = [];
  log.push(session.requireChange(), session.who());
  session.signIn({ username: "a", csrf: "c1", must_change: false, must_change_until: 99 });
  log.push(session.who());
  session.signIn({ username: "a", csrf: "c1", must_change: true, must_change_until: 99 });
  log.push(session.who());
  session.signIn({ username: "b", csrf: "c2", must_change: false });
  session.forget("c1");
  log.push(session.isSignedIn(), session.getCsrf());
  session.signOut();
  log.push(session.getCsrf(), session.requireChange());
  return log;
}"#,
    );
    assert_eq!(
        out,
        json!([
            false,
            null,
            {"username": "a", "mustChange": false, "mustChangeUntil": 0},
            {"username": "a", "mustChange": true, "mustChangeUntil": 99.0},
            true,
            "c2",
            null,
            false
        ])
    );
}
