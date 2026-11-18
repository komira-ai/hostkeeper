//! The key encoder (`ui/terminal_keys.js`): what a key event or a paste sends to the host.

mod support;

use serde_json::{Value, json};
use support::Ui;

/// The bytes `encodeKey` returns for each `[event, appCursor]`, `null` for none.
fn keys(events: Vec<(Value, bool)>) -> Vec<Value> {
    let input: Vec<Value> = events
        .iter()
        .map(|(event, app)| json!({"event": event, "app": app}))
        .collect();
    let driver = format!(
        r#"
import {{ encodeKey }} from "./terminal_keys.js";
const cases = {};
export async function main() {{
  return cases.map((c) => {{
    const bytes = encodeKey(c.event, c.app);
    return bytes === null ? null : Array.from(bytes);
  }});
}}"#,
        json!(input)
    );
    Ui::start()
        .run(&driver)
        .as_array()
        .expect("one answer per key")
        .clone()
}

fn key(name: &str) -> Value {
    json!({"key": name})
}

fn with(name: &str, modifiers: &str) -> Value {
    json!({"key": name, "shiftKey": modifiers.contains('s'), "altKey": modifiers.contains('a'),
           "ctrlKey": modifiers.contains('c'), "metaKey": modifiers.contains('m')})
}

fn bytes(text: &str) -> Value {
    json!(text.as_bytes())
}

fn check(table: Vec<(Value, bool, Value)>) {
    let got = keys(table.iter().map(|(e, a, _)| (e.clone(), *a)).collect());
    for ((event, app, want), got) in table.iter().zip(&got) {
        assert_eq!(got, want, "{event} (application cursor keys: {app})");
    }
}

/// Proves: printable keys send their UTF-8 bytes, Enter a carriage return, Backspace DEL, Tab a
/// tab, Shift+Tab the back-tab sequence and Escape an escape.
///
/// Catches: Enter sent as a line feed, Backspace sent as BS (which a shell does not take as
/// delete), and a letter with Shift sent as its lower case.
#[test]
fn printable_keys_and_the_editing_keys_send_their_bytes() {
    check(vec![
        (key("a"), false, bytes("a")),
        (key("A"), false, bytes("A")),
        (with("A", "s"), false, bytes("A")),
        (key("7"), false, bytes("7")),
        (key(" "), false, bytes(" ")),
        (key("\u{e9}"), false, bytes("\u{e9}")),
        (key("\u{20ac}"), false, bytes("\u{20ac}")),
        (key("\u{1f600}"), false, bytes("\u{1f600}")),
        (key("Enter"), false, bytes("\r")),
        (key("Backspace"), false, bytes("\u{7f}")),
        (key("Tab"), false, bytes("\t")),
        (with("Tab", "s"), false, bytes("\x1b[Z")),
        (key("Escape"), false, bytes("\x1b")),
    ]);
}

/// Proves: the cursor keys send `ESC [ A` to `D` and Home and End `ESC [ H` and `ESC [ F`, or `ESC O`
/// in place of `ESC [` when the host switched on application cursor keys; with a modifier they
/// send `ESC [ 1 ; m` and the letter, whatever the mode, `m` being 1 plus Shift (1), Alt (2) and
/// Ctrl (4).
///
/// Catches: application mode ignored (a full-screen program's arrow keys do nothing), a modifier
/// dropped from an arrow, and a modifier number that is off by one.
#[test]
fn cursor_keys_honour_application_mode_and_carry_their_modifiers() {
    let mut table = Vec::new();
    for (name, letter) in [
        ("ArrowUp", "A"),
        ("ArrowDown", "B"),
        ("ArrowRight", "C"),
        ("ArrowLeft", "D"),
        ("Home", "H"),
        ("End", "F"),
    ] {
        table.push((key(name), false, bytes(&format!("\x1b[{letter}"))));
        table.push((key(name), true, bytes(&format!("\x1bO{letter}"))));
        table.push((with(name, "s"), false, bytes(&format!("\x1b[1;2{letter}"))));
        table.push((with(name, "a"), true, bytes(&format!("\x1b[1;3{letter}"))));
        table.push((with(name, "c"), false, bytes(&format!("\x1b[1;5{letter}"))));
        table.push((with(name, "sac"), true, bytes(&format!("\x1b[1;8{letter}"))));
    }
    check(table);
}

/// Proves: Insert, Delete, PageUp, PageDown and F5 to F12 send `ESC [ n ~` with their numbers, F1 to
/// F4 send `ESC O P` to `S` (in either cursor mode), and a modifier is the second parameter.
///
/// Catches: the gap in the function key numbers (F5 is 15, F6 is 17, F11 is 23), and F1 to F4 sent
/// as `ESC [ P`.
#[test]
fn editing_and_function_keys_send_their_numbered_sequences() {
    let mut table = Vec::new();
    for (name, number) in [
        ("Insert", 2),
        ("Delete", 3),
        ("PageUp", 5),
        ("PageDown", 6),
        ("F5", 15),
        ("F6", 17),
        ("F7", 18),
        ("F8", 19),
        ("F9", 20),
        ("F10", 21),
        ("F11", 23),
        ("F12", 24),
    ] {
        table.push((key(name), false, bytes(&format!("\x1b[{number}~"))));
        table.push((key(name), true, bytes(&format!("\x1b[{number}~"))));
        table.push((with(name, "c"), false, bytes(&format!("\x1b[{number};5~"))));
        table.push((with(name, "sa"), false, bytes(&format!("\x1b[{number};4~"))));
    }
    for (name, letter) in [("F1", "P"), ("F2", "Q"), ("F3", "R"), ("F4", "S")] {
        table.push((key(name), false, bytes(&format!("\x1bO{letter}"))));
        table.push((key(name), true, bytes(&format!("\x1bO{letter}"))));
        table.push((with(name, "s"), false, bytes(&format!("\x1b[1;2{letter}"))));
    }
    check(table);
}

/// Proves: Ctrl with a letter sends its control byte (Ctrl+A is 1 to Ctrl+Z is 26, in either
/// case), Ctrl with `[`, `\`, `]`, `^`, `_`, Space and `@` the controls they stand for, Alt with a
/// printable key or Enter, Tab, Backspace or Escape an escape before it, and Ctrl+Alt with a
/// character (AltGr) the character itself.
///
/// Catches: Ctrl+C that does not interrupt, Alt+b and Alt+f that do not move by word in a shell,
/// and an AltGr character (an `@` on some keyboards) sent as a control.
#[test]
fn control_and_alt_combinations_send_control_bytes_and_escape_prefixes() {
    let mut table = vec![
        (with("a", "c"), false, json!([1])),
        (with("c", "c"), false, json!([3])),
        (with("C", "c"), false, json!([3])),
        (with("d", "c"), false, json!([4])),
        (with("z", "c"), false, json!([26])),
        (with("[", "c"), false, json!([27])),
        (with("\\", "c"), false, json!([28])),
        (with("]", "c"), false, json!([29])),
        (with("^", "c"), false, json!([30])),
        (with("6", "c"), false, json!([30])),
        (with("_", "c"), false, json!([31])),
        (with("/", "c"), false, json!([31])),
        (with(" ", "c"), false, json!([0])),
        (with("@", "c"), false, json!([0])),
        (with("?", "c"), false, json!([127])),
        (with("5", "c"), false, json!(null)),
        (with("b", "a"), false, bytes("\x1bb")),
        (with("f", "a"), false, bytes("\x1bf")),
        (with("\u{e9}", "a"), false, bytes("\x1b\u{e9}")),
        (with("Backspace", "a"), false, bytes("\x1b\u{7f}")),
        (with("Enter", "a"), false, bytes("\x1b\r")),
        (with("Tab", "a"), false, bytes("\x1b\t")),
        (with("Escape", "a"), false, bytes("\x1b\x1b")),
        (with("@", "ac"), false, bytes("@")),
        (with("e", "ac"), false, bytes("e")),
    ];
    table.push((with("ArrowUp", "c"), true, bytes("\x1b[1;5A")));
    check(table);
}

/// Proves: a key that sends nothing returns null: a modifier alone, a lock key, a dead key, a key
/// during composition, a Meta (Command) combination (the browser's), Ctrl+V and Ctrl+Shift+a letter
/// (paste and copy), an event with no key, and a key this terminal has no code for.
///
/// Catches: Ctrl+V that cannot paste and Ctrl+Shift+C that cannot copy, a Command shortcut taken by
/// the shell, and a modifier press that sends garbage.
#[test]
fn keys_that_send_nothing_and_the_browsers_shortcuts_return_null() {
    let none = json!(null);
    check(vec![
        (key("Shift"), false, none.clone()),
        (key("Control"), false, none.clone()),
        (key("Alt"), false, none.clone()),
        (key("Meta"), false, none.clone()),
        (key("CapsLock"), false, none.clone()),
        (key("Dead"), false, none.clone()),
        (key("Unidentified"), false, none.clone()),
        (key("ContextMenu"), false, none.clone()),
        (key("F13"), false, none.clone()),
        (key(""), false, none.clone()),
        (json!({}), false, none.clone()),
        (
            json!({"key": "a", "isComposing": true}),
            false,
            none.clone(),
        ),
        (with("c", "m"), false, none.clone()),
        (with("v", "m"), false, none.clone()),
        (with("ArrowLeft", "m"), false, none.clone()),
        (with("v", "c"), false, none.clone()),
        (with("V", "c"), false, none.clone()),
        (with("C", "cs"), false, none.clone()),
        (with("v", "cs"), false, none.clone()),
    ]);
}

/// Proves: pasted text goes out as UTF-8 with its line breaks as carriage returns; when the host
/// switched on bracketed paste it is between `ESC [ 200 ~` and `ESC [ 201 ~` with any escape in it
/// dropped; and a long one is cut into pieces of at most 16 KiB that together are the whole.
///
/// Catches: a paste whose newlines run the commands before the operator looks at them in a host
/// that asked for brackets, a paste that carries the closing bracket early, and a message over
/// the 16 KiB the coordinator takes.
#[test]
fn pasted_text_is_sent_with_carriage_returns_brackets_when_asked_and_in_pieces() {
    let driver = r#"
import { encodePaste, pieces, MESSAGE_LIMIT } from "./terminal_keys.js";
export async function main() {
  const plain = Array.from(encodePaste("ls\nid\r\né", false));
  const bracketed = Array.from(encodePaste("a\nb\u001b[201~c", true));
  const empty = Array.from(encodePaste("", true));
  const big = new Uint8Array(40000).fill(97);
  const parts = pieces(big);
  return {
    plain, bracketed, empty,
    sizes: parts.map((p) => p.length),
    joined: parts.reduce((n, p) => n + p.length, 0),
    limit: MESSAGE_LIMIT,
    none: pieces(new Uint8Array(0)).length,
    exact: pieces(new Uint8Array(16384)).map((p) => p.length),
  };
}"#;
    let out = Ui::start().run(driver);
    assert_eq!(out["plain"], json!("ls\rid\r\u{e9}".as_bytes()));
    assert_eq!(
        out["bracketed"],
        json!("\x1b[200~a\rb[201~c\x1b[201~".as_bytes())
    );
    assert_eq!(out["empty"], json!("\x1b[200~\x1b[201~".as_bytes()));
    assert_eq!(out["sizes"], json!([16384, 16384, 7232]));
    assert_eq!(out["joined"], json!(40000));
    assert_eq!(out["limit"], json!(16384));
    assert_eq!(out["none"], json!(0));
    assert_eq!(out["exact"], json!([16384]));
}
