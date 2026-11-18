//! The Remote SSH tab of the Host page (`ui/sshpanel.js`), run in the JavaScript engine
//! against the fake gateway and a fake socket: the pin of the host keys, the button that opens a
//! terminal, what the page sends over the socket, what it draws, and the words for every way a
//! terminal ends. Nothing here waits on a clock: the test delivers each message itself.

mod support;

use prost_reflect::DescriptorPool;
use serde_json::{Value, json};
use support::Ui;
use support::pages::m;

const FP1: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FP2: &str = "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const CSRF: &str = "operator-token";

/// The host the page is read for: live, its last heartbeat reporting `reported` host keys.
fn host(node: &str, reported: &[&str]) -> Value {
    let mut host = json!({"nodeId": node, "live": true, "lastHeartbeatUnix": "1800000000"});
    if !reported.is_empty() {
        host["heartbeat"] = json!({"ssh": {"hostKeyFingerprints": reported}});
    }
    host
}

/// A gateway that answers the reads of the Host page for `host`, and `Ssh/HostKeys` with `keys`
/// (`None` leaves it unanswered, which the gateway refuses as unimplemented).
fn ui_for(host: &Value, keys: Option<Value>) -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/Get"), json!({ "host": host }));
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    if let Some(keys) = keys {
        ui.reply(&m("Ssh/HostKeys"), keys);
    }
    ui
}

/// A driver that renders the Host page of `node` with the operator's session (`session` is the
/// CSRF token, or null for none), answers the page's questions with `answers`, then runs `then`
/// (JavaScript with `tree`, `ctx`, `seen`, `mount()` (the page mounted in a fake document, whose
/// root it returns), and the kit's helpers in scope) and returns what it returns.
fn driver(node: &str, session: &str, answers: &str, then: &str) -> String {
    format!(
        r#"
import {{ api, recorder, text, named, nodes, click, settle, mounted, domNamed, domTexts, elements, fire }} from "../test/kit.js";
import {{ utf8 }} from "./bytes.js";
import * as session from "./session.js";
import {{ render }} from "./pages/host.js";
import {{ CAUSES, causeWords }} from "./sshpanel.js";
export async function main() {{
  const {{ ctx, seen }} = recorder(api({session}), {{ nodeId: {node} }}, {answers});
  const tree = await render(ctx);
  const mount = () => mounted(tree).root;
  const status = (doc) => domTexts(domNamed(doc, "ssh-status")).join("");
  const screen = (doc) => domTexts(domNamed(doc, "terminal")).join("|");
  const bytes = (text) => utf8(text);
  const say = (socket, message) => socket.handlers.message(JSON.stringify(message));
  const out = await (async () => {{ {then} }})();
  return {{ seen, out: out === undefined ? null : out }};
}}"#,
        node = json!(node),
        session = session,
        answers = answers,
        then = then
    )
}

fn run(ui: &mut Ui, node: &str, then: &str) -> Value {
    ui.run(&driver(node, &format!("\"{CSRF}\""), "[]", then))
}

/// Proves: the tab shows the pin the coordinator holds: pinned keys are listed with no button to
/// pin; with no pin, or a changed one, the keys the host's last heartbeat reported are listed
/// with a "Pin these host keys" button; with none reported it says so and offers no button; and a
/// refused read is shown, with the terminal still openable.
///
/// Catches: a pin button offered for keys already pinned, a button with nothing to pin, and a
/// failed read that hides the terminal.
#[test]
fn the_tab_shows_the_pin_state_and_offers_to_pin_only_what_the_host_reported() {
    const READ: &str = r#"
const find = (name) => nodes(tree).filter((n) => n.attrs["name"] === name);
return {
  pin: find("ssh-pin").map(text),
  pinned: find("ssh-pinned").flatMap((n) => n.children.map(text)),
  reported: find("ssh-reported").flatMap((n) => n.children.map(text)),
  pinButton: find("pin-host-keys").length,
  unreported: find("ssh-unreported").map(text),
  open: find("open-terminal").length,
  tab: find("tab-ssh").map(text),
};"#;
    let cases = [
        (
            "pinned",
            host("web-2", &[FP1]),
            Some(json!({"state": "pinned", "fingerprints": [FP1, FP2]})),
        ),
        (
            "no pin",
            host("web-2", &[FP1, FP2]),
            Some(json!({"state": "no_pin"})),
        ),
        (
            "changed",
            host("web-2", &[FP2]),
            Some(json!({"state": "changed", "fingerprints": [FP1]})),
        ),
        (
            "no pin, none reported",
            host("web-2", &[]),
            Some(json!({"state": "no_pin"})),
        ),
        (
            "a state it does not know",
            host("web-2", &[]),
            Some(json!({"state": "odd"})),
        ),
        ("unreadable", host("web-2", &[FP1]), None),
    ];
    let mut got = Vec::new();
    for (name, host, keys) in cases {
        let mut ui = ui_for(&host, keys);
        let out = run(&mut ui, "web-2", READ);
        assert_eq!(
            out["out"]["open"],
            json!(1),
            "{name}: the terminal can be opened"
        );
        assert_eq!(out["out"]["tab"], json!(["Remote SSH"]), "{name}");
        got.push((name, out["out"].clone()));
    }
    let pin = |i: usize| {
        got[i].1["pin"][0]
            .as_str()
            .expect("the pin sentence")
            .to_owned()
    };
    assert_eq!(got[0].1["pinned"], json!([FP1, FP2]));
    assert_eq!(got[0].1["pinButton"], json!(0));
    assert_eq!(got[0].1["reported"], json!([]));
    assert!(pin(0).contains("are pinned"), "{}", pin(0));
    assert_eq!(got[1].1["pinned"], json!([]));
    assert_eq!(got[1].1["reported"], json!([FP1, FP2]));
    assert_eq!(got[1].1["pinButton"], json!(1));
    assert!(pin(1).contains("No host key is pinned"), "{}", pin(1));
    assert_eq!(got[2].1["pinned"], json!([FP1]));
    assert_eq!(got[2].1["reported"], json!([FP2]));
    assert_eq!(got[2].1["pinButton"], json!(1));
    assert!(pin(2).contains("other than the pinned ones"), "{}", pin(2));
    assert_eq!(got[3].1["pinButton"], json!(0));
    assert_eq!(
        got[3].1["unreported"],
        json!([
            "The host has not reported its SSH host keys yet; they can be pinned after its next heartbeat."
        ])
    );
    assert_eq!(got[4].1["pinButton"], json!(0));
    assert_eq!(pin(4), "The pin of the host's SSH host keys is \"odd\".");
    assert_eq!(got[5].1["pinButton"], json!(0));
    assert!(
        pin(5).starts_with("The coordinator did not say which SSH host keys are pinned: "),
        "{}",
        pin(5)
    );
}

/// Proves: Pin these host keys asks first, naming the host and listing the keys; a no sends
/// nothing and says so; a yes sends `Ssh/PinHostKeys` with exactly the host id and the keys the
/// heartbeat reported, and the tab then shows them as pinned with no button; a refusal is
/// reported and leaves the tab as it was.
///
/// Catches: a pin sent without the question, one that sends keys other than the reported ones (or
/// adds fields), and a tab that says pinned after a refusal.
#[test]
fn pinning_asks_first_and_sends_exactly_the_host_id_and_the_reported_keys() {
    let reported = host("web-2", &[FP1, FP2]);
    let mut ui = ui_for(&reported, Some(json!({"state": "no_pin"})));
    ui.reply(&m("Ssh/PinHostKeys"), json!({}));
    let out = ui.run(&driver(
        "web-2",
        &format!("\"{CSRF}\""),
        "[false, true]",
        r#"
await click(tree, "pin-host-keys");
const afterNo = seen.reports.slice();
await click(tree, "pin-host-keys");
return { afterNo };"#,
    ));
    let sent: Vec<_> = support::pages::bodies(&ui)
        .into_iter()
        .filter(|(method, _)| method.ends_with("Ssh/PinHostKeys"))
        .collect();
    assert_eq!(
        sent,
        [(
            m("Ssh/PinHostKeys"),
            json!({"hostId": "web-2", "fingerprints": [FP1, FP2]})
        )]
    );
    assert_eq!(out["out"]["afterNo"], json!(["Host keys not pinned."]));
    assert_eq!(
        out["seen"]["reports"],
        json!(["Host keys not pinned.", "SSH host keys of web-2 pinned."])
    );
    let questions = out["seen"]["questions"].as_array().expect("questions");
    assert_eq!(questions.len(), 2);
    for question in questions {
        let question = question.as_str().expect("a question");
        for piece in ["web-2", FP1, FP2] {
            assert!(question.contains(piece), "{piece} is not in: {question}");
        }
    }
}

/// Proves: after a pin the mounted tab draws the keys as pinned and drops the button, and a pin the
/// coordinator refuses is reported with its code and reason and leaves the button and the old
/// words.
///
/// Catches: a tab that keeps offering to pin what is pinned, and one that claims a pin the
/// coordinator refused.
#[test]
fn the_mounted_tab_shows_a_pin_when_it_succeeds_and_not_when_it_is_refused() {
    let reported = host("web-2", &[FP1]);
    let mut ui = ui_for(&reported, Some(json!({"state": "no_pin"})));
    ui.refuse(
        &m("Ssh/PinHostKeys"),
        409,
        "FAILED_PRECONDITION",
        "not_reported",
    );
    ui.reply(&m("Ssh/PinHostKeys"), json!({}));
    let out = ui.run(&driver(
        "web-2",
        &format!("\"{CSRF}\""),
        "[true, true]",
        r#"
const doc = mount();
const head = () => domTexts(domNamed(doc, "ssh-head")).join(" ");
const buttons = () => elements(doc).filter((e) => e.attributes["name"] === "pin-host-keys").length;
fire(domNamed(doc, "pin-host-keys"), "click", {});
await settle();
const refused = { head: head(), buttons: buttons(), reports: seen.reports.slice() };
fire(domNamed(doc, "pin-host-keys"), "click", {});
await settle();
return { refused, pinned: head(), buttons: buttons() };"#,
    ));
    let refused = &out["out"]["refused"];
    assert_eq!(refused["buttons"], json!(1));
    assert!(
        refused["head"]
            .as_str()
            .unwrap()
            .contains("No host key is pinned")
    );
    let report = refused["reports"][0].as_str().expect("a report");
    assert!(report.contains("not_reported"), "{report}");
    let pinned = out["out"]["pinned"].as_str().unwrap();
    assert!(
        pinned.contains("are pinned") && pinned.contains(FP1),
        "{pinned}"
    );
    assert_eq!(out["out"]["buttons"], json!(0));
}

/// Proves: Open a terminal opens exactly one socket, to `/v1/ssh/terminal?host=` and the host id
/// percent-encoded; sends nothing until the socket is open; then sends one text message, first,
/// holding the session's CSRF token and a 120 by 36 terminal; and a second click while the first
/// terminal is up opens no second socket.
///
/// Catches: a path with the host id unescaped, a CSRF token that is not the session's, a first
/// message that is binary or that carries more than the token and size, and two sockets from a
/// double click.
#[test]
fn open_a_terminal_opens_one_socket_to_the_hosts_path_and_sends_the_csrf_and_size_first() {
    let node = "rack 1/web?2";
    let mut ui = ui_for(
        &host(node, &[]),
        Some(json!({"state": "pinned", "fingerprints": [FP1]})),
    );
    let out = run(
        &mut ui,
        node,
        r#"
await click(tree, "open-terminal");
const opening = { sockets: seen.sockets.length, path: seen.sockets[0].path, sent: seen.sockets[0].sent.slice() };
await click(tree, "open-terminal");
seen.sockets[0].handlers.open();
return { opening, sockets: seen.sockets.length, sent: seen.sockets[0].sent, reports: seen.reports };"#,
    );
    let out = &out["out"];
    assert_eq!(out["opening"]["sockets"], json!(1));
    assert_eq!(
        out["opening"]["path"],
        json!("/v1/ssh/terminal?host=rack%201%2Fweb%3F2")
    );
    assert_eq!(
        out["opening"]["sent"],
        json!([]),
        "nothing is sent before the socket is open"
    );
    assert_eq!(out["sockets"], json!(1));
    assert_eq!(out["reports"], json!(["A terminal is already open."]));
    assert_eq!(
        out["sent"],
        json!([{"text": "{\"csrf\":\"operator-token\",\"cols\":120,\"rows\":36}"}])
    );
}

/// Proves: with no `ctx.openSocket` the tab says it cannot open a terminal, and with nobody signed
/// in it says to sign in; neither opens a socket.
///
/// Catches: a click that throws, or one that opens a terminal with no CSRF token to send.
#[test]
fn without_a_socket_opener_or_a_session_the_button_says_why_and_opens_nothing() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = ui.run(&driver(
        "web-2",
        "\"operator-token\"",
        "[]",
        r#"
ctx.openSocket = undefined;
await click(tree, "open-terminal");
return { reports: seen.reports, sockets: seen.sockets.length };"#,
    ));
    assert_eq!(
        out["out"]["reports"],
        json!(["This page cannot open a terminal."])
    );
    assert_eq!(out["out"]["sockets"], json!(0));
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = ui.run(&driver(
        "web-2",
        "\"operator-token\"",
        "[]",
        r#"
session.signOut();
await click(tree, "open-terminal");
return { reports: seen.reports, sockets: seen.sockets.length };"#,
    ));
    assert_eq!(
        out["out"]["reports"],
        json!(["Sign in again to open a terminal."])
    );
    assert_eq!(out["out"]["sockets"], json!(0));
}

/// Proves: keys go out as binary messages of their bytes once the coordinator says the terminal is
/// open (and not before, and not after it ends), a key that sends nothing is left to the browser,
/// the arrow keys follow the host's application cursor keys mode, and a paste is sent as binary
/// with carriage returns, bracketed once the host asked for it, in pieces of at most 16 KiB.
///
/// Catches: keystrokes sent as text, keys typed into a terminal that is not open, a handled key the
/// browser still acts on, and a large paste in one message.
#[test]
fn typed_keys_and_pastes_go_out_as_binary_messages_only_while_the_terminal_is_open() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        r#"
const box = named(tree, "terminal")[0];
let prevented = 0;
const press = (event) => box.on.keydown(Object.assign({ preventDefault: () => { prevented += 1; } }, event));
const paste = (text) => box.on.paste({ clipboardData: { getData: () => text }, preventDefault: () => { prevented += 1; } });
await click(tree, "open-terminal");
const socket = seen.sockets[0];
socket.handlers.open();
press({ key: "x" });
paste("early");
const before = socket.sent.length;
say(socket, { type: "status", step: "opened", login_user: "ops" });
press({ key: "a" });
press({ key: "Enter" });
press({ key: "c", ctrlKey: true });
press({ key: "ArrowUp" });
press({ key: "Shift", shiftKey: true });
press({ key: "v", metaKey: true });
const typed = prevented;
socket.handlers.message(bytes("\u001b[?1h"));
press({ key: "ArrowUp" });
paste("ls\npwd");
socket.handlers.message(bytes("\u001b[?2004h"));
paste("a\nb");
paste("");
box.on.paste({ preventDefault: () => { prevented += 1; } });
paste("z".repeat(40000));
const sizes = socket.sent.slice(-3).map((s) => s.bytes.length);
const kinds = socket.sent.slice(1).map((s) => Object.keys(s)[0]);
say(socket, { type: "closed", cause: "exit" });
const after = socket.sent.length;
press({ key: "q" });
paste("late");
return { before, sent: socket.sent.slice(1, 12), typed, prevented, sizes, kinds: [...new Set(kinds)], after, final: socket.sent.length };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["before"],
        json!(1),
        "only the first message went out before the terminal was open"
    );
    let sent = out["sent"].as_array().expect("sent");
    let bytes = |i: usize| sent[i]["bytes"].clone();
    assert_eq!(bytes(0), json!([97]));
    assert_eq!(bytes(1), json!([13]));
    assert_eq!(bytes(2), json!([3]));
    assert_eq!(bytes(3), json!([27, 91, 65]));
    assert_eq!(
        out["typed"],
        json!(4),
        "Shift alone and Cmd+V are left to the browser"
    );
    assert_eq!(bytes(4), json!([27, 79, 65]), "application cursor keys");
    assert_eq!(bytes(5), json!("ls\rpwd".as_bytes()));
    assert_eq!(bytes(6), json!("\x1b[200~a\rb\x1b[201~".as_bytes()));
    assert_eq!(out["prevented"], json!(4 + 1 + 1 + 1 + 1 + 1 + 1));
    assert_eq!(out["sizes"], json!([16384, 16384, 7244]));
    assert_eq!(out["kinds"], json!(["bytes"]));
    assert_eq!(
        out["after"], out["final"],
        "nothing is sent once the terminal has ended"
    );
}

/// Proves: the host's bytes are drawn in the page: text, a colour as a class, the cursor, and the
/// box scrolled to the newest line; the screen survives the page being drawn again (a change of
/// tab) and goes on in the new elements; and the status line says where the coordinator is in
/// opening the terminal, in words, and who it logged in as.
///
/// Catches: output that is received and not shown, a terminal that goes blank when the tab is
/// changed, a screen that goes on drawing into elements that left the page, and a status line that
/// shows the wire's step names.
#[test]
fn the_hosts_output_is_drawn_and_survives_a_change_of_tab_and_the_status_is_in_words() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        r#"
const doc = mount();
fire(domNamed(doc, "open-terminal"), "click", {});
await settle();
const socket = seen.sockets[0];
const words = [status(doc)];
socket.handlers.open();
say(socket, { type: "status", step: "signing" });
words.push(status(doc));
say(socket, { type: "status", step: "connecting" });
words.push(status(doc));
say(socket, { type: "status", step: "weird" });
words.push(status(doc));
say(socket, { type: "status", step: "opened", login_user: "ops" });
words.push(status(doc));
socket.handlers.message(bytes("hello\r\n\u001b[31mred\u001b[0m é"));
const box = domNamed(doc, "terminal");
const red = elements(box).find((e) => (e.attributes["class"] || "").includes("tf1"));
const first = { screen: screen(doc), red: domTexts(red).join(""), scrolled: box.scrollTop, connected: domTexts(domNamed(doc, "ssh-head")).join(" ") };
const second = mount();
const remounted = { screen: screen(second), status: status(second) };
socket.handlers.message(bytes("\r\nmore"));
return { words, first, remounted, again: screen(second), old: screen(doc) };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["words"],
        json!([
            "Connecting to the coordinator.",
            "The coordinator is signing a certificate for you.",
            "The coordinator is connecting to the host.",
            "The coordinator is connecting to the host.",
            "Connected as ops."
        ])
    );
    assert!(out["first"]["screen"].as_str().unwrap().contains("hello"));
    assert!(out["first"]["screen"].as_str().unwrap().contains("\u{e9}"));
    assert_eq!(out["first"]["red"], json!("red"));
    assert_eq!(out["first"]["scrolled"], json!(500));
    assert!(
        out["first"]["connected"]
            .as_str()
            .unwrap()
            .contains("connected")
    );
    assert!(
        out["remounted"]["screen"]
            .as_str()
            .unwrap()
            .contains("hello")
    );
    assert_eq!(out["remounted"]["status"], json!("Connected as ops."));
    assert!(out["again"].as_str().unwrap().contains("more"));
    assert!(
        !out["old"].as_str().unwrap().contains("more"),
        "the old elements are not drawn into"
    );
}

/// The tokens of `SshRefusal` as the coordinator sends them: each value's name without its prefix,
/// in lower case, but for the unspecified one.
fn refusals() -> Vec<String> {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("descriptors");
    let refusal = pool
        .get_enum_by_name("hostkeeper.operator.v1.SshRefusal")
        .expect("SshRefusal");
    refusal
        .values()
        .map(|v| v.name().to_owned())
        .filter(|n| n != "SSH_REFUSAL_UNSPECIFIED")
        .map(|n| n.trim_start_matches("SSH_REFUSAL_").to_lowercase())
        .collect()
}

/// Proves: the last `closed` message ends the terminal and its cause is shown in plain words for
/// every cause the coordinator sends (each refusal of `SshRefusal`, each close cause of a tunnel,
/// `no_pin`, `host_key_mismatch`, `auth_refused`, `shell_refused`, `bad_request` and `exit`), the
/// words are different for different causes and are never the wire token, the page closes its
/// end of the socket, and Open a terminal is offered again; a token the page has no words for is
/// shown as it came.
///
/// Catches: a refusal added to the proto with no words for it, a cause shown as its token, and a
/// terminal that stays "open" after the coordinator ended it.
#[test]
fn every_cause_a_terminal_ends_with_is_shown_in_words() {
    let mut tokens = refusals();
    assert_eq!(tokens.len(), 18, "SshRefusal has 18 refusals");
    let tunnel = [
        "eof",
        "idle",
        "max_session",
        "token_revoked",
        "revoked",
        "not_permitted",
        "host_closed",
        "coordinator_stopping",
        "instance_lost",
        "error",
    ];
    let terminal = [
        "no_pin",
        "host_key_mismatch",
        "auth_refused",
        "shell_refused",
        "bad_request",
        "exit",
    ];
    for token in tunnel.iter().chain(&terminal) {
        if !tokens.contains(&(*token).to_owned()) {
            tokens.push((*token).to_owned());
        }
    }
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        &format!(
            r#"
const doc = mount();
const tokens = {};
const words = {{}};
const closes = [];
for (const [i, cause] of tokens.entries()) {{
  fire(domNamed(doc, "open-terminal"), "click", {{}});
  await settle();
  const socket = seen.sockets[i];
  socket.handlers.open();
  say(socket, {{ type: "status", step: "opened", login_user: "ops" }});
  say(socket, {{ type: "closed", cause }});
  words[cause] = status(doc);
  closes.push(socket.closes);
}}
fire(domNamed(doc, "open-terminal"), "click", {{}});
await settle();
const last = seen.sockets[tokens.length];
last.handlers.open();
say(last, {{ type: "closed", cause: "something_new" }});
const unknown = status(doc);
fire(domNamed(doc, "open-terminal"), "click", {{}});
await settle();
const next = seen.sockets[tokens.length + 1];
say(next, {{ type: "closed" }});
return {{ words, closes, unknown, missing: status(doc), table: Object.keys(CAUSES).sort(), single: causeWords("exit") }};"#,
            json!(tokens)
        ),
    );
    let out = &out["out"];
    let words = out["words"].as_object().expect("words");
    let mut seen = std::collections::BTreeSet::new();
    for token in &tokens {
        let text = words[token].as_str().expect("a sentence");
        assert!(!text.is_empty(), "{token}");
        assert!(
            !text.starts_with("The terminal ended ("),
            "{token} has no words: {text}"
        );
        assert!(!text.contains('_'), "{token} is shown as its token: {text}");
        assert!(
            seen.insert(text.to_owned()),
            "{token} has the words of another cause: {text}"
        );
    }
    let mut table: Vec<String> = tokens.clone();
    table.sort();
    assert_eq!(
        out["table"],
        json!(table),
        "the page has words for exactly these causes"
    );
    assert_eq!(
        out["closes"],
        json!(vec![1; tokens.len()]),
        "the page closes its end of the socket"
    );
    assert_eq!(words["exit"], json!("The shell exited."));
    assert_eq!(
        words["no_pin"],
        json!(
            "No host key is pinned for this host. Pin the host's keys (above), then open the terminal again."
        )
    );
    assert_eq!(
        words["host_key_mismatch"],
        json!(
            "The host presented an SSH host key that is not the pinned one, so nothing was sent to it. It may be another machine."
        )
    );
    assert_eq!(
        words["not_permitted"],
        json!("You are not permitted a terminal on this host.")
    );
    assert_eq!(
        words["token_revoked"],
        json!(
            "Your sign-in ended (you signed out, it expired, or your account was removed), so the terminal was closed."
        )
    );
    assert_eq!(out["unknown"], json!("The terminal ended (something_new)."));
    assert_eq!(out["missing"], json!("The terminal ended (unknown)."));
    assert_eq!(out["single"], json!("The shell exited."));
}

/// Proves: a socket that closes with no `closed` message says so, in one sentence if it had been
/// open and in another if it never opened (a refused upgrade), and either way Open a terminal is
/// offered again and the keys go nowhere.
///
/// Catches: a dropped connection that leaves the terminal looking alive, and a refused upgrade
/// that shows no reason at all.
#[test]
fn a_socket_that_closes_with_no_closed_message_says_so() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        r#"
const doc = mount();
const open = async () => { fire(domNamed(doc, "open-terminal"), "click", {}); await settle(); return seen.sockets[seen.sockets.length - 1]; };
let socket = await open();
socket.handlers.close();
const refused = status(doc);
socket = await open();
socket.handlers.open();
say(socket, { type: "status", step: "opened", login_user: "ops" });
socket.handlers.close();
const dropped = status(doc);
const again = elements(doc).filter((e) => e.attributes["name"] === "open-terminal").length;
const before = socket.sent.length;
domNamed(doc, "terminal").listeners.keydown.forEach((l) => l({ key: "a", preventDefault() {} }));
return { refused, dropped, again, sockets: seen.sockets.length, unsent: socket.sent.length === before, closes: socket.closes };"#,
    );
    let out = &out["out"];
    assert_eq!(
        out["refused"],
        json!(
            "The coordinator did not accept the connection. Your sign-in may have ended: sign in again and retry."
        )
    );
    assert_eq!(
        out["dropped"],
        json!("The connection to the coordinator closed without saying why.")
    );
    assert_eq!(out["again"], json!(1));
    assert_eq!(out["sockets"], json!(2));
    assert_eq!(out["unsent"], json!(true));
    assert_eq!(
        out["closes"],
        json!(0),
        "a socket that is closed is not closed again"
    );
}

/// Proves: Close closes the socket and says so; whatever the closed socket sends afterwards (its
/// open event, a message, a close) changes nothing; and Open a terminal after it opens a new one.
///
/// Catches: a Close that leaves the socket open, and a late message from a closed terminal drawn
/// over the next one.
#[test]
fn close_closes_the_socket_and_ignores_what_it_sends_after() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        r#"
const doc = mount();
fire(domNamed(doc, "open-terminal"), "click", {});
await settle();
const socket = seen.sockets[0];
socket.handlers.open();
say(socket, { type: "status", step: "opened", login_user: "ops" });
socket.handlers.message(bytes("shown"));
fire(domNamed(doc, "close-terminal"), "click", {});
await settle();
const closed = { status: status(doc), closes: socket.closes, open: elements(doc).filter((e) => e.attributes["name"] === "open-terminal").length };
const sentBefore = socket.sent.length;
socket.handlers.open();
socket.handlers.message(bytes("ghost"));
say(socket, { type: "closed", cause: "exit" });
socket.handlers.close();
const after = { status: status(doc), screen: screen(doc), unsent: socket.sent.length === sentBefore, closes: socket.closes };
fire(domNamed(doc, "open-terminal"), "click", {});
await settle();
return { closed, after, sockets: seen.sockets.length, path: seen.sockets[1].path };"#,
    );
    let out = &out["out"];
    assert_eq!(out["closed"]["status"], json!("You closed the terminal."));
    assert_eq!(out["closed"]["closes"], json!(1));
    assert_eq!(out["closed"]["open"], json!(1));
    assert_eq!(out["after"]["status"], json!("You closed the terminal."));
    assert!(out["after"]["screen"].as_str().unwrap().contains("shown"));
    assert!(!out["after"]["screen"].as_str().unwrap().contains("ghost"));
    assert_eq!(out["after"]["unsent"], json!(true));
    assert_eq!(out["after"]["closes"], json!(1));
    assert_eq!(out["sockets"], json!(2));
    assert_eq!(out["path"], json!("/v1/ssh/terminal?host=web-2"));
}

/// Proves: when the terminal's box has left the page (the tab was drawn again as a different
/// page's), the next output closes the socket and the terminal says so, so a terminal nobody can
/// see does not stay open.
///
/// Catches: a shell left running behind a page the operator has left.
#[test]
fn output_for_a_terminal_that_left_the_page_closes_it() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        r#"
const doc = mount();
fire(domNamed(doc, "open-terminal"), "click", {});
await settle();
const socket = seen.sockets[0];
socket.handlers.open();
say(socket, { type: "status", step: "opened", login_user: "ops" });
domNamed(doc, "terminal").isConnected = false;
socket.handlers.message(bytes("x"));
return { closes: socket.closes, status: status(doc) };"#,
    );
    assert_eq!(out["out"]["closes"], json!(1));
    assert_eq!(out["out"]["status"], json!("You left the terminal."));
}

/// Proves: when the Host page is drawn again (a refresh builds a new panel) while a terminal sits
/// idle, the old terminal's socket is closed at once, without any output from the host, and the
/// new panel can open a terminal of its own; a terminal the operator already closed is not closed
/// twice.
///
/// Catches: an idle shell kept open, with its tunnel and certificate session, behind a page nobody
/// can see, until the server's idle limit.
#[test]
fn drawing_the_page_again_closes_an_idle_terminal() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    // The page is read twice: the redraw asks the gateway again.
    for (method, reply) in [
        ("Hosts/Get", json!({"host": host("web-2", &[])})),
        ("Services/List", json!({})),
        ("Layout/Get", json!({})),
        ("Ssh/HostKeys", json!({"state": "pinned"})),
    ] {
        ui.reply(&m(method), reply);
    }
    let out = run(
        &mut ui,
        "web-2",
        r#"
const doc = mount();
fire(domNamed(doc, "open-terminal"), "click", {});
await settle();
const socket = seen.sockets[0];
socket.handlers.open();
say(socket, { type: "status", step: "opened", login_user: "ops" });
const before = socket.closes;
const again = mounted(await render(ctx)).root;
const afterRedraw = socket.closes;
fire(domNamed(again, "open-terminal"), "click", {});
await settle();
const second = seen.sockets[1];
second.handlers.open();
say(second, { type: "status", step: "opened", login_user: "ops" });
fire(domNamed(again, "close-terminal"), "click", {});
await settle();
await render(ctx);
return { before, afterRedraw, sockets: seen.sockets.length, secondCloses: second.closes };"#,
    );
    assert_eq!(out["out"]["before"], json!(0));
    assert_eq!(out["out"]["afterRedraw"], json!(1));
    assert_eq!(out["out"]["sockets"], json!(2));
    assert_eq!(out["out"]["secondCloses"], json!(1));
}

/// Proves: text messages the page does not understand (not JSON, not an object, an unknown type, a
/// status step it has no words for) change nothing.
///
/// Catches: a malformed message from the coordinator that throws in the page or blanks its status.
#[test]
fn text_messages_that_are_not_status_or_closed_are_ignored() {
    let mut ui = ui_for(&host("web-2", &[]), Some(json!({"state": "pinned"})));
    let out = run(
        &mut ui,
        "web-2",
        r#"
const doc = mount();
fire(domNamed(doc, "open-terminal"), "click", {});
await settle();
const socket = seen.sockets[0];
socket.handlers.open();
const before = status(doc);
for (const text of ["not json", "5", "null", "\"text\"", "{\"type\":\"other\"}", "{}"]) {
  socket.handlers.message(text);
}
return { before, after: status(doc), closes: socket.closes };"#,
    );
    assert_eq!(out["out"]["before"], out["out"]["after"]);
    assert_eq!(out["out"]["closes"], json!(0));
}

/// Proves: through the app, the page is given the opener the app was started with, and the first
/// message carries the CSRF token of the session the app read at start.
///
/// Catches: an app that does not hand `openSocket` to its pages, and a token that is not the
/// signed-in session's.
#[test]
fn the_app_gives_pages_its_socket_opener_and_the_sessions_csrf_token() {
    let ui = Ui::start();
    ui.session("csrf_5b2e91c7d4a06f38");
    for _ in 0..4 {
        ui.reply(&m("Hosts/List"), json!({"hosts": []}));
        ui.reply(&m("Releases/Status"), json!({}));
        ui.reply(&m("Alerts/List"), json!({}));
        ui.reply(&m("Layout/Get"), json!({}));
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
    }
    ui.reply(&m("Hosts/Get"), json!({"host": host("web-2", &[])}));
    ui.reply(&m("Ssh/HostKeys"), json!({"state": "pinned"}));
    let mut ui = ui;
    let out = ui.run(
        r#"
import { fakeDocument, settle, domNamed, fire } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  const opened = [];
  const sent = [];
  const app = startApp({
    doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000,
    openSocket: (path, handlers) => {
      opened.push({ path, handlers });
      return { send: (data) => sent.push(data), close: () => {} };
    },
  });
  await settle();
  app.navigate("host", { nodeId: "web-2" });
  await settle();
  fire(domNamed(doc.root, "tab-ssh"), "click", {});
  await settle();
  fire(domNamed(doc.root, "open-terminal"), "click", {});
  await settle();
  opened[0].handlers.open();
  return { paths: opened.map((o) => o.path), sent };
}"#,
    );
    assert_eq!(out["paths"], json!(["/v1/ssh/terminal?host=web-2"]));
    assert_eq!(
        out["sent"],
        json!(["{\"csrf\":\"csrf_5b2e91c7d4a06f38\",\"cols\":120,\"rows\":36}"])
    );
}

/// Proves: through the app, an idle terminal (no byte from the host) is closed as soon as the
/// operator goes to another page, and one is closed when the operator signs out; the socket of a
/// terminal the operator stays with is not.
///
/// Catches: a terminal left open, unseen, after its page is gone, holding a tunnel and a
/// certificate's session until the coordinator's idle limit.
#[test]
fn leaving_the_page_or_signing_out_closes_an_idle_terminal() {
    let ui = Ui::start();
    ui.session("csrf_5b2e91c7d4a06f38");
    for _ in 0..6 {
        ui.reply(&m("Hosts/List"), json!({"hosts": []}));
        ui.reply(&m("Releases/Status"), json!({}));
        ui.reply(&m("Alerts/List"), json!({}));
        ui.reply(&m("Layout/Get"), json!({}));
        ui.reply(&m("Enrollment/ListRecords"), json!({}));
        ui.reply(&m("Hosts/Get"), json!({"host": host("web-2", &[])}));
        ui.reply(&m("Ssh/HostKeys"), json!({"state": "pinned"}));
    }
    let mut ui = ui;
    let out = ui.run(
        r#"
import { fakeDocument, settle, domNamed, fire } from "../test/kit.js";
import { startApp } from "./app.js";
export async function main() {
  const doc = fakeDocument();
  const closes = [];
  const opened = [];
  const app = startApp({
    doc, root: doc.root, fetch: gatewayFetch, confirm: () => true, now: () => 1800000000,
    openSocket: (path, handlers) => {
      const at = closes.length;
      closes.push(0);
      opened.push(handlers);
      return { send: () => {}, close: () => { closes[at] += 1; } };
    },
  });
  await settle();
  const open = async () => {
    app.navigate("host", { nodeId: "web-2" });
    await settle();
    fire(domNamed(doc.root, "tab-ssh"), "click", {});
    await settle();
    fire(domNamed(doc.root, "open-terminal"), "click", {});
    await settle();
    opened[opened.length - 1].open();
    await settle();
  };
  await open();
  const stayed = closes[0];
  app.navigate("home", {});
  await settle();
  const leftPage = closes[0];
  await open();
  const beforeSignOut = closes[1];
  fire(domNamed(doc.root, "sign-out"), "click", {});
  await settle();
  return { stayed, leftPage, beforeSignOut, signedOut: closes[1], opened: closes.length };
}"#,
    );
    assert_eq!(out["stayed"], json!(0));
    assert_eq!(out["leftPage"], json!(1));
    assert_eq!(out["beforeSignOut"], json!(0));
    assert_eq!(out["signedOut"], json!(1));
    assert_eq!(out["opened"], json!(2));
}
