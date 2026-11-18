//! The terminal's model (`ui/terminal.js`), the look and the modes: SGR colours and attributes, the
//! alternate screen, the private modes, UTF-8 across writes, wide and combining characters,
//! sequences the terminal ignores, and the view the page draws.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::screen::{case, check, run};

/// What `cellAt(0, 0)` returns after `text` is written, then one `X`: the style the sequences set.
fn style_after(sequences: &[&str]) -> Vec<Value> {
    let cases: Vec<Value> = sequences
        .iter()
        .map(|s| json!({"cols": 4, "rows": 1, "writes": [format!("{s}X")]}))
        .collect();
    let driver = format!(
        r#"
import {{ Terminal }} from "./terminal.js";
import {{ utf8 }} from "./bytes.js";
const cases = {};
export async function main() {{
  return cases.map((c) => {{
    const t = new Terminal(c.cols, c.rows);
    t.write(utf8(c.writes[0]));
    return t.cellAt(0, 0);
  }});
}}"#,
        json!(cases)
    );
    Ui::start()
        .run(&driver)
        .as_array()
        .expect("one cell per case")
        .clone()
}

fn style(fg: i64, bg: i64, flags: &str) -> Value {
    json!({"glyph": "X", "fg": fg, "bg": bg, "bold": flags.contains('b'), "dim": flags.contains('d'),
           "italic": flags.contains('i'), "underline": flags.contains('u'),
           "inverse": flags.contains('v')})
}

/// Proves: SGR sets and clears each attribute (bold, dim, italic, underline, inverse), the 16
/// colours (30-37, 90-97 and 40-47, 100-107), the default colours (39, 49), reset (0, an empty
/// list), and maps a 256-colour (38;5;n, 48;5;n) or truecolor (38;2;r;g;b) choice to the nearest of
/// the 16; a sequence with a missing argument, a colon form or an attribute this terminal does
/// not draw changes nothing else.
///
/// Catches: a bright colour taken as a normal one, a clear (22, 23, 24, 27) that clears the wrong
/// attribute, an extended colour that eats or leaves the arguments of the next one, and a colon
/// sub-parameter read as a separate attribute.
#[test]
fn sgr_sets_attributes_and_colours_and_maps_extended_colours_to_the_nearest_of_16() {
    let table: Vec<(&str, Value)> = vec![
        ("", style(-1, -1, "")),
        ("\x1b[1m", style(-1, -1, "b")),
        ("\x1b[2m", style(-1, -1, "d")),
        ("\x1b[3m", style(-1, -1, "i")),
        ("\x1b[4m", style(-1, -1, "u")),
        ("\x1b[7m", style(-1, -1, "v")),
        ("\x1b[1;2;3;4;7m", style(-1, -1, "bdiuv")),
        ("\x1b[1;2;3;4;7;0m", style(-1, -1, "")),
        ("\x1b[1;3;4m\x1b[m", style(-1, -1, "")),
        ("\x1b[1;2;22m", style(-1, -1, "")),
        ("\x1b[3;23m", style(-1, -1, "")),
        ("\x1b[4;24m", style(-1, -1, "")),
        ("\x1b[7;27m", style(-1, -1, "")),
        ("\x1b[1;3;22m", style(-1, -1, "i")),
        ("\x1b[31m", style(1, -1, "")),
        ("\x1b[37m", style(7, -1, "")),
        ("\x1b[42m", style(-1, 2, "")),
        ("\x1b[47m", style(-1, 7, "")),
        ("\x1b[90m", style(8, -1, "")),
        ("\x1b[97m", style(15, -1, "")),
        ("\x1b[100m", style(-1, 8, "")),
        ("\x1b[107m", style(-1, 15, "")),
        ("\x1b[1;31;42m", style(1, 2, "b")),
        ("\x1b[31;42m\x1b[39m", style(-1, 2, "")),
        ("\x1b[31;42m\x1b[49m", style(1, -1, "")),
        ("\x1b[31;42m\x1b[0m", style(-1, -1, "")),
        ("\x1b[31m\x1b[34m", style(4, -1, "")),
        ("\x1b[38;5;9m", style(9, -1, "")),
        ("\x1b[38;5;196m", style(9, -1, "")),
        ("\x1b[38;5;21m", style(4, -1, "")),
        ("\x1b[38;5;244m", style(8, -1, "")),
        ("\x1b[38;5;16m", style(0, -1, "")),
        ("\x1b[38;5;231m", style(15, -1, "")),
        ("\x1b[48;5;46m", style(-1, 10, "")),
        ("\x1b[38;2;250;10;5m", style(9, -1, "")),
        ("\x1b[48;2;10;10;10m", style(-1, 0, "")),
        ("\x1b[38;2;0;0;255;1m", style(4, -1, "b")),
        ("\x1b[38;5;1;2m", style(1, -1, "d")),
        ("\x1b[38;5;300m", style(-1, -1, "")),
        ("\x1b[38;5m", style(-1, -1, "")),
        ("\x1b[38;5;m", style(0, -1, "")),
        ("\x1b[38m", style(-1, -1, "")),
        ("\x1b[38;9;1m", style(-1, -1, "b")),
        ("\x1b[38;2;1;2m", style(-1, -1, "")),
        ("\x1b[4:3m", style(-1, -1, "")),
        ("\x1b[38:5:1m", style(-1, -1, "")),
        ("\x1b[5;8;9;53m", style(-1, -1, "")),
        ("\x1b[;1m", style(-1, -1, "b")),
        ("\x1b[1;;31m", style(1, -1, "")),
        ("\x1b[99999999m", style(-1, -1, "")),
    ];
    let sequences: Vec<&str> = table.iter().map(|(s, _)| *s).collect();
    let got = style_after(&sequences);
    for ((sequence, want), got) in table.iter().zip(&got) {
        assert_eq!(got, want, "after {sequence:?}");
    }
}

/// Proves: the alternate screen (`?1049`, `?47`, `?1047`) is a fresh blank screen that the host
/// draws on, and leaving it brings back the main screen as it was; `?1049` also restores the
/// cursor of before, `?47` leaves it where the alternate screen put it, entering twice or leaving
/// without having entered changes nothing, and the scroll region of the main screen survives.
///
/// Catches: alternate-screen output left on the main screen after a full-screen program ends (a
/// pager, an editor), a main screen that is cleared on the way back, and a cursor not restored.
#[test]
fn the_alternate_screen_is_blank_and_the_main_screen_comes_back_as_it_was() {
    check(vec![
        case(
            "1049 in and out",
            (6, 3),
            &["main\x1b[?1049h"],
            &["", "", ""],
            (4, 0),
        ),
        case(
            "1049 draws on its own screen",
            (6, 3),
            &["main\x1b[?1049h\x1b[2;2Halt"],
            &["", " alt", ""],
            (4, 1),
        ),
        case(
            "1049 out restores the screen and the cursor",
            (6, 3),
            &["main\x1b[?1049h\x1b[2;2Halt\x1b[?1049l"],
            &["main", "", ""],
            (4, 0),
        ),
        case(
            "47 out leaves the cursor",
            (6, 3),
            &["main\x1b[?47h\x1b[2;2Halt\x1b[?47l"],
            &["main", "", ""],
            (4, 1),
        ),
        case(
            "1047 in and out",
            (6, 3),
            &["main\x1b[?1047hxyz\x1b[?1047l!"],
            &["main", " !", ""],
            (2, 1),
        ),
        case(
            "in twice is one",
            (6, 3),
            &["a\x1b[?1049hb\x1b[?1049hc\x1b[?1049l"],
            &["a", "", ""],
            (1, 0),
        ),
        case(
            "out without in does nothing",
            (6, 3),
            &["ab\x1b[?1049l"],
            &["ab", "", ""],
            (2, 0),
        ),
        case(
            "the region of the main screen survives",
            (3, 5),
            &["1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[?1049h\x1b[?1049l\x1b[4;1H\n"],
            &["1", "3", "4", "", "5"],
            (0, 3),
        ),
        case(
            "the alternate screen has no region",
            (3, 5),
            &["\x1b[2;4r\x1b[?1049h\x1b[5;1Hz\n"],
            &["", "", "", "z", ""],
            (1, 4),
        ),
    ]);
}

/// Proves: DEC private modes ?25 (cursor visible), ?1 (application cursor keys), ?7 (autowrap) and
/// ?2004 (bracketed paste) are state the page reads, each set by `h` and cleared by `l`, several
/// in one sequence, and a mode this terminal does not know, or a mode of a sequence that is not
/// private, is ignored.
///
/// Catches: a mode that is set but not cleared, a list of modes of which only the first is read,
/// and an unknown mode that disturbs a known one.
#[test]
fn private_modes_are_state_and_unknown_modes_are_ignored() {
    let got = run(json!([
        {"cols": 5, "rows": 2, "writes": []},
        {"cols": 5, "rows": 2, "writes": ["\x1b[?25l\x1b[?1h\x1b[?2004h"]},
        {"cols": 5, "rows": 2, "writes": ["\x1b[?25l\x1b[?1h\x1b[?2004h\x1b[?25h\x1b[?1l\x1b[?2004l"]},
        {"cols": 5, "rows": 2, "writes": ["\x1b[?25;1;2004l\x1b[?1;2004h"]},
        {"cols": 5, "rows": 2, "writes": ["\x1b[?9999h\x1b[?1000;1006h\x1b[4h\x1b[20h\x1b[?25z"]},
    ]));
    let modes = |i: usize| {
        (
            got[i]["cursorVisible"].as_bool(),
            got[i]["appCursor"].as_bool(),
            got[i]["bracketedPaste"].as_bool(),
        )
    };
    assert_eq!(modes(0), (Some(true), Some(false), Some(false)));
    assert_eq!(modes(1), (Some(false), Some(true), Some(true)));
    assert_eq!(modes(2), (Some(true), Some(false), Some(false)));
    assert_eq!(modes(3), (Some(false), Some(true), Some(true)));
    assert_eq!(modes(4), (Some(true), Some(false), Some(false)));
    assert_eq!(got[4]["lines"], json!(["", ""]));
}

fn raw(bytes: &[u8]) -> Value {
    json!(bytes)
}

/// Proves: UTF-8 is decoded across the boundaries of writes (a 2-, 3- and 4-byte character cut at
/// every byte), and a broken sequence (a stray continuation byte, a cut sequence, an overlong
/// form, a surrogate, a byte that is never UTF-8, a code point past U+10FFFF) is one U+FFFD with the
/// byte after it read again.
///
/// Catches: a character cut by a message boundary drawn as two replacement marks, and a broken
/// sequence that swallows the ASCII after it.
#[test]
fn utf8_is_decoded_across_writes_and_broken_sequences_become_one_replacement() {
    let cases = vec![
        ("2 bytes cut", vec![raw(&[0xc3]), raw(&[0xa9])], "\u{e9}"),
        (
            "3 bytes cut",
            vec![raw(&[0xe2]), raw(&[0x82]), raw(&[0xac])],
            "\u{20ac}",
        ),
        (
            "3 bytes cut once",
            vec![raw(&[0xe2, 0x82]), raw(&[0xac, b'a'])],
            "\u{20ac}a",
        ),
        (
            "4 bytes cut",
            vec![raw(&[0xf0]), raw(&[0x9f]), raw(&[0x98]), raw(&[0x80])],
            "\u{1f600}",
        ),
        (
            "text then a cut then text",
            vec![
                json!("a\u{e9}"),
                raw(&[0xe2]),
                raw(&[0x82, 0xac]),
                json!("b"),
            ],
            "a\u{e9}\u{20ac}b",
        ),
        ("stray continuation", vec![raw(&[0x80, b'a'])], "\u{fffd}a"),
        (
            "cut then ascii",
            vec![raw(&[0xe2, 0x82, b'A'])],
            "\u{fffd}A",
        ),
        (
            "cut then another lead",
            vec![raw(&[0xe2, 0xc3, 0xa9])],
            "\u{fffd}\u{e9}",
        ),
        (
            "overlong 2 bytes",
            vec![raw(&[0xc0, 0x80])],
            "\u{fffd}\u{fffd}",
        ),
        (
            "overlong 3 bytes",
            vec![raw(&[0xe0, 0x80, 0x80])],
            "\u{fffd}",
        ),
        ("a surrogate", vec![raw(&[0xed, 0xa0, 0x80])], "\u{fffd}"),
        (
            "past the last code point",
            vec![raw(&[0xf4, 0x90, 0x80, 0x80])],
            "\u{fffd}",
        ),
        (
            "a byte never in UTF-8",
            vec![raw(&[0xff, 0xf8, b'z'])],
            "\u{fffd}\u{fffd}z",
        ),
    ];
    let input: Vec<Value> = cases
        .iter()
        .map(|(_, writes, _)| json!({"cols": 10, "rows": 1, "writes": writes}))
        .collect();
    let got = run(json!(input));
    for ((name, _, want), got) in cases.iter().zip(&got) {
        assert_eq!(got["lines"], json!([want]), "{name}");
    }
}

/// Proves: a wide character takes two columns (its second cell is empty and prints nothing), wraps
/// as a whole when only one column is left, and stays at the last column when autowrap is off; a
/// combining mark joins the character before it and moves nothing.
///
/// Catches: a wide character that shifts the rest of its row left of where a monospaced font puts
/// it, and a combining accent that takes a column of its own.
#[test]
fn wide_characters_take_two_columns_and_combining_marks_none() {
    check(vec![
        case(
            "wide in a row",
            (10, 2),
            &["a\u{4e2d}b"],
            &["a\u{4e2d}b", ""],
            (4, 0),
        ),
        case(
            "emoji is wide",
            (10, 2),
            &["\u{1f600}x"],
            &["\u{1f600}x", ""],
            (3, 0),
        ),
        case(
            "wraps as a whole",
            (5, 2),
            &["abcd\u{4e2d}"],
            &["abcd", "\u{4e2d}"],
            (2, 1),
        ),
        case(
            "fills the row",
            (5, 2),
            &["abc\u{4e2d}"],
            &["abc\u{4e2d}", ""],
            (4, 0),
        ),
        case(
            "off at the last column",
            (5, 2),
            &["\x1b[?7labcd\u{4e2d}"],
            &["abcd", ""],
            (4, 0),
        ),
        case(
            "combining joins",
            (5, 2),
            &["e\u{301}x"],
            &["e\u{301}x", ""],
            (2, 0),
        ),
        case(
            "combining after a full row joins the last cell",
            (3, 2),
            &["abc\u{301}"],
            &["abc\u{301}", ""],
            (2, 0),
        ),
        case(
            "a zero width space joins",
            (5, 2),
            &["a\u{200b}b"],
            &["a\u{200b}b", ""],
            (2, 0),
        ),
        case(
            "combining with nothing before it is lost",
            (5, 2),
            &["\u{301}a"],
            &["a", ""],
            (1, 0),
        ),
    ]);
}

/// Proves: operating system commands (ended by BEL or ST), device control, application, privacy
/// and start-of-string strings (ended by ST), character set designations, an unknown final
/// character, a sequence with an intermediate, a private marker other than `?`, and a sequence too
/// long to be one are all taken and ignored, with the text after each printed; CAN and SUB cancel a
/// sequence; a control inside a sequence is run; ESC inside a sequence starts the next one.
///
/// Catches: the title of a window printed into the screen, an unterminated sequence that leaves the
/// terminal stuck, and a sequence the terminal does not know that eats the text after it.
#[test]
fn sequences_the_terminal_does_not_use_are_ignored_and_the_text_after_them_is_kept() {
    let long = format!("\x1b[{}Hz", "1;".repeat(100));
    check(vec![
        case(
            "osc ended by bel",
            (20, 2),
            &["a\x1b]0;a title\x07b"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "osc ended by st",
            (20, 2),
            &["a\x1b]2;a title\x1b\\b"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "osc cut by an escape sequence",
            (20, 2),
            &["a\x1b]0;t\x1b[1;1Hb"],
            &["b", ""],
            (1, 0),
        ),
        case(
            "dcs ended by st",
            (20, 2),
            &["a\x1bPq#0;2;0;0;0\x1b\\b"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "apc pm sos",
            (20, 2),
            &["a\x1b_x\x1b\\\x1b^y\x1b\\\x1bXz\x1b\\b"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "a string is not ended by bel",
            (20, 2),
            &["a\x1bPx\x07y\x1b\\b"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "charset designations",
            (20, 2),
            &["a\x1b(0\x1b)B\x1b*B\x1b+Bb"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "esc # and esc %",
            (20, 2),
            &["a\x1b#8\x1b%Gb"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "keypad modes and tab set",
            (20, 2),
            &["a\x1b=\x1b>\x1bHb"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "an unknown final",
            (20, 2),
            &["a\x1b[5zb\x1b[?7zc"],
            &["abc", ""],
            (3, 0),
        ),
        case(
            "device attributes and reports",
            (20, 2),
            &["a\x1b[c\x1b[>c\x1b[6n\x1b[0qb"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "an intermediate",
            (20, 2),
            &["a\x1b[ q\x1b[!pb\x1b[2;3 xc"],
            &["abc", ""],
            (3, 0),
        ),
        case(
            "another private marker",
            (20, 2),
            &["a\x1b[>1;2m\x1b[=5ub"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "a sequence too long",
            (20, 2),
            &[long.as_str()],
            &["z", ""],
            (1, 0),
        ),
        case(
            "can cancels",
            (20, 2),
            &["a\x1b[3\x18b\x1b]t\x18c"],
            &["abc", ""],
            (3, 0),
        ),
        case(
            "sub cancels",
            (20, 2),
            &["a\x1b[3\x1ab"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "a control inside a sequence runs",
            (20, 3),
            &["ab\x1b[\n1Dc"],
            &["ab", " c", ""],
            (2, 1),
        ),
        case(
            "esc inside a sequence",
            (20, 2),
            &["a\x1b[3\x1b[1;1Hb"],
            &["b", ""],
            (1, 0),
        ),
        case("esc esc", (20, 2), &["a\x1b\x1b[1;1Hb"], &["b", ""], (1, 0)),
        case(
            "esc then a control",
            (20, 3),
            &["ab\x1b\nc"],
            &["ab", "  c", ""],
            (3, 1),
        ),
        case(
            "a character that is not a sequence's",
            (20, 2),
            &["a\x1b[1\u{e9}b"],
            &["ab", ""],
            (2, 0),
        ),
        case(
            "c1 controls are ignored",
            (20, 2),
            &["a\u{9b}1;1H\u{85}b"],
            &["a1;1Hb", ""],
            (6, 0),
        ),
    ]);
}

/// Proves: the view the page draws is the scrollback then the screen, with the cursor's row
/// counted in them, no scrollback on the alternate screen, and no cursor when it is hidden.
///
/// Catches: a cursor drawn on the wrong row once lines have scrolled off, and a hidden cursor
/// still drawn.
#[test]
fn the_view_is_the_scrollback_then_the_screen_with_the_cursor_in_them() {
    let driver = r#"
import { Terminal } from "./terminal.js";
import { utf8 } from "./bytes.js";
export async function main() {
  const t = new Terminal(4, 2);
  const at = (view) => ({ rows: view.lines.length, cursor: view.cursor });
  const out = [at(t.view())];
  t.write(utf8("a\r\nb\r\nc"));
  out.push(at(t.view()));
  t.write(utf8("\x1b[?25l"));
  out.push(at(t.view()));
  t.write(utf8("\x1b[?25h\x1b[?1049h"));
  out.push(at(t.view()));
  return out;
}"#;
    let got = Ui::start().run(driver);
    assert_eq!(
        got,
        json!([
            {"rows": 2, "cursor": {"row": 0, "col": 0}},
            {"rows": 3, "cursor": {"row": 2, "col": 1}},
            {"rows": 3, "cursor": null},
            {"rows": 2, "cursor": {"row": 1, "col": 1}},
        ])
    );
}
