//! The Console page's terminal adapter (`ui/terminal_port.js`, what `ui/main.js` hands the app as
//! `openTerminal`), run in the JavaScript engine with the vendored attach addon itself
//! (`ui/vendor/xterm-addon-attach/addon-attach.js`, byte for byte as pinned) and a recording
//! stand-in for xterm.js's `Terminal`, which draws into a real document the engine has not. A
//! socket the test drives stands in for the browser's: nothing here waits on a clock.

mod support;

use hostkeeper_ui_tests::UiFile;
use serde_json::{Value, json};
use support::Ui;

/// The vendored attach addon's text, as the browser runs it.
fn addon() -> &'static str {
    UiFile::named("vendor/xterm-addon-attach/addon-attach.js")
        .expect("the addon is vendored")
        .text
}

/// Runs `then` (JavaScript with `terminal` (what `openTerminal` returned), `made` (the stand-in
/// terminal's record), `socket` (the driven socket), `element` and `into` in scope) after the
/// adapter has opened a terminal on the socket, and returns what it returns.
fn run(then: &str) -> Value {
    let mut ui = Ui::start();
    let driver = String::from(
        r#"
import { openTerminal, SCROLLBACK } from "./terminal_port.js";
// What the browser gives a classic script: the global object as `self`, and the socket class's
// state numbers, which the addon reads.
globalThis.self = globalThis;
globalThis["Web" + "Socket"] = { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 };
"#,
    ) + addon()
        + r#"
// A terminal that records what is done to it, and gives its listeners to the test.
class Terminal {
  constructor(options) {
    this.made = { options, opened: [], written: [], addons: 0, refreshed: [], focuses: 0, disposes: 0 };
    this.listeners = { data: [], binary: [] };
    this.rows = 24;
    this.element = { parentNode: null };
    globalThis.made = this.made;
    globalThis.term = this;
  }
  open(element) { this.made.opened.push(element["name"]); this.element.parentNode = element; }
  loadAddon(addon) { this.made.addons += 1; addon.activate(this); }
  write(data) { this.made.written.push(typeof data === "string" ? { text: data } : { bytes: Array.from(data) }); }
  onData(listener) { this.listeners["data"].push(listener); return { dispose: () => { this.listeners["data"] = []; } }; }
  onBinary(listener) { this.listeners.binary.push(listener); return { dispose: () => { this.listeners.binary = []; } }; }
  refresh(start, end) { this.made.refreshed.push([start, end]); }
  focus() { this.made.focuses += 1; }
  dispose() { this.made.disposes += 1; }
}
// An open socket the test drives: its listeners by event, what is sent on it.
function drivenSocket() {
  const socket = {
    readyState: 1, binaryType: "blob", listeners: {}, sent: [],
    addEventListener(type, listener) { (this.listeners[type] = this.listeners[type] || []).push(listener); },
    removeEventListener(type, listener) { this.listeners[type] = (this.listeners[type] || []).filter((l) => l !== listener); },
    send(data) { this.sent.push(typeof data === "string" ? { text: data } : { bytes: Array.from(data) }); },
    emit(type, event) { for (const listener of (this.listeners[type] || []).slice()) { listener(event); } },
  };
  return socket;
}
export async function main() {
  const socket = drivenSocket();
  const element = { name: "terminal", appendChild() {} };
  const into = { name: "terminal-2", children: [], appendChild(child) { this.children.push(child); child.parentNode = this; } };
  const terminal = openTerminal(element, { socket, send() {}, close() {} }, { Terminal, AttachAddon: globalThis.AttachAddon });
  const term = globalThis.term;
  const made = globalThis.made;
  const out = await (async () => { "#
        + then
        + r#" })();
  return out === undefined ? null : out;
}"#;
    ui.run(&driver)
}

/// Proves: the adapter makes one terminal in the page's element with the scrollback it names,
/// hands the attach addon the socket itself (which the addon sets to read binary messages as
/// array buffers), and what the socket brings reaches the terminal: a binary message as its bytes,
/// a text message as its text.
///
/// Catches: a terminal that shows nothing of the host's output (an addon given the wrapper of the
/// opener and not the socket, or never loaded).
#[test]
fn what_the_socket_brings_reaches_the_terminal() {
    let out = run(r#"
socket.emit("message", { data: new Uint8Array([108, 111, 103, 105, 110, 58, 32]).buffer });
socket.emit("message", { data: "\r\n" });
return { made, binaryType: socket.binaryType, scrollback: SCROLLBACK };"#);
    assert_eq!(out["binaryType"], json!("arraybuffer"));
    assert_eq!(out["made"]["opened"], json!(["terminal"]));
    assert_eq!(out["made"]["addons"], json!(1));
    assert_eq!(
        out["made"]["options"],
        json!({"convertEol": false, "scrollback": 2000})
    );
    assert_eq!(
        out["made"]["written"],
        json!([{"bytes": [108, 111, 103, 105, 110, 58, 32]}, {"text": "\r\n"}])
    );
}

/// Proves: what the operator types reaches the socket: the terminal's text as a text message, its
/// binary reports as their bytes; and once the socket closes the addon lets go of both ways (a
/// late message is not drawn, a late key is not sent).
///
/// Catches: a console that shows the host but takes no keys, and keys or output crossing a
/// socket that has closed.
#[test]
fn what_the_operator_types_reaches_the_socket_until_it_closes() {
    let out = run(r#"
term.listeners.data.forEach((l) => l("root\r"));
term.listeners.binary.forEach((l) => l("\u001b[M !!"));
socket.emit("close", { code: 1000, reason: "" });
socket.emit("message", { data: "late" });
const keys = term.listeners.data.length;
return { sent: socket.sent, written: made.written, keys, left: Object.values(socket.listeners).every((l) => l.length === 0) };"#);
    assert_eq!(
        out["sent"],
        json!([{"text": "root\r"}, {"bytes": [27, 91, 77, 32, 33, 33]}])
    );
    assert_eq!(out["written"], json!([]));
    assert_eq!(out["keys"], json!(0));
    assert_eq!(out["left"], json!(true));
}

/// Proves: `show` moves the terminal into the element of a new drawing and draws all its rows
/// again, and does nothing when the terminal is already there; `focus` and `dispose` reach the
/// terminal.
///
/// Catches: a terminal lost when the page is drawn again, and one drawn again for nothing.
#[test]
fn show_moves_the_terminal_and_focus_and_dispose_reach_it() {
    let out = run(r#"
terminal.show(into);
terminal.show(into);
terminal.focus();
terminal.dispose();
return { moved: into.children.length, refreshed: made.refreshed, focuses: made.focuses, disposes: made.disposes };"#);
    assert_eq!(
        out,
        json!({"moved": 1, "refreshed": [[0, 23]], "focuses": 1, "disposes": 1})
    );
}
