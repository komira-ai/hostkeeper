//! The Console page (`ui/pages/serialconsole.js`), run in the JavaScript engine against the fake
//! gateway, a fake socket and a fake terminal: the open sessions, Console.Open with its reason,
//! the one socket it opens and what it offers, the terminal it hands that socket to, the "no
//! output yet" note, and the words for every way a console closes. Nothing here waits on a clock:
//! the test delivers each socket event and each tick of the page's timer itself.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

/// The CSRF token of the sign-in the kit's `api(csrf)` makes.
const CSRF: &str = "operator-token";
/// A session id as Console.Open hands it out: 64 lower-case hex digits.
const SESSION: &str = "5b2e91c7d4a06f385b2e91c7d4a06f385b2e91c7d4a06f385b2e91c7d4a06f38";

/// A gateway with `lists` answers to Console.List, each listing no session.
fn ui_with(lists: usize) -> Ui {
    let ui = Ui::start();
    for _ in 0..lists {
        ui.reply(&m("Console/List"), json!({}));
    }
    ui
}

/// The answer of Console.Open for a serial session that asks for `quiet` seconds (none: the
/// field is left out, as an older coordinator leaves it).
fn opened(quiet: Option<&str>) -> Value {
    let mut reply =
        json!({"kind": "CONSOLE_KIND_SERIAL", "session": SESSION, "expiresInSecs": "30"});
    if let Some(quiet) = quiet {
        reply["quietSecs"] = json!(quiet);
    }
    reply
}

/// A driver that renders the Console page of `node-02` signed in with `session` (the CSRF token,
/// or null), mounts it in a fake document (`doc`), answers the page's questions with `answers`,
/// then runs `then` with `tree`, `ctx`, `seen`, `leave`, `doc`, `status()`, `quiet()`,
/// `open(reason)` (types the reason, clicks Open the console and settles; returns the socket's
/// record, or null) and the kit's helpers in scope, and returns what it returns.
fn driver(session: &str, answers: &str, then: &str) -> String {
    format!(
        r#"
import {{ api, recorder, text, named, nodes, type, click, settle, mounted, domNamed, domTexts, elements, fire }} from "../test/kit.js";
import * as session from "./session.js";
import {{ render, CLOSES, PROTOCOL, QUIET_NOTE }} from "./pages/serialconsole.js";
export async function main() {{
  const {{ ctx, seen, leave }} = recorder(api({session}), {{ nodeId: "node-02" }}, {answers});
  const tree = await render(ctx);
  const doc = mounted(tree).root;
  const has = (name) => elements(doc).some((e) => e.attributes["name"] === name);
  const status = () => (has("console-status") ? domTexts(domNamed(doc, "console-status")).join("") : null);
  const quiet = () => (has("console-quiet") ? domTexts(domNamed(doc, "console-quiet")).join("") : null);
  const open = async (reason) => {{
    type(tree, "console-reason", reason);
    fire(domNamed(doc, "open-console"), "click", {{}});
    await settle();
    return seen.sockets.length === 0 ? null : seen.sockets[seen.sockets.length - 1];
  }};
  const out = await (async () => {{ {then} }})();
  return {{ seen, out: out === undefined ? null : out }};
}}"#
    )
}

fn run(ui: &mut Ui, then: &str) -> Value {
    ui.run(&driver(&format!("\"{CSRF}\""), "[]", then))
}

/// Proves: drawing the page reads the open sessions and nothing else (no Console.Open, no
/// socket, no terminal), and lists each session with its holder, its start and whether it is
/// attached; with none open it says so.
///
/// Catches: a console opened, or a BMC contacted, by merely visiting the page.
#[test]
fn the_page_lists_the_open_sessions_and_opens_nothing_until_asked() {
    let mut ui = Ui::start();
    ui.reply(
        &m("Console/List"),
        json!({"sessions": [
            {"hostId": "node-04", "kind": "CONSOLE_KIND_SERIAL", "openedBy": "alice",
             "openedUnix": "1800000000", "attached": true, "reason": "watching a boot"}]}),
    );
    let out = run(
        &mut ui,
        r#"return { rows: rows(), sockets: seen.sockets.length, terminals: seen.terminals.length, status: status() };
function rows() { return elements(domNamed(doc, "sessions")).filter((e) => e.tagName === "TR").map((r) => domTexts(r)); }"#,
    );
    assert_eq!(ui.methods(), [m("Console/List")]);
    let out = &out["out"];
    assert_eq!(
        out["rows"],
        json!([
            ["Host", "Held by", "Since", "Attached", ""],
            [
                "node-04",
                "alice",
                "2027-01-15 08:00:00 UTC",
                "yes",
                "Close"
            ]
        ])
    );
    assert_eq!(out["sockets"], json!(0));
    assert_eq!(out["terminals"], json!(0));
    assert_eq!(out["status"], json!(null));

    let mut ui = ui_with(1);
    let out = run(
        &mut ui,
        r#"return domTexts(domNamed(doc, "sessions")).join("|");"#,
    );
    assert_eq!(out["out"], json!("No console session is open."));
}

/// Proves: Open the console sends Console.Open with the host and the trimmed reason, then opens
/// exactly one socket, to the session's stream path, and nothing more while it is open; once the
/// socket opens the page hands that very socket to one terminal, put in the page's terminal box
/// and focused, sends one carriage return, and offers Close in place of Open.
///
/// Catches: a second socket for one console, a terminal given anything but the socket the page
/// opened (an address, another socket), and a login prompt that stays undrawn until the operator
/// guesses to press Enter.
#[test]
fn the_page_opens_exactly_one_socket_and_gives_it_to_the_viewer() {
    let mut ui = ui_with(3);
    ui.reply(&m("Console/Open"), opened(Some("10")));
    let out = run(
        &mut ui,
        r#"
const socket = await open("  watch the boot  ");
const before = { terminals: seen.terminals.length, sent: socket.sent.length, status: status() };
await click(tree, "open-console");
const again = seen.reports.slice();
socket.handlers.open();
await settle();
const terminal = seen.terminals[0];
return {
  before, again, path: socket.path, sockets: seen.sockets.length, terminals: seen.terminals.length,
  same: terminal.socket === socket, into: terminal.into, focuses: terminal.focuses, sent: socket.sent,
  status: status(), close: has("close-console"), openButton: has("open-console"),
};"#,
    );
    let bodies: Vec<Value> = ui
        .requests()
        .iter()
        .filter(|r| r.method() == m("Console/Open"))
        .map(|r| r.body())
        .collect();
    assert_eq!(
        bodies,
        [json!({"hostId": "node-02", "reason": "watch the boot"})]
    );
    let out = &out["out"];
    assert_eq!(
        out["before"],
        json!({"terminals": 0, "sent": 0, "status": "Connecting to the serial port."})
    );
    assert_eq!(out["again"], json!(["A console is already open."]));
    assert_eq!(out["path"], json!(format!("/console/v1/stream/{SESSION}")));
    assert_eq!(out["sockets"], json!(1));
    assert_eq!(out["terminals"], json!(1));
    assert_eq!(out["same"], json!(true));
    assert_eq!(out["into"], json!(["terminal"]));
    assert_eq!(out["focuses"], json!(1));
    assert_eq!(out["sent"], json!([{"text": "\r"}]));
    assert_eq!(out["status"], json!("Connected to the serial port."));
    assert_eq!(out["close"], json!(true));
    assert_eq!(out["openButton"], json!(false));
}

/// Proves: the socket offers exactly two subprotocols, the session's name first and then
/// `csrf.` with the sign-in's CSRF token, and the token is in neither the socket's path nor
/// anything sent over it.
///
/// Catches: the CSRF token put in the address (where history and logs keep it) or sent as
/// console input to the host, and an offer without the name the coordinator selects.
#[test]
fn the_csrf_token_goes_only_in_the_subprotocol() {
    let mut ui = ui_with(2);
    ui.reply(&m("Console/Open"), opened(Some("10")));
    let out = run(
        &mut ui,
        r#"
const socket = await open("boot");
socket.handlers.open();
socket.handlers.message(new Uint8Array([108, 111, 103, 105, 110, 58]));
return { protocols: socket.protocols, name: PROTOCOL, path: socket.path, sent: JSON.stringify(socket.sent) };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["protocols"],
        json!(["hostkeeper.console.v1", format!("csrf.{CSRF}")])
    );
    assert_eq!(out["name"], json!("hostkeeper.console.v1"));
    assert!(!out["path"].as_str().expect("a path").contains(CSRF));
    assert!(!out["sent"].as_str().expect("sent").contains(CSRF));
}

/// Proves: every reason token the coordinator closes a console with is said in the page's words,
/// with Open offered again; a reason the page has no words for is shown with its code; a close
/// with no reason says the coordinator refused the connection when the socket never opened, and
/// that the connection was lost (1006) or ended with its code when it had.
///
/// Catches: a console that ends with no word of why, and a refused upgrade (a sign-in that ended,
/// no console access) shown as a lost connection.
#[test]
fn a_close_code_shows_its_reason() {
    let closes = [
        (
            4001,
            "device_unreachable",
            "The BMC did not answer at the host's address, or its serial stream ended.",
        ),
        (
            4002,
            "bmc_sol_busy",
            "Someone else holds the BMC's Serial over LAN (a person in the lab or another tool), or took it from this session.",
        ),
        (
            4003,
            "bmc_busy",
            "The BMC has no free session. Try again in a minute.",
        ),
        (
            4004,
            "bmc_auth_failed",
            "The BMC refused the coordinator's BMC credential.",
        ),
        (
            4005,
            "idle",
            "Nothing crossed the console for too long, so it was closed.",
        ),
        (
            4006,
            "total_limit",
            "The console reached the longest time a session may last and was closed.",
        ),
        (
            4007,
            "closed_by_operator",
            "An operator closed this console session.",
        ),
        (
            4008,
            "token_revoked",
            "Your sign-in or your console access ended, so the console was closed.",
        ),
        (
            4009,
            "address_mismatch",
            "The BMC at the host's address reports another MAC than the host's record, so nothing was sent to it.",
        ),
        (
            4010,
            "coordinator_stopping",
            "The coordinator is stopping and closed the session.",
        ),
        (
            4011,
            "mac_screen_sharing_off",
            "The Mac refused the connection: Screen Sharing is off.",
        ),
        (
            4012,
            "mac_unreachable_or_asleep",
            "The Mac did not answer: it is asleep, off or not at its address.",
        ),
        (
            4013,
            "bmc_failed",
            "The coordinator could not run the BMC tool or read its answer.",
        ),
        (1000, "device_closed", "The device closed the connection."),
        (
            1002,
            "protocol_error",
            "The coordinator closed the console: it did not understand what the page sent.",
        ),
        (
            1009,
            "message_too_big",
            "The coordinator closed the console: the page sent a message over 64 KiB.",
        ),
        (
            4099,
            "something_new",
            "The console ended (4099 something_new).",
        ),
    ];
    let cases: Vec<Value> = closes
        .iter()
        .map(|(code, reason, _)| json!({"code": code, "reason": reason}))
        .collect();
    let mut ui = ui_with(2 * closes.len() + 6);
    for _ in 0..closes.len() + 3 {
        ui.reply(&m("Console/Open"), opened(Some("10")));
    }
    let out = run(
        &mut ui,
        &format!(
            r#"
const said = [];
for (const closed of {cases}) {{
  const socket = await open("boot");
  socket.handlers.open();
  socket.handlers.close(closed);
  await settle();
  said.push([status(), has("open-console")]);
}}
let socket = await open("boot");
socket.handlers.close({{ code: 1006, reason: "" }});
await settle();
const refused = status();
socket = await open("boot");
socket.handlers.open();
socket.handlers.close({{ code: 1006, reason: "" }});
await settle();
const lost = status();
socket = await open("boot");
socket.handlers.open();
socket.handlers.close({{ code: 1000, reason: "" }});
await settle();
const ended = status();
return {{ said, refused, lost, ended, words: Object.keys(CLOSES).length }};"#,
            cases = json!(cases)
        ),
    );
    let out = &out["out"];
    let said: Vec<Value> = closes
        .iter()
        .map(|(_, _, words)| json!([words, true]))
        .collect();
    assert_eq!(out["said"], json!(said));
    assert_eq!(
        out["words"],
        json!(closes.len() - 1),
        "a sentence for every token of the coordinator"
    );
    assert_eq!(
        out["refused"],
        json!(
            "The coordinator did not accept the connection. Your sign-in may have ended: sign in again and open the console again."
        )
    );
    assert_eq!(
        out["lost"],
        json!("The connection to the coordinator was lost.")
    );
    assert_eq!(out["ended"], json!("The console ended (1000)."));
}

/// Proves: once the socket is open the page asks to be woken after the session's quiet seconds
/// (the coordinator's, else 10); a tick with no byte yet shows the ttyS1 note and stops the
/// timer; the first byte takes the note away; and a byte before the tick stops the timer with no
/// note at all. Nothing is asked before the socket opens.
///
/// Catches: a blank terminal on a host whose console is on another port, with no hint of why, and
/// a note left up after the host has spoken.
#[test]
fn no_output_for_the_quiet_seconds_shows_the_ttys1_note_and_a_byte_clears_it() {
    let mut ui = ui_with(5);
    ui.reply(&m("Console/Open"), opened(Some("7")));
    ui.reply(&m("Console/Open"), opened(None));
    let out = run(
        &mut ui,
        r#"
let socket = await open("boot");
const beforeOpen = seen.repeats.length;
socket.handlers.open();
const timer = seen.repeats[0];
const beforeTick = quiet();
timer.run();
const silent = { note: quiet(), stops: timer.stops };
socket.handlers.message(new Uint8Array([13, 10]));
const spoke = { note: quiet(), stops: timer.stops };
fire(domNamed(doc, "close-console"), "click", {});
await settle();
socket = await open("boot");
socket.handlers.open();
const second = seen.repeats[1];
socket.handlers.message(new Uint8Array([108]));
const early = { stops: second.stops };
second.run();
return { beforeOpen, seconds: String(timer.seconds), beforeTick, silent, spoke, defaultSeconds: String(second.seconds), early,
  afterLateTick: quiet(), note: QUIET_NOTE };"#,
    );
    let out = &out["out"];
    assert_eq!(out["beforeOpen"], json!(0));
    assert_eq!(out["seconds"], json!("7"));
    assert_eq!(out["beforeTick"], json!(null));
    assert_eq!(
        out["silent"],
        json!({"note": "No output yet: the host's serial port must be ttyS1.", "stops": 1})
    );
    assert_eq!(out["spoke"], json!({"note": null, "stops": 1}));
    assert_eq!(out["defaultSeconds"], json!("10"));
    assert_eq!(out["early"], json!({"stops": 1}));
    assert_eq!(out["afterLateTick"], json!(null));
    assert_eq!(
        out["note"],
        json!("No output yet: the host's serial port must be ttyS1.")
    );
}

/// Proves: leaving the page (another page, a sign-out, the session ending: the app runs the
/// page's leave hook for each) closes the open socket once, stops the quiet timer and ends the
/// terminal; a page left with no console open closes nothing.
///
/// Catches: a console left attached, holding the host's one session and the BMC's Serial over LAN,
/// after its page is gone.
#[test]
fn leaving_the_page_closes_its_console() {
    let mut ui = ui_with(2);
    ui.reply(&m("Console/Open"), opened(Some("10")));
    let out = run(
        &mut ui,
        r#"
const socket = await open("boot");
socket.handlers.open();
leave();
const after = { closes: socket.closes, stops: seen.repeats[0].stops, disposes: seen.terminals[0].disposes, status: status() };
socket.handlers.close({ code: 1000, reason: "client_left" });
return { after, closes: socket.closes, status: status() };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["after"],
        json!({"closes": 1, "stops": 1, "disposes": 1, "status": "You left the page, so the session closed."})
    );
    assert_eq!(out["closes"], json!(1));
    assert_eq!(
        out["status"],
        json!("You left the page, so the session closed.")
    );

    let mut ui = ui_with(1);
    let out = run(
        &mut ui,
        r#"leave(); return { sockets: seen.sockets.length };"#,
    );
    assert_eq!(out["out"], json!({"sockets": 0}));
}

/// Proves: through the app, signing out closes the open console's socket and ends its terminal,
/// and the Console page is reached by its route with the app's socket opener and terminal: the
/// subprotocols reach the opener as the page gave them, and the terminal gets the opened socket.
///
/// Catches: a console that outlives its sign-in in the browser, and an app that drops the
/// subprotocols or the terminal on the way to the page.
#[test]
fn signing_out_closes_every_open_console() {
    let ui = Ui::start();
    ui.session(CSRF);
    ui.reply(&m("Hosts/List"), json!({"hosts": []}));
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    for _ in 0..3 {
        ui.reply(&m("Console/List"), json!({}));
    }
    ui.reply(&m("Console/Open"), opened(Some("10")));
    ui.reply_raw(support::LOGOUT, 200, "{}");
    let mut ui = ui;
    let out = ui.run(
        r#"
import { fakeDocument, settle, domNamed, fire } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  const sockets = [];
  const terminals = [];
  const app = startApp({
    doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000,
    every: () => () => {},
    openSocket: (path, handlers, protocols) => {
      const socket = { path, protocols, closes: 0 };
      sockets.push({ socket, handlers });
      return { send: () => {}, close: () => { socket.closes += 1; }, socket };
    },
    openTerminal: (element, opened) => {
      const terminal = { same: opened.socket === sockets[0].socket, disposes: 0 };
      terminals.push(terminal);
      return { show: () => {}, focus: () => {}, dispose: () => { terminal.disposes += 1; } };
    },
  });
  await settle();
  app.navigate("console", { nodeId: "node-02" });
  await settle();
  fire(domNamed(doc.root, "console-reason"), "input", { target: { value: "boot" } });
  fire(domNamed(doc.root, "open-console"), "click", {});
  await settle();
  sockets[0].handlers.open();
  await settle();
  const before = { closes: sockets[0].socket.closes, disposes: terminals[0].disposes };
  fire(domNamed(doc.root, "sign-out"), "click", {});
  await settle();
  return {
    path: sockets[0].socket.path, protocols: sockets[0].socket.protocols, same: terminals[0].same, before,
    closes: sockets[0].socket.closes, disposes: terminals[0].disposes, sockets: sockets.length,
  };
}"#,
    );
    assert_eq!(out["path"], json!(format!("/console/v1/stream/{SESSION}")));
    assert_eq!(
        out["protocols"],
        json!(["hostkeeper.console.v1", format!("csrf.{CSRF}")])
    );
    assert_eq!(out["same"], json!(true));
    assert_eq!(out["before"], json!({"closes": 0, "disposes": 0}));
    assert_eq!(out["closes"], json!(1));
    assert_eq!(out["disposes"], json!(1));
    assert_eq!(out["sockets"], json!(1));
}

/// Proves: a refusal of Console.Open is said in words for the refusals an operator can act on
/// (no console access, a host already held, too many sessions) and as its code and token
/// otherwise; no socket is opened and the sessions are read again (to show who holds the host).
///
/// Catches: a refused open that leaves the page saying nothing, or that opens a socket to no
/// session.
#[test]
fn a_refused_open_says_why_and_opens_no_socket() {
    let mut ui = ui_with(4);
    ui.refuse(
        &m("Console/Open"),
        403,
        "PERMISSION_DENIED",
        "no_console_access",
    );
    ui.refuse(&m("Console/Open"), 409, "ALREADY_EXISTS", "console_busy");
    ui.refuse(&m("Console/Open"), 503, "UNAVAILABLE", "device_unreachable");
    let out = run(
        &mut ui,
        r#"
const said = [];
for (let i = 0; i < 3; i++) {
  await open("boot");
  said.push(status());
}
return { said, sockets: seen.sockets.length, open: has("open-console") };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["said"],
        json!([
            "You are not named in the coordinator's console operators file, so you cannot open one.",
            "Someone already holds this host's console: see the open sessions below.",
            "The console did not open: UNAVAILABLE device_unreachable."
        ])
    );
    assert_eq!(out["sockets"], json!(0));
    assert_eq!(out["open"], json!(true));
    assert_eq!(
        ui.methods(),
        [
            m("Console/List"),
            m("Console/Open"),
            m("Console/List"),
            m("Console/Open"),
            m("Console/List"),
            m("Console/Open"),
            m("Console/List")
        ]
    );
}

/// Proves: Open the console sends nothing without a reason, with a reason that holds a control
/// character, or with one over 256 bytes, and says why; a reason of exactly 256 bytes is sent.
///
/// Catches: a call the coordinator refuses for its reason sent anyway, and an off-by-one at the
/// reason's limit.
#[test]
fn a_reason_is_required_and_bounded_before_anything_is_sent() {
    let mut ui = ui_with(2);
    ui.reply(&m("Console/Open"), opened(Some("10")));
    let out = run(
        &mut ui,
        r#"
const said = [];
for (const reason of ["   ", "a\tb", "x".repeat(257)]) {
  type(tree, "console-reason", reason);
  await click(tree, "open-console");
  said.push(seen.reports[seen.reports.length - 1]);
}
const before = seen.sockets.length;
await open("x".repeat(256));
return { said, before, after: seen.sockets.length };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["said"],
        json!([
            "Give a reason first.",
            "A reason has no control character (a tab or a line break, for example).",
            "A reason is at most 256 bytes; this one is 257."
        ])
    );
    assert_eq!(out["before"], json!(0));
    assert_eq!(out["after"], json!(1));
    let opens: Vec<Value> = ui
        .requests()
        .iter()
        .filter(|r| r.method() == m("Console/Open"))
        .map(|r| r.body())
        .collect();
    assert_eq!(
        opens,
        [json!({"hostId": "node-02", "reason": "x".repeat(256)})]
    );
}

/// Proves: a session that is not a serial one (a Mac's screen) is closed at once with
/// Console.Close of the host and no socket; and a page with no socket opener or no terminal
/// says it cannot open a console and calls nothing.
///
/// Catches: a screen session left holding the host's slot until its window ends, and a page that
/// opens a session it cannot attach.
#[test]
fn a_session_this_page_cannot_show_is_closed_at_once() {
    let mut ui = ui_with(2);
    ui.reply(
        &m("Console/Open"),
        json!({"kind": "CONSOLE_KIND_SCREEN", "session": SESSION, "expiresInSecs": "30"}),
    );
    ui.reply(&m("Console/Close"), json!({}));
    let out = run(
        &mut ui,
        r#"await open("look"); return { status: status(), sockets: seen.sockets.length };"#,
    );
    assert_eq!(
        out["out"],
        json!({"status": "node-02 is not a Linux server: its console is not a serial port, so it was closed.", "sockets": 0})
    );
    let closes: Vec<Value> = ui
        .requests()
        .iter()
        .filter(|r| r.method() == m("Console/Close"))
        .map(|r| r.body())
        .collect();
    assert_eq!(closes, [json!({"hostId": "node-02"})]);

    let mut ui = ui_with(1);
    let out = run(
        &mut ui,
        r#"
const said = [];
for (const missing of ["openSocket", "openTerminal"]) {
  const kept = ctx[missing];
  ctx[missing] = undefined;
  type(tree, "console-reason", "boot");
  await click(tree, "open-console");
  said.push(seen.reports[seen.reports.length - 1]);
  ctx[missing] = kept;
}
return said;"#,
    );
    assert_eq!(
        out["out"],
        json!([
            "This page cannot open a terminal.",
            "This page cannot open a terminal."
        ])
    );
    assert_eq!(ui.methods(), [m("Console/List")]);
}

/// Proves: when the sign-in ends while Console.Open is on its way, the session it opened is
/// closed at once and no socket is opened (there is no CSRF token to offer).
///
/// Catches: a socket opened with `csrf.null`, which the coordinator refuses, leaving the session
/// held until its window ends.
#[test]
fn a_sign_in_that_ends_during_open_closes_the_session() {
    let mut ui = ui_with(2);
    ui.reply(&m("Console/Close"), json!({}));
    let out = run(
        &mut ui,
        r#"
ctx.api = Object.assign({}, ctx.api, { console: Object.assign({}, ctx.api.console, {
  open: async () => { session.signOut(); return { kind: "CONSOLE_KIND_SERIAL", session: "ab" }; },
}) });
await open("boot");
return { status: status(), sockets: seen.sockets.length };"#,
    );
    assert_eq!(
        out["out"],
        json!({"status": "Sign in again, then open the console again.", "sockets": 0})
    );
}

/// Proves: Close closes the socket once, says so and reads the open sessions again; what the
/// closed socket does afterwards (a message, a close with a reason) changes nothing; and Open the
/// console after it opens a new socket and a new terminal, the old one ended.
///
/// Catches: a Close that leaves the socket open, and a late close of an old console shown over
/// the next one.
#[test]
fn close_closes_the_socket_and_ignores_what_it_sends_after() {
    let mut ui = ui_with(4);
    ui.reply(&m("Console/Open"), opened(Some("10")));
    ui.reply(&m("Console/Open"), opened(Some("10")));
    let out = run(
        &mut ui,
        r#"
const first = await open("boot");
first.handlers.open();
fire(domNamed(doc, "close-console"), "click", {});
await settle();
const closed = { closes: first.closes, status: status(), stops: seen.repeats[0].stops };
first.handlers.message(new Uint8Array([65]));
first.handlers.close({ code: 4007, reason: "closed_by_operator" });
first.handlers.open();
const late = { status: status(), sent: first.sent.length, terminals: seen.terminals.length };
const second = await open("boot");
second.handlers.open();
return { closed, late, sockets: seen.sockets.length, terminals: seen.terminals.length,
  firstDisposed: seen.terminals[0].disposes, secondSame: seen.terminals[1].socket === second };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["closed"],
        json!({"closes": 1, "status": "You closed the session.", "stops": 1})
    );
    assert_eq!(
        out["late"],
        json!({"status": "You closed the session.", "sent": 1, "terminals": 1})
    );
    assert_eq!(out["sockets"], json!(2));
    assert_eq!(out["terminals"], json!(2));
    assert_eq!(out["firstDisposed"], json!(1));
    assert_eq!(out["secondSame"], json!(true));
    assert_eq!(
        ui.methods(),
        [
            m("Console/List"),
            m("Console/Open"),
            m("Console/List"),
            m("Console/List"),
            m("Console/Open"),
            m("Console/List")
        ],
        "Close reads the open sessions again"
    );
}

/// Proves: when the page is drawn again (a status line, a power button's answer), the terminal
/// is moved into the new terminal box and not made again; a box drawn before the socket opened
/// gets the terminal when it opens.
///
/// Catches: a terminal lost (or a second one made on the same socket) each time the frame redraws.
#[test]
fn the_terminal_moves_into_the_box_of_each_drawing() {
    let mut ui = ui_with(2);
    ui.reply(&m("Console/Open"), opened(Some("10")));
    let out = run(
        &mut ui,
        r#"
const socket = await open("boot");
mounted(tree);
socket.handlers.open();
mounted(tree);
mounted(tree);
return { terminals: seen.terminals.length, into: seen.terminals[0].into };"#,
    );
    assert_eq!(
        out["out"],
        json!({"terminals": 1, "into": ["terminal", "terminal", "terminal"]})
    );
}

/// Proves: Close on a listed session asks first; no sends nothing; yes sends Console.Close of
/// that host, reads the sessions again and says it is closed; a list that cannot be read says so.
///
/// Catches: another operator's console closed without a question, and a failed read shown as no
/// session at all.
#[test]
fn closing_a_listed_session_asks_first_and_reads_the_list_again() {
    let mut ui = Ui::start();
    let held = json!({"sessions": [
        {"hostId": "node-04", "kind": "CONSOLE_KIND_SERIAL", "openedBy": "alice", "openedUnix": "1800000000"}]});
    ui.reply(&m("Console/List"), held.clone());
    ui.reply(&m("Console/Close"), json!({}));
    ui.reply(&m("Console/List"), json!({}));
    let out = ui.run(&driver(
        &format!("\"{CSRF}\""),
        "[false, true]",
        r#"
fire(domNamed(doc, "close-session"), "click", {});
await settle();
const declined = { said: seen.reports.slice() };
fire(domNamed(doc, "close-session"), "click", {});
await settle();
return { declined, questions: seen.questions, said: seen.reports, now: domTexts(domNamed(doc, "sessions")).join("|") };"#,
    ));
    let out = &out["out"];
    assert_eq!(out["declined"]["said"], json!(["Not closed."]));
    assert_eq!(
        out["questions"],
        json!([
            "Close the console session of node-04, held by alice?",
            "Close the console session of node-04, held by alice?"
        ])
    );
    assert_eq!(
        out["said"],
        json!(["Not closed.", "The console session of node-04 is closed."])
    );
    assert_eq!(out["now"], json!("No console session is open."));
    assert_eq!(
        ui.methods(),
        [m("Console/List"), m("Console/Close"), m("Console/List")]
    );

    let mut ui = Ui::start();
    ui.reply(
        &m("Console/List"),
        json!({"sessions": [{"hostId": "node-02", "openedBy": "bob", "attached": false}]}),
    );
    let out = ui.run(&driver(
        &format!("\"{CSRF}\""),
        "[]",
        r#"return { cells: elements(domNamed(doc, "sessions")).filter((e) => e.tagName === "TD").map((e) => domTexts(e).join("")) };"#,
    ));
    assert_eq!(
        out["out"]["cells"],
        json!(["node-02", "bob", "-", "no", "Close"])
    );

    let mut ui = Ui::start();
    ui.refuse(
        &m("Console/List"),
        403,
        "PERMISSION_DENIED",
        "no_console_access",
    );
    let out = run(
        &mut ui,
        r#"return domTexts(domNamed(doc, "sessions-failed")).join("");"#,
    );
    assert_eq!(
        out["out"],
        json!("The open sessions could not be read: PERMISSION_DENIED no_console_access.")
    );
}

/// Proves: the page shows the host's Power and boot section beside the console (its buttons send
/// Console.Power with the host), and its breadcrumb leads back to the host page.
///
/// Catches: a Console page without the power controls the incident needs next to the console.
#[test]
fn the_page_has_the_hosts_power_section_and_a_way_back_to_the_host() {
    let mut ui = ui_with(1);
    ui.reply(
        &m("Console/Power"),
        json!({"state": "POWER_STATE_ON", "sent": "POWER_ACTION_STATUS"}),
    );
    let out = ui.run(&driver(
        &format!("\"{CSRF}\""),
        "[true]",
        r#"
type(tree, "power-reason", "check");
await click(tree, "power-status");
await click(tree, "crumb-host");
return { navigations: seen.navigations };"#,
    ));
    assert_eq!(
        out["out"]["navigations"],
        json!([{"name": "host", "params": {"nodeId": "node-02"}}])
    );
    let power: Vec<Value> = ui
        .requests()
        .iter()
        .filter(|r| r.method() == m("Console/Power"))
        .map(|r| r.body())
        .collect();
    assert_eq!(
        power,
        [json!({"hostId": "node-02", "action": "POWER_ACTION_STATUS", "reason": "check"})]
    );
}

/// Proves: Home's list of servers has a Console column: a Console link on the row of each host
/// whose last heartbeat says Linux (either architecture), which opens the Console page of that
/// host, and none for a Mac (whose row has the Desktop link instead, `desktop_page.rs`) or a host
/// that has not said its platform.
///
/// Catches: a Console offered for a Mac (which has no serial console) or for a host of unknown
/// platform, and a link that opens another host's console.
#[test]
fn the_servers_list_links_each_linux_servers_console() {
    let mut ui = Ui::start();
    let host = |id: &str, platform: Option<&str>| {
        let mut host = json!({"nodeId": id, "live": true, "lastHeartbeatUnix": "1800000000"});
        if let Some(platform) = platform {
            host["heartbeat"] = json!({ "platform": platform });
        }
        host
    };
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [
            host("x86-1", Some("PLATFORM_LINUX_X86_64")),
            host("arm-1", Some("PLATFORM_LINUX_ARM64")),
            host("mac-1", Some("PLATFORM_DARWIN_ARM64")),
            host("new-1", None),
        ]}),
    );
    ui.reply(&m("Releases/Status"), json!({}));
    ui.reply(&m("Alerts/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    ui.reply(&m("Enrollment/ListRecords"), json!({}));
    let out = ui.run(
        r#"
import { api, recorder, rows, nodes } from "../test/kit.js";
import { render } from "./pages/home.js";
export async function main() {
  const { ctx, seen } = recorder(api("operator-token"), { list: true }, []);
  const tree = await render(ctx);
  const links = nodes(tree).filter((n) => n.attrs.name === "open-console");
  await links[1].on.click();
  return { table: rows(tree).map((r) => [r[0], r[6]]), navigations: seen.navigations };
}"#,
    );
    assert_eq!(
        out["table"],
        json!([
            ["Server", "Console"],
            ["arm-1", "Console"],
            ["mac-1", "Desktop"],
            ["new-1", "-"],
            ["x86-1", "Console"]
        ])
    );
    assert_eq!(
        out["navigations"],
        json!([{"name": "console", "params": {"nodeId": "x86-1"}}])
    );
}

/// Proves: when the operator leaves the page while Console.Open is on its way, the session it
/// opened is closed at once with Console.Close of the host, and no socket or terminal is made.
///
/// Catches: a console attached by a page nobody sees any more, holding the host's one session
/// until the coordinator's idle limit.
#[test]
fn leaving_while_the_console_opens_closes_the_session() {
    let mut ui = ui_with(1);
    ui.reply(&m("Console/Close"), json!({}));
    let out = run(
        &mut ui,
        r#"
ctx.api = Object.assign({}, ctx.api, { console: Object.assign({}, ctx.api.console, {
  open: async () => { leave(); return { kind: "CONSOLE_KIND_SERIAL", session: "ab", quietSecs: "10" }; },
}) });
await open("boot");
return { sockets: seen.sockets.length, terminals: seen.terminals.length };"#,
    );
    assert_eq!(out["out"], json!({"sockets": 0, "terminals": 0}));
    assert_eq!(ui.methods(), [m("Console/List"), m("Console/Close")]);
}
