//! The Desktop page of a Mac (`ui/pages/desktop.js`), its link in the lab's list, the viewer's
//! adapter over noVNC (`ui/screen.js`) and the words for a console stream's close
//! (`closeWords` of `ui/pages/serialconsole.js`), run in the JavaScript engine against the fake gateway, a fake socket
//! and a fake viewer. Nothing here waits on a clock: the test opens, closes and speaks for the
//! socket and the viewer itself.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m};

const CSRF: &str = "csrf_5b2e91c7d4a06f38";
const NODE: &str = "mac-1";
/// A session id: 64 lower-case hex digits.
const SESSION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The reply of `Console/Open` for a Mac.
fn opened() -> Value {
    json!({"kind": "CONSOLE_KIND_SCREEN", "session": SESSION, "expiresInSecs": "30"})
}

/// A gateway that answers `Console/List` with no session and `Console/Open` with `open`.
fn ui_with(open: Value) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Console/List"), json!({}));
    ui.reply(&m("Console/Open"), open);
    ui
}

/// A driver that renders the Desktop page of `NODE` signed in with `CSRF`, mounts it in a fake
/// document, answers the page's questions with `answers`, then runs `then` (JavaScript with
/// `ctx`, `seen`, `root`, `status()`, `typeIn(name, value)`, `press(name)`, `has(name)`,
/// `screen` and the kit's helpers in scope) and returns what it returns.
fn driver(answers: &str, then: &str) -> String {
    format!(
        r#"
import {{ api, recorder, mounted, domNamed, domTexts, elements, fire, settle }} from "../test/kit.js";
import {{ render }} from "./pages/desktop.js";
export async function main() {{
  const {{ ctx, seen }} = recorder(api({csrf}), {{ nodeId: {node} }}, {answers});
  const tree = await render(ctx);
  const root = mounted(tree).root;
  const status = () => domTexts(domNamed(root, "desktop-status")).join("");
  const typeIn = (name, value) => fire(domNamed(root, name), "input", {{ target: {{ value }} }});
  const press = async (name) => {{ fire(domNamed(root, name), "click", {{}}); await settle(); }};
  const has = (name) => elements(root).some((e) => e.attributes["name"] === name);
  const screen = domNamed(root, "desktop-screen");
  const out = await (async () => {{ {then} }})();
  return out === undefined ? null : out;
}}"#,
        csrf = json!(CSRF),
        node = json!(NODE),
    )
}

fn run(ui: &mut Ui, then: &str) -> Value {
    ui.run(&driver("[]", then))
}

/// Opens the desktop with the reason "look at the login window", and opens the stream's socket.
const OPEN: &str = r#"
typeIn("desktop-reason", "  look at the login window  ");
await press("open-desktop");
const socket = seen.sockets[0];
socket.handlers.open();
await settle();
"#;

/// The source of the console's close causes (`hostkeeper_console::close::Close`), read as text:
/// this crate does not depend on the console's.
const CLOSE_RS: &str = include_str!("../../hostkeeper-console/src/close.rs");

/// The reason tokens `Close::token` returns, in its order: each `=> "<token>",` arm of that
/// function.
fn close_tokens() -> Vec<String> {
    let body = CLOSE_RS
        .split_once("pub const fn token(self)")
        .expect("close.rs has Close::token")
        .1;
    let body = body.split_once("\n    }\n").expect("Close::token ends").0;
    body.lines()
        .filter_map(|line| line.trim().split_once("=> \""))
        .filter_map(|(_, rest)| rest.strip_suffix("\","))
        .map(str::to_owned)
        .collect()
}

/// The methods the page sent, in order.
fn methods(ui: &Ui) -> Vec<String> {
    ui.methods()
}

/// Proves: the lab's list has a Desktop link on the row of a host whose last heartbeat said it is
/// a Mac (`PLATFORM_DARWIN_ARM64`), and on no other row (a Linux host, a host that never
/// heartbeated); the link opens the Desktop page of that host.
///
/// Catches: a Desktop link on a Linux server (whose console is its serial port), a link that opens
/// another host's page, and a list that lost the column.
#[test]
fn the_desktop_link_is_on_a_macs_row_only_and_opens_its_page() {
    let mut ui = Ui::start();
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [
            {"nodeId": "mac-1", "live": true, "lastHeartbeatUnix": "1800000000",
             "heartbeat": {"platform": "PLATFORM_DARWIN_ARM64"}},
            {"nodeId": "web-1", "live": true, "lastHeartbeatUnix": "1800000000",
             "heartbeat": {"platform": "PLATFORM_LINUX_X86_64"}},
            {"nodeId": "new-1"},
        ]}),
    );
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    let out = ui.run(
        r#"
import { api, recorder, nodes, rows, click } from "../test/kit.js";
import { render } from "./pages/home.js";
export async function main() {
  const { ctx, seen } = recorder(api("operator-token"), { list: true }, []);
  const tree = await render(ctx);
  const links = nodes(tree).filter((n) => n.attrs["name"] === "open-desktop").length;
  await click(tree, "open-desktop", 0);
  return { rows: rows(tree), links, navigations: seen.navigations };
}"#,
    );
    assert_eq!(
        out["rows"],
        json!([
            [
                "Server", "State", "Address", "Where", "Group", "Version", "Console"
            ],
            ["mac-1", "healthy", "-", "Unplaced", "-", "-", "Desktop"],
            ["new-1", "pending", "-", "Unplaced", "-", "-", "-"],
            ["web-1", "healthy", "-", "Unplaced", "-", "-", "Console"],
        ])
    );
    assert_eq!(out["links"], json!(1), "one Desktop link");
    assert_eq!(
        out["navigations"],
        json!([{"name": "desktop", "params": {"nodeId": "mac-1"}}])
    );
}

/// Proves: the page opens a desktop only with a reason the API takes: with none (or only spaces),
/// with one over 256 bytes, or with a control character, it says why and sends no
/// `Console/Open` and opens no socket; the reason it sends is trimmed.
///
/// Catches: an open without a reason (which the coordinator refuses only after a round trip), and
/// a reason sent untrimmed.
#[test]
fn opening_needs_a_reason_and_sends_nothing_without_one() {
    let long = "x".repeat(257);
    let mut ui = ui_with(opened());
    let out = run(
        &mut ui,
        &format!(
            r#"
const said = [];
for (const reason of ["", "   ", {long}, "a\tb"]) {{
  typeIn("desktop-reason", reason);
  await press("open-desktop");
  said.push(seen.reports[seen.reports.length - 1]);
}}
return {{ said, sockets: seen.sockets.length }};"#,
            long = json!(long)
        ),
    );
    assert_eq!(
        out["said"],
        json!([
            "Give a reason first.",
            "Give a reason first.",
            "A reason is at most 256 bytes; this one is 257.",
            "A reason has no control character (a tab or a line break, for example).",
        ])
    );
    assert_eq!(out["sockets"], json!(0));
    assert_eq!(methods(&ui), [m("Console/List")]);
}

/// Proves: opening sends `Console/Open` with the host and the trimmed reason, then opens one
/// socket at `/console/v1/stream/<session>` with the sign-in's CSRF token for its subprotocol
/// (the opener puts it there: `socket_address.rs`), and nothing else: the token is in no path and
/// no message, and the page sends no message of its own over the socket.
///
/// Catches: a token put in the address, a socket opened without it (which the coordinator refuses
/// 403 `csrf`), and a page that writes into the Mac's stream.
#[test]
fn the_csrf_goes_only_in_the_subprotocol() {
    let mut ui = ui_with(opened());
    let out = run(
        &mut ui,
        &format!(
            "{OPEN}\nreturn {{ path: socket.path, protocols: socket.protocols, sent: socket.sent, sockets: seen.sockets.length }};"
        ),
    );
    assert_eq!(
        out,
        json!({
            "path": format!("/console/v1/stream/{SESSION}"),
            "protocols": ["hostkeeper.console.v1", format!("csrf.{CSRF}")],
            "sent": [],
            "sockets": 1,
        })
    );
    assert!(!out["path"].as_str().unwrap_or_default().contains(CSRF));
    assert_eq!(
        bodies(&ui),
        [
            (m("Console/List"), json!({})),
            (
                m("Console/Open"),
                json!({"hostId": NODE, "reason": "look at the login window"})
            ),
        ]
    );
}

/// Proves: the viewer is built only in the socket's `open` handler (before it opens there is no
/// viewer: the Mac speaks first, and the viewer must be listening for its first byte), once, on the
/// page's screen element and with the very socket object the opener handed over (no address); a
/// second open of the same socket builds nothing more, and while a desktop is open the page offers
/// no second Open, so one page opens exactly one socket.
///
/// Catches: a viewer given an address (which would open a second socket of its own), a viewer
/// built before the socket is open, and a second socket for one page.
#[test]
fn the_page_opens_exactly_one_socket_and_gives_it_to_the_viewer() {
    let mut ui = ui_with(opened());
    let out = run(
        &mut ui,
        r#"
typeIn("desktop-reason", "look");
await press("open-desktop");
const before = seen.viewers.length;
const socket = seen.sockets[0];
socket.handlers.open();
await settle();
const viewer = seen.viewers[0];
const once = { sameSocket: viewer.socket === socket, onScreen: viewer.element === screen };
const offered = has("open-desktop");
socket.handlers.open();
await settle();
return { before, once, offered, viewers: seen.viewers.length, sockets: seen.sockets.length,
         close: has("close-desktop"), status: status() };"#,
    );
    assert_eq!(
        out,
        json!({
            "before": 0,
            "once": {"sameSocket": true, "onScreen": true},
            "offered": false,
            "viewers": 1,
            "sockets": 1,
            "close": true,
            "status": "Connected to the coordinator; waiting for the Mac.",
        })
    );
}

/// Proves: the page shows the coordinator's reason for every way a stream closes: each token of
/// the coordinator's (`Close::token`, read from its source) and the protocol closes (`device_closed`, `protocol_error`,
/// `message_too_big`) has its own sentence; a token the page does not know is shown with its code;
/// a close with no token says whether the stream had opened. A closed desktop can be opened again.
///
/// Catches: a close that leaves the page saying "connecting", a code shown as a bare number, a
/// token of the coordinator with no words, and a desktop that cannot be opened again after one
/// ended.
#[test]
fn a_close_code_shows_its_reason() {
    let mut tokens = close_tokens();
    tokens.extend(
        ["device_closed", "protocol_error", "message_too_big"]
            .iter()
            .map(|t| (*t).to_owned()),
    );
    let out = Ui::start().run(&format!(
        r#"
import {{ CLOSES, closeWords }} from "./pages/serialconsole.js";
const tokens = {tokens};
export async function main() {{
  return {{
    missing: tokens.filter((t) => !Object.prototype.hasOwnProperty.call(CLOSES, t)),
    extra: Object.keys(CLOSES).filter((t) => !tokens.includes(t)),
  }};
}}"#,
        tokens = json!(tokens)
    ));
    assert_eq!(out, json!({"missing": [], "extra": []}));
    // The 13 causes of `Close::token` (4001 to 4013) and the three protocol closes.
    assert_eq!(tokens.len(), 16, "{tokens:?}");

    let mut ui = ui_with(opened());
    ui.reply(&m("Console/Open"), opened());
    ui.reply(&m("Console/Open"), opened());
    ui.reply(&m("Console/Open"), opened());
    let out = run(
        &mut ui,
        r#"
const said = [];
const closeWith = async (code, reason, opens) => {
  typeIn("desktop-reason", "look");
  await press("open-desktop");
  const socket = seen.sockets[seen.sockets.length - 1];
  if (opens) {
    socket.handlers.open();
  }
  socket.handlers.close({ code, reason });
  await settle();
  said.push({ status: status(), reopen: has("open-desktop") });
};
await closeWith(4011, "mac_screen_sharing_off", true);
await closeWith(4999, "something_new", true);
await closeWith(1006, "", true);
await closeWith(1006, "", false);
return said;"#,
    );
    assert_eq!(
        out,
        json!([
            {"status": "The Mac refused the connection: Screen Sharing is off.", "reopen": true},
            {"status": "The console ended (4999 something_new).", "reopen": true},
            {"status": "The connection to the coordinator was lost.", "reopen": true},
            {"status": "The coordinator did not accept the connection. Your sign-in may have ended: sign in again and open the console again.", "reopen": true},
        ])
    );
}

/// Proves: when the Mac asks for its login, the page shows a form for exactly what it asks (a
/// password alone, or a user name and a password), hands what is typed to the viewer and to
/// nothing else (no call, no socket message), and keeps no copy of the password once sent; a
/// refused login and the shown screen are said in words.
///
/// Catches: a password left in the page after it was sent, a password sent to the coordinator, a
/// user name field offered to a Mac that asks only for a password, and a refusal with no words.
#[test]
fn the_macs_login_goes_to_the_viewer_and_is_not_kept() {
    let mut ui = ui_with(opened());
    let out = run(
        &mut ui,
        &format!(
            r#"{OPEN}
const viewer = seen.viewers[0];
viewer.events.credentials(["password"]);
await settle();
const passwordOnly = {{ user: has("mac-username"), password: has("mac-password") }};
viewer.events.credentials(["username", "password"]);
await settle();
const asked = status();
typeIn("mac-username", "operator");
typeIn("mac-password", "hunter2-secret");
await press("mac-sign-in");
const kept = elements(root).filter((e) => e.attributes["name"] === "mac-password")
  .map((e) => e.value === undefined ? "" : e.value);
const afterSend = {{ status: status(), form: has("mac-password") }};
viewer.events.securityFailure("Authentication failed");
await settle();
const refused = status();
viewer.events.securityFailure("");
await settle();
const refusedBare = status();
viewer.events.connect();
await settle();
return {{ passwordOnly, asked, credentials: viewer.credentials, kept, afterSend, refused, refusedBare,
         shown: status(), sent: seen.sockets[0].sent }};"#
        ),
    );
    assert_eq!(
        out,
        json!({
            "passwordOnly": {"user": false, "password": true},
            "asked": "The Mac asks for its own login: type it below. It goes to the Mac through the viewer and is not kept.",
            "credentials": [{"username": "operator", "password": "hunter2-secret"}],
            "kept": [],
            "afterSend": {"status": "Login sent to the Mac.", "form": false},
            "refused": "The Mac refused the login: Authentication failed",
            "refusedBare": "The Mac refused the login.",
            "shown": "The screen of mac-1 is shown.",
            "sent": [],
        })
    );
    assert_eq!(methods(&ui), [m("Console/List"), m("Console/Open")]);
}

/// Proves: a refused `Console/Open` (another operator holds the Mac's console) is shown with its
/// refusal and opens no socket; the page can open again after.
///
/// Catches: a refusal swallowed, leaving the page "asking the coordinator" for ever, and a socket
/// opened without a session.
#[test]
fn a_refused_open_says_why_and_opens_no_socket() {
    let mut ui = Ui::start();
    ui.reply(&m("Console/List"), json!({}));
    ui.refuse(&m("Console/Open"), 409, "ALREADY_EXISTS", "console_busy");
    let out = run(
        &mut ui,
        r#"
typeIn("desktop-reason", "look");
await press("open-desktop");
return { status: status(), sockets: seen.sockets.length, reopen: has("open-desktop") };"#,
    );
    assert_eq!(
        out,
        json!({
            "status": "The desktop did not open: ALREADY_EXISTS console_busy.",
            "sockets": 0,
            "reopen": true,
        })
    );
}

/// Proves: when `Console/Open` answers a serial console (the host is not a Mac), or a session id of
/// no fixed shape, the page opens no socket, says so, and gives the session back with
/// `Console/Close` for the host.
///
/// Catches: a Mac viewer pointed at a serial port's bytes, and a session left holding the host's
/// one console slot until its window passes.
#[test]
fn a_host_that_is_not_a_mac_gives_the_session_back() {
    for (reply, said) in [
        (
            json!({"kind": "CONSOLE_KIND_SERIAL", "session": SESSION, "expiresInSecs": "30"}),
            "mac-1 is not a Mac: its console is not a screen. The session was given back.",
        ),
        (
            json!({"kind": "CONSOLE_KIND_SCREEN", "session": "NOT-A-SESSION", "expiresInSecs": "30"}),
            "The coordinator's answer named no session this page can open. The session was given back.",
        ),
    ] {
        let mut ui = ui_with(reply);
        ui.reply(&m("Console/Close"), json!({}));
        let out = ui.run(&driver(
            "[]",
            r#"
const real = ctx.openSocket;
ctx.openSocket = (path, handlers, protocols) => {
  if (!/^\/console\/v1\/stream\/[0-9a-f]{64}$/.test(path)) {
    throw new Error(`not a socket path: ${path}`);
  }
  return real(path, handlers, protocols);
};
typeIn("desktop-reason", "look");
await press("open-desktop");
return { status: status(), sockets: seen.sockets.length, viewers: seen.viewers.length };"#,
        ));
        assert_eq!(
            out,
            json!({"status": said, "sockets": 0, "viewers": 0}),
            "{said}"
        );
        assert_eq!(
            bodies(&ui),
            [
                (m("Console/List"), json!({})),
                (m("Console/Open"), json!({"hostId": NODE, "reason": "look"})),
                (m("Console/Close"), json!({"hostId": NODE})),
            ]
        );
    }
}

/// Proves: the page names who holds the Mac's console (from `Console/List`: the operator, since
/// when, attached or not, and the reason), with a Close that asks first and sends `Console/Close`
/// for the host only on a yes; a host nobody holds says so; a list that cannot be read is said and
/// the desktop can still be opened.
///
/// Catches: a holder's session closed without a question, a Close that names another host, and a
/// failed read that hides the Open button.
#[test]
fn the_holder_of_the_macs_console_is_named_with_a_close_that_asks_first() {
    let holder = json!({"sessions": [
        {"hostId": "web-1", "kind": "CONSOLE_KIND_SERIAL", "openedBy": "bob", "openedUnix": "1800000000"},
        {"hostId": NODE, "kind": "CONSOLE_KIND_SCREEN", "openedBy": "alice",
         "openedUnix": "1800000000", "attached": true, "reason": "fix the login window"},
    ]});
    for answers in ["[false]", "[true]"] {
        let mut ui = Ui::start();
        ui.reply(&m("Console/List"), holder.clone());
        ui.reply(&m("Console/Close"), json!({}));
        let out = ui.run(&driver(
            answers,
            r#"
const shown = domTexts(domNamed(root, "desktop-holder")).join("");
fire(domNamed(root, "close-holder"), "click", {});
await settle();
return { shown, questions: seen.questions, reports: seen.reports, refreshes: seen.refreshes };"#,
        ));
        let yes = answers == "[true]";
        assert_eq!(
            out,
            json!({
                "shown": "alice holds the console of mac-1 since 2027-01-15 08:00:00 UTC (attached): fix the login window.",
                "questions": ["Close the console of mac-1, which alice holds?"],
                "reports": [if yes { "The console of mac-1 was closed." } else { "Not closed." }],
                "refreshes": i32::from(yes),
            }),
            "{answers}"
        );
        let mut want = vec![(m("Console/List"), json!({}))];
        if yes {
            want.push((m("Console/Close"), json!({"hostId": NODE})));
        }
        assert_eq!(bodies(&ui), want, "{answers}");
    }

    let mut ui = Ui::start();
    ui.reply(&m("Console/List"), json!({}));
    let none = run(
        &mut ui,
        r#"return domTexts(domNamed(root, "desktop-holder")).join("");"#,
    );
    assert_eq!(none, json!("No console of mac-1 is open."));

    let mut ui = Ui::start();
    ui.refuse(
        &m("Console/List"),
        403,
        "PERMISSION_DENIED",
        "no_console_access",
    );
    let failed = run(
        &mut ui,
        r#"return { holder: domTexts(domNamed(root, "desktop-holder")).join(""), open: has("open-desktop") };"#,
    );
    assert_eq!(
        failed,
        json!({
            "holder": "Who holds a console could not be read: PERMISSION_DENIED no_console_access.",
            "open": true,
        })
    );
}

/// Proves: the page's own Close ends the desktop: the viewer is disconnected and the socket
/// closed, once each, and the page says so and offers Open again; a page with no opener or no
/// viewer, or no page named, cannot open one and says so.
///
/// Catches: a Close that leaves the socket (and the Mac's console slot) open, and a page that
/// throws when the app gave it no opener.
#[test]
fn closing_the_desktop_ends_the_viewer_and_the_socket() {
    let mut ui = ui_with(opened());
    let out = run(
        &mut ui,
        &format!(
            r#"{OPEN}
await press("close-desktop");
return {{ disconnects: seen.viewers[0].disconnects, closes: socket.closes, status: status(),
         open: has("open-desktop") }};"#
        ),
    );
    assert_eq!(
        out,
        json!({"disconnects": 1, "closes": 1, "status": "You closed the desktop.", "open": true})
    );

    let mut ui = ui_with(opened());
    let out = run(
        &mut ui,
        r#"
delete ctx.openSocket;
typeIn("desktop-reason", "look");
await press("open-desktop");
return seen.reports;"#,
    );
    assert_eq!(out, json!(["This page cannot open a desktop."]));

    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { api, recorder, text } from "../test/kit.js";
import { render } from "./pages/desktop.js";
export async function main() {
  const { ctx } = recorder(api("operator-token"), {}, []);
  return text(await render(ctx));
}"#,
    );
    assert_eq!(out, json!("Desktop No host was named."));
    assert!(ui.methods().is_empty());
}

/// Proves: through the app, an open desktop is closed (its viewer disconnected and its socket
/// closed) when the operator goes to another page, and when the operator signs out; one the
/// operator stays with is not; and the app hands the page the CSRF token of the sign-in for the
/// socket's subprotocol.
///
/// Catches: a Mac's screen left streaming, unseen, after its page is gone or the operator signed
/// out, holding the Mac's one console until the coordinator's idle limit.
#[test]
fn signing_out_closes_every_open_console() {
    let ui = Ui::start();
    ui.session(CSRF);
    for _ in 0..4 {
        ui.reply(&m("Hosts/List"), json!({"hosts": []}));
        ui.reply(&m("Releases/Status"), json!({}));
        ui.reply(&m("Alerts/List"), json!({}));
        ui.reply(&m("Layout/Get"), json!({}));
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
        ui.reply(&m("Console/List"), json!({}));
        ui.reply(&m("Console/Open"), opened());
    }
    let mut ui = ui;
    let out = ui.run(
        r#"
import { fakeDocument, settle, domNamed, fire } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  const sockets = [];
  const viewers = [];
  const app = startApp({
    doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000,
    openSocket: (path, handlers, protocols) => {
      const socket = { path, handlers, protocols, closes: 0 };
      sockets.push(socket);
      return { send: () => {}, close: () => { socket.closes += 1; }, socket: {} };
    },
    viewScreen: () => {
      const viewer = { disconnects: 0 };
      viewers.push(viewer);
      return { sendCredentials: () => {}, disconnect: () => { viewer.disconnects += 1; }, focus: () => {} };
    },
  });
  await settle();
  const open = async () => {
    app.navigate("desktop", { nodeId: "mac-1" });
    await settle();
    fire(domNamed(doc.root, "desktop-reason"), "input", { target: { value: "look" } });
    fire(domNamed(doc.root, "open-desktop"), "click", {});
    await settle();
    sockets[sockets.length - 1].handlers.open();
    await settle();
  };
  await open();
  const stayed = { closes: sockets[0].closes, disconnects: viewers[0].disconnects };
  app.navigate("home", {});
  await settle();
  const leftPage = { closes: sockets[0].closes, disconnects: viewers[0].disconnects };
  await open();
  const beforeSignOut = { closes: sockets[1].closes, disconnects: viewers[1].disconnects };
  fire(domNamed(doc.root, "sign-out"), "click", {});
  await settle();
  const signedOut = { closes: sockets[1].closes, disconnects: viewers[1].disconnects };
  return { stayed, leftPage, beforeSignOut, signedOut, protocols: sockets.map((s) => s.protocols[1]), opened: sockets.length };
}"#,
    );
    assert_eq!(
        out,
        json!({
            "stayed": {"closes": 0, "disconnects": 0},
            "leftPage": {"closes": 1, "disconnects": 1},
            "beforeSignOut": {"closes": 0, "disconnects": 0},
            "signedOut": {"closes": 1, "disconnects": 1},
            "protocols": [format!("csrf.{CSRF}"), format!("csrf.{CSRF}")],
            "opened": 2,
        })
    );
}

/// Proves: the viewer's adapter builds the library's `RFB` on the element and the open socket it
/// is given (no address), scales the Mac's screen to the page and leaves the Mac's resolution
/// alone, tells the page each thing the library says (connect, the login it asks, a refused login
/// with or without words, a disconnect), and passes the login, a disconnect and a focus to it.
///
/// Catches: an adapter that hands the library an address, a login asked for and never shown, and
/// a library event lost.
#[test]
fn the_viewer_is_built_on_the_open_socket_and_says_what_the_library_says() {
    let out = Ui::start().run(
        r#"
import { screenViewer } from "./screen.js";
export async function main() {
  const built = [];
  class FakeRFB {
    constructor(target, socket) {
      this.element = target;
      this.socket = socket;
      this.listeners = {};
      this.calls = [];
      built.push(this);
    }
    addEventListener(type, run) { this.listeners[type] = run; }
    sendCredentials(creds) { this.calls.push(["credentials", creds]); }
    disconnect() { this.calls.push(["disconnect"]); }
    focus() { this.calls.push(["focus"]); }
  }
  const told = [];
  const target = { element: true };
  const socket = { open: true };
  const viewer = screenViewer(FakeRFB)(target, { socket, send() {}, close() {} }, {
    connect: () => told.push(["connect"]),
    credentials: (types) => told.push(["credentials", types]),
    securityFailure: (reason) => told.push(["securityFailure", reason]),
    disconnect: (clean) => told.push(["disconnect", clean]),
  });
  const rfb = built[0];
  rfb.listeners.connect({});
  rfb.listeners.credentialsrequired({ detail: { types: ["username", "password", "target"] } });
  rfb.listeners.credentialsrequired({});
  rfb.listeners.securityfailure({ detail: { status: 1, reason: "Authentication failed" } });
  rfb.listeners.securityfailure({ detail: { status: 1 } });
  rfb.listeners.disconnect({ detail: { clean: true } });
  rfb.listeners.disconnect({});
  viewer.sendCredentials({ username: "u", password: "p", extra: "x" });
  viewer.focus();
  viewer.disconnect();
  return {
    built: built.length, target: rfb.element === target, socket: rfb.socket === socket,
    scale: rfb.scaleViewport, resize: rfb.resizeSession, told, calls: rfb.calls,
  };
}"#,
    );
    assert_eq!(
        out,
        json!({
            "built": 1, "target": true, "socket": true, "scale": true, "resize": false,
            "told": [
                ["connect"],
                ["credentials", ["username", "password"]],
                ["credentials", ["password"]],
                ["securityFailure", "Authentication failed"],
                ["securityFailure", ""],
                ["disconnect", true],
                ["disconnect", false],
            ],
            "calls": [
                ["credentials", {"username": "u", "password": "p"}],
                ["focus"],
                ["disconnect"],
            ],
        })
    );
}

/// What a browser gives noVNC's `core/websock.js` when it loads: a `window` whose `console` the
/// library's logging binds, and `WebSocket`'s four ready states. Imported ahead of the library.
const BROWSER_STUB: &str = r#"
const quiet = { debug() {}, info() {}, warn() {}, error() {} };
globalThis.window = { console: quiet, Error };
globalThis.console = quiet;
globalThis.WebSocket = { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 };
"#;

/// Proves: the vendored noVNC socket layer (`core/websock.js`, with the one change this repository
/// made to it) refuses to open a socket from an address (`open` throws, naming the open socket it
/// wants instead), and attaches to a socket already open: it reads the open socket's state, takes
/// its binary messages into its queue byte for byte and hands its close (with the close frame's
/// code) to its owner. This is the path `RFB` takes when the Desktop page gives it the open socket.
///
/// Catches: a re-vendored library whose `open` makes a socket again (the scan refuses only the
/// call's text), and an attach that no longer takes a socket handed over open.
#[test]
fn the_vendored_socket_layer_refuses_an_address_and_attaches_an_open_socket() {
    let mut ui = Ui::start_with(
        &[
            "vendor/novnc/core/websock.js",
            "vendor/novnc/core/util/logging.js",
        ],
        &[("browser_stub.js", BROWSER_STUB)],
    );
    let out = ui.run(
        r#"
import "../test/browser_stub.js";
import Websock from "./vendor/novnc/core/websock.js";
export async function main() {
  const ws = new Websock();
  let refused = null;
  try {
    ws.open("wss://coordinator.example/console/v1/stream/x", ["hostkeeper.console.v1"]);
  } catch (e) {
    refused = e.message;
  }
  const said = [];
  ws.on("open", () => said.push("open"));
  ws.on("message", () => said.push(`message ${ws.rQlen()}`));
  ws.on("close", (e) => said.push(`close ${e.code}`));
  const channel = {
    send() {}, close() {}, binaryType: "blob", onerror: null, onmessage: null, onopen: null,
    protocol: "hostkeeper.console.v1", readyState: 1,
  };
  ws.attach(channel);
  const state = ws.readyState;
  channel.onmessage({ data: new Uint8Array([82, 70, 66, 32]).buffer });
  const greeting = ws.rQshiftStr(4);
  channel.onclose({ code: 4011, reason: "mac_screen_sharing_off" });
  return { refused, state, binaryType: channel.binaryType, greeting, said };
}"#,
    );
    assert_eq!(
        out,
        json!({
            // `Websock` and `open` joined by a dot, which the hygiene lint would read as a host.
            "refused": format!("{}.open is not available: hand RFB an open socket", "Websock"),
            "state": "open",
            "binaryType": "arraybuffer",
            "greeting": "RFB ",
            "said": ["message 4", "close 4011"],
        })
    );
}

/// Proves: when the viewer ends the stream itself after the Mac refused the login (its close
/// carries no word of the coordinator's), the page keeps saying the Mac refused it; a close the
/// coordinator gives a reason still says that reason; and each end takes the viewer's elements
/// out of the screen box, whichever side ended it, so a new desktop does not draw beside a stale
/// one.
///
/// Catches: the Mac's refusal replaced by "closed without saying why", and the last frame left on
/// the page after the desktop ended.
#[test]
fn a_refused_login_stays_said_and_an_ended_desktop_leaves_no_screen() {
    let mut ui = ui_with(opened());
    ui.reply(&m("Console/Open"), opened());
    ui.reply(&m("Console/Open"), opened());
    let out = run(
        &mut ui,
        r#"
const draw = () => screen.appendChild({ nodeType: 1, attributes: {}, children: [], listeners: {} });
const once = async (close) => {
  typeIn("desktop-reason", "look");
  await press("open-desktop");
  const socket = seen.sockets[seen.sockets.length - 1];
  socket.handlers.open();
  await settle();
  draw();
  const viewer = seen.viewers[seen.viewers.length - 1];
  viewer.events.securityFailure("Authentication failed");
  await settle();
  const drawn = screen.children.length;
  await close(socket);
  return { status: status(), drawn, left: screen.children.length };
};
const byViewer = await once(async (socket) => { socket.handlers.close({ code: 1006, reason: "" }); await settle(); });
const byCoordinator = await once(async (socket) => { socket.handlers.close({ code: 4005, reason: "idle" }); await settle(); });
const byOperator = await once(async () => { await press("close-desktop"); });
return { byViewer, byCoordinator, byOperator };"#,
    );
    assert_eq!(
        out,
        json!({
            "byViewer": {"status": "The Mac refused the login: Authentication failed", "drawn": 1, "left": 0},
            "byCoordinator": {"status": "Nothing crossed the console for too long, so it was closed.", "drawn": 1, "left": 0},
            "byOperator": {"status": "You closed the desktop.", "drawn": 1, "left": 0},
        })
    );
}

/// Proves: an Open the operator closed before the coordinator answered blocks a second Open until
/// it is answered; its session is then given back with `Console.Close` for the host and no socket is
/// opened for it; after that a new Open goes through.
///
/// Catches: a late answer to an abandoned Open whose give-back (by host) ends the session a second
/// Open took meanwhile, and a socket opened for a session the operator closed.
#[test]
fn an_open_closed_before_its_answer_is_given_back_before_another_open() {
    let mut ui = Ui::start();
    ui.reply(&m("Console/List"), json!({}));
    ui.hold(&m("Console/Open"), 200, &opened().to_string());
    ui.reply(&m("Console/Close"), json!({}));
    ui.reply(&m("Console/Open"), opened());
    let out = run(
        &mut ui,
        r#"
typeIn("desktop-reason", "look");
await press("open-desktop");
await press("close-desktop");
typeIn("desktop-reason", "look");
await press("open-desktop");
const blocked = seen.reports.slice();
gatewayRelease("hostkeeper.operator.v1.Console/Open");
await settle();
const afterAnswer = seen.sockets.length;
typeIn("desktop-reason", "look");
await press("open-desktop");
return { blocked, afterAnswer, sockets: seen.sockets.length };"#,
    );
    assert_eq!(
        out,
        json!({
            "blocked": ["The coordinator has not answered the last Open yet."],
            "afterAnswer": 0,
            "sockets": 1,
        })
    );
    assert_eq!(
        methods(&ui),
        [
            m("Console/List"),
            m("Console/Open"),
            m("Console/Close"),
            m("Console/Open")
        ]
    );
}
