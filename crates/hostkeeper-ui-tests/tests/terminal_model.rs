//! The terminal's model (`ui/terminal.js`), the screen and the cursor: the bytes of a host's output
//! in, the rows' text and the cursor out, for text and the controls, wrapping, scrolling, cursor
//! movement, erasing, the scroll region and saved cursors. Each case writes to a terminal of a given
//! size and reads back every row's text and the cursor.

mod support;

use serde_json::json;
use support::Ui;
use support::screen::{case, check, run};

/// Proves: text lands where the cursor is, and the controls move it as a terminal's do: CR to the
/// column 0, LF down a row keeping the column, BS left (never past column 0), HT to the next
/// multiple of 8 (never past the last column), BEL and other controls nothing.
///
/// Catches: a line feed that also returns the carriage, a backspace that erases, a tab that stops
/// at the wrong column or runs off the row.
#[test]
fn text_and_the_controls_cr_lf_bs_ht_bel_move_the_cursor_as_a_terminal_does() {
    check(vec![
        case(
            "two lines",
            (10, 4),
            &["ab\r\ncd"],
            &["ab", "cd", "", ""],
            (2, 1),
        ),
        case(
            "lf keeps the column",
            (10, 4),
            &["ab\nc"],
            &["ab", "  c", "", ""],
            (3, 1),
        ),
        case(
            "vt and ff are line feeds",
            (10, 4),
            &["a\x0bb\x0cc"],
            &["a", " b", "  c", ""],
            (3, 2),
        ),
        case("cr overwrites", (10, 2), &["abc\rX"], &["Xbc", ""], (1, 0)),
        case(
            "bs moves left and overwrites",
            (10, 2),
            &["abc\x08d"],
            &["abd", ""],
            (3, 0),
        ),
        case(
            "bs at column 0 stays",
            (10, 2),
            &["\x08\x08a"],
            &["a", ""],
            (1, 0),
        ),
        case(
            "tab to the next stop",
            (20, 2),
            &["a\tb\t\tc"],
            &["a       b          c", ""],
            (19, 0),
        ),
        case(
            "tab stops at the last column",
            (10, 2),
            &["a\t\t\tb"],
            &["a        b", ""],
            (9, 0),
        ),
        case(
            "bel and shift controls do nothing",
            (10, 2),
            &["a\x07\x0e\x0fb\x7f"],
            &["ab", ""],
            (2, 0),
        ),
    ]);
}

/// Proves: a character written in the last column leaves the cursor there until the next
/// printable one, which wraps to the next row; with autowrap off (`?7l`) the last column is
/// overwritten instead, and `?7h` turns wrapping back on.
///
/// Catches: a wrap done as the last column is filled (a line of exactly the width would gain a
/// blank row), and an autowrap switch that does nothing.
#[test]
fn autowrap_waits_for_the_next_character_and_can_be_switched_off() {
    check(vec![
        case(
            "fills the row",
            (5, 3),
            &["abcde"],
            &["abcde", "", ""],
            (4, 0),
        ),
        case(
            "wraps on the next",
            (5, 3),
            &["abcdef"],
            &["abcde", "f", ""],
            (1, 1),
        ),
        case(
            "wraps twice",
            (5, 3),
            &["abcdefghijk"],
            &["abcde", "fghij", "k"],
            (1, 2),
        ),
        case(
            "cr after a full row stays on it",
            (5, 3),
            &["abcde\rX"],
            &["Xbcde", "", ""],
            (1, 0),
        ),
        case(
            "off overwrites the last column",
            (5, 3),
            &["\x1b[?7labcdefg"],
            &["abcdg", "", ""],
            (4, 0),
        ),
        case(
            "on again",
            (5, 3),
            &["\x1b[?7l\x1b[?7habcdef"],
            &["abcde", "f", ""],
            (1, 1),
        ),
    ]);
    let got = run(json!([{"cols": 5, "rows": 2, "writes": ["\x1b[?7l"]}]));
    assert_eq!(got[0]["autowrap"], json!(false));
}

/// Proves: when the cursor is on the last row a line feed scrolls the screen up, the row that left
/// the top is kept as history (at most 200 rows), and the history is not kept on the alternate
/// screen.
///
/// Catches: output past the last row that overwrites it, history that grows without bound, and
/// the alternate screen's rows leaking into the history.
#[test]
fn a_line_feed_on_the_last_row_scrolls_and_keeps_the_history() {
    let mut many = String::new();
    for n in 0..250 {
        many.push_str(&format!("{n}\r\n"));
    }
    let got = run(json!([
        {"cols": 5, "rows": 3, "writes": ["1\r\n2\r\n3\r\n4"]},
        {"cols": 5, "rows": 2, "writes": [many]},
        {"cols": 5, "rows": 2, "writes": ["a\r\nb\x1b[?1049h1\r\n2\r\n3\r\n4"]},
    ]));
    assert_eq!(got[0]["lines"], json!(["2", "3", "4"]));
    assert_eq!(got[0]["scrollback"], json!(["1"]));
    let history = got[1]["scrollback"].as_array().expect("history");
    assert_eq!(history.len(), 200);
    assert_eq!(history[0], json!("49"));
    assert_eq!(history[199], json!("248"));
    assert_eq!(got[1]["lines"], json!(["249", ""]));
    assert_eq!(got[2]["scrollback"], json!([]));
    assert_eq!(got[2]["lines"], json!(["3", "4"]));
}

/// Proves: the cursor movement sequences A B C D E F G H f d move as they say, a missing or zero
/// count is 1, and every move stops at the edges of the screen.
///
/// Catches: a zero count that does not move, a move past the edge, a row or column taken as
/// 0-based, and H with one parameter reading the column from nothing.
#[test]
fn cursor_movement_sequences_move_by_their_counts_and_stop_at_the_edges() {
    check(vec![
        case(
            "H row and column",
            (10, 5),
            &["\x1b[3;4H"],
            &["", "", "", "", ""],
            (3, 2),
        ),
        case(
            "H alone is home",
            (10, 5),
            &["abc\x1b[H"],
            &["abc", "", "", "", ""],
            (0, 0),
        ),
        case(
            "H with a row only",
            (10, 5),
            &["\x1b[4H"],
            &["", "", "", "", ""],
            (0, 3),
        ),
        case(
            "f is H",
            (10, 5),
            &["\x1b[2;2f"],
            &["", "", "", "", ""],
            (1, 1),
        ),
        case(
            "H clamps",
            (10, 5),
            &["\x1b[99;99H"],
            &["", "", "", "", ""],
            (9, 4),
        ),
        case(
            "B and C",
            (10, 5),
            &["\x1b[2B\x1b[3C"],
            &["", "", "", "", ""],
            (3, 2),
        ),
        case(
            "A and D",
            (10, 5),
            &["\x1b[4;5H\x1b[2A\x1b[3D"],
            &["", "", "", "", ""],
            (1, 1),
        ),
        case(
            "no count and zero are one",
            (10, 5),
            &["\x1b[2;2H\x1b[A\x1b[0D\x1b[B\x1b[C"],
            &["", "", "", "", ""],
            (1, 1),
        ),
        case(
            "A and D clamp",
            (10, 5),
            &["\x1b[2;2H\x1b[9A\x1b[9D"],
            &["", "", "", "", ""],
            (0, 0),
        ),
        case(
            "B and C clamp",
            (10, 5),
            &["\x1b[9B\x1b[99C"],
            &["", "", "", "", ""],
            (9, 4),
        ),
        case(
            "E is down to column 0",
            (10, 5),
            &["ab\x1b[2E"],
            &["ab", "", "", "", ""],
            (0, 2),
        ),
        case(
            "F is up to column 0",
            (10, 5),
            &["\x1b[4;5H\x1b[2F"],
            &["", "", "", "", ""],
            (0, 1),
        ),
        case(
            "G sets the column",
            (10, 5),
            &["\x1b[2;2H\x1b[7G"],
            &["", "", "", "", ""],
            (6, 1),
        ),
        case(
            "G clamps",
            (10, 5),
            &["\x1b[99G"],
            &["", "", "", "", ""],
            (9, 0),
        ),
        case(
            "d sets the row",
            (10, 5),
            &["\x1b[3;3H\x1b[5d"],
            &["", "", "", "", ""],
            (2, 4),
        ),
        case(
            "a move ends a pending wrap",
            (5, 3),
            &["abcde\x1b[D\x1b[CX"],
            &["abcdX", "", ""],
            (4, 0),
        ),
    ]);
}

/// Proves: erase in display (J 0, 1, 2, 3), erase in line (K 0, 1, 2), erase characters (X), delete
/// characters (P) and insert characters (@) change exactly the cells they name, and an erased
/// cell takes the background of the current style.
///
/// Catches: an erase one cell short or long, P and @ that do not shift the rest of the row, and an
/// erase that forgets the background colour.
#[test]
fn erase_delete_and_insert_change_exactly_the_cells_they_name() {
    let fill = "abcde\r\nfghij\r\nklmno";
    check(vec![
        case(
            "K 0",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[K"],
            &["abcde", "fg", "klmno"],
            (2, 1),
        ),
        case(
            "K 1",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[1K"],
            &["abcde", "   ij", "klmno"],
            (2, 1),
        ),
        case(
            "K 2",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[2K"],
            &["abcde", "", "klmno"],
            (2, 1),
        ),
        case(
            "J 0",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[J"],
            &["abcde", "fg", ""],
            (2, 1),
        ),
        case(
            "J 1",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[1J"],
            &["", "   ij", "klmno"],
            (2, 1),
        ),
        case(
            "J 2",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[2J"],
            &["", "", ""],
            (2, 1),
        ),
        case("J 3", (5, 3), &[fill, "\x1b[3J"], &["", "", ""], (4, 2)),
        case(
            "a mode of K or J that is not one",
            (5, 3),
            &[fill, "\x1b[2;3H\x1b[7K\x1b[7J"],
            &["abcde", "fghij", "klmno"],
            (2, 1),
        ),
        case(
            "X",
            (5, 3),
            &[fill, "\x1b[2;2H\x1b[2X"],
            &["abcde", "f  ij", "klmno"],
            (1, 1),
        ),
        case(
            "X clamps",
            (5, 3),
            &[fill, "\x1b[2;4H\x1b[9X"],
            &["abcde", "fgh", "klmno"],
            (3, 1),
        ),
        case(
            "P",
            (5, 3),
            &[fill, "\x1b[2;2H\x1b[2P"],
            &["abcde", "fij", "klmno"],
            (1, 1),
        ),
        case(
            "P clamps",
            (5, 3),
            &[fill, "\x1b[2;4H\x1b[9P"],
            &["abcde", "fgh", "klmno"],
            (3, 1),
        ),
        case(
            "@",
            (5, 3),
            &[fill, "\x1b[2;2H\x1b[2@"],
            &["abcde", "f  gh", "klmno"],
            (1, 1),
        ),
        case(
            "@ clamps",
            (5, 3),
            &[fill, "\x1b[2;4H\x1b[9@"],
            &["abcde", "fgh", "klmno"],
            (3, 1),
        ),
    ]);
    let got = run(json!([
        {"cols": 5, "rows": 2, "writes": ["abc\x1b[44m\x1b[2K"]},
    ]));
    assert_eq!(got[0]["lines"], json!(["", ""]));
}

/// Proves: erased and scrolled-in cells carry the background colour that was set when they were
/// made, and none of the other attributes.
///
/// Catches: a cleared line that stays the default colour under a coloured background, or that
/// keeps the bold and underline of the pen.
#[test]
fn erased_cells_take_the_current_background_and_no_other_attribute() {
    let driver = r#"
import { Terminal } from "./terminal.js";
import { utf8 } from "./bytes.js";
export async function main() {
  const t = new Terminal(4, 2);
  t.write(utf8("\x1b[1;4;31;44m\x1b[2K"));
  return [t.cellAt(0, 0), t.cellAt(0, 3), t.cellAt(1, 0)];
}"#;
    let out = Ui::start().run(driver);
    let blue = json!({"glyph": " ", "fg": -1, "bg": 4, "bold": false, "dim": false, "italic": false,
                      "underline": false, "inverse": false});
    let plain = json!({"glyph": " ", "fg": -1, "bg": -1, "bold": false, "dim": false, "italic": false,
                       "underline": false, "inverse": false});
    assert_eq!(out, json!([blue, blue, plain]));
}

/// Proves: a scroll region (`CSI t;b r`) confines line feeds, reverse index, insert line (L),
/// delete line (M) and scroll up and down (S, T) to its rows, leaves the rows outside it alone,
/// keeps nothing in the history, homes the cursor when it is set, and is refused when it is not
/// at least two rows or when the bottom is above the top.
///
/// Catches: a region that scrolls the whole screen, lines that scroll into the history from inside
/// a region, and a one-row region taken.
#[test]
fn a_scroll_region_confines_scrolling_and_line_edits_to_its_rows() {
    let fill = "1\r\n2\r\n3\r\n4\r\n5";
    check(vec![
        case(
            "lf at the bottom of the region",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[4;1H\n"],
            &["1", "3", "4", "", "5"],
            (0, 3),
        ),
        case(
            "setting it homes the cursor",
            (3, 5),
            &[fill, "\x1b[2;4r"],
            &["1", "2", "3", "4", "5"],
            (0, 0),
        ),
        case(
            "ri at the top of the region",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[2;1H\x1bM"],
            &["1", "", "2", "3", "5"],
            (0, 1),
        ),
        case(
            "ri below the top of the region moves up",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[4;1H\x1bM"],
            &["1", "2", "3", "4", "5"],
            (0, 2),
        ),
        case(
            "ri at the top of the screen outside",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[1;1H\x1bM"],
            &["1", "2", "3", "4", "5"],
            (0, 0),
        ),
        case(
            "L inside",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[3;1H\x1b[L"],
            &["1", "2", "", "3", "5"],
            (0, 2),
        ),
        case(
            "M inside",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[3;1H\x1b[M"],
            &["1", "2", "4", "", "5"],
            (0, 2),
        ),
        case(
            "L outside does nothing",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[5;1H\x1b[L"],
            &["1", "2", "3", "4", "5"],
            (0, 4),
        ),
        case(
            "M outside does nothing",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[1;1H\x1b[M"],
            &["1", "2", "3", "4", "5"],
            (0, 0),
        ),
        case(
            "L with a count past the region",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[3;1H\x1b[9L"],
            &["1", "2", "", "", "5"],
            (0, 2),
        ),
        case(
            "S scrolls the region up",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[S"],
            &["1", "3", "4", "", "5"],
            (0, 0),
        ),
        case(
            "T scrolls the region down",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[T"],
            &["1", "", "2", "3", "5"],
            (0, 0),
        ),
        case(
            "S with no region",
            (3, 3),
            &["1\r\n2\r\n3\x1b[2S"],
            &["3", "", ""],
            (1, 2),
        ),
        case(
            "down and up stop at the region",
            (3, 5),
            &["\x1b[2;4r\x1b[2;1H\x1b[9B\x1b[9A"],
            &["", "", "", "", ""],
            (0, 1),
        ),
        case(
            "down from below the region goes on",
            (3, 5),
            &["\x1b[2;3r\x1b[4;1H\x1b[9B"],
            &["", "", "", "", ""],
            (0, 4),
        ),
        case(
            "up from above the region goes on",
            (3, 5),
            &["\x1b[3;4r\x1b[2;1H\x1b[9A"],
            &["", "", "", "", ""],
            (0, 0),
        ),
        case(
            "E and F stop at the region",
            (3, 5),
            &["\x1b[2;4r\x1b[3;1H\x1b[9E\x1b[9F"],
            &["", "", "", "", ""],
            (0, 1),
        ),
        case(
            "no parameters resets it",
            (3, 5),
            &[fill, "\x1b[2;4r\x1b[r\x1b[5;1H\n"],
            &["2", "3", "4", "5", ""],
            (0, 4),
        ),
        case(
            "one row is refused",
            (3, 5),
            &[fill, "\x1b[3;3r\x1b[5;1H\n"],
            &["2", "3", "4", "5", ""],
            (0, 4),
        ),
        case(
            "bottom above top is refused",
            (3, 5),
            &[fill, "\x1b[4;2r\x1b[5;1H\n"],
            &["2", "3", "4", "5", ""],
            (0, 4),
        ),
        case(
            "a bottom past the screen is the last row",
            (3, 5),
            &[fill, "\x1b[2;99r\x1b[5;1H\n"],
            &["1", "3", "4", "5", ""],
            (0, 4),
        ),
        case(
            "a top alone",
            (3, 5),
            &[fill, "\x1b[3r\x1b[5;1H\n"],
            &["1", "2", "4", "5", ""],
            (0, 4),
        ),
    ]);
    let got = run(json!([{"cols": 3, "rows": 5, "writes": [fill, "\x1b[2;4r\x1b[4;1H\n\n\n"]}]));
    assert_eq!(
        got[0]["scrollback"],
        json!([]),
        "a region keeps nothing in the history"
    );
}

/// Proves: ESC 7 and CSI s save the cursor and the style, ESC 8 and CSI u put them back (clamped
/// to the screen), ESC D indexes, ESC M reverses, ESC E is a next line, and ESC c resets
/// everything (the screen, the style, the modes, the region, the alternate screen).
///
/// Catches: a restore that forgets the style, an index that does not scroll at the bottom, and a
/// reset that leaves a mode or the alternate screen behind.
#[test]
fn save_restore_index_next_line_and_reset_do_what_they_name() {
    check(vec![
        case(
            "esc 7 and 8",
            (10, 3),
            &["ab\x1b7\r\ncd\x1b8X"],
            &["abX", "cd", ""],
            (3, 0),
        ),
        case(
            "csi s and u",
            (10, 3),
            &["ab\x1b[s\x1b[3;1Hz\x1b[uY"],
            &["abY", "", "z"],
            (3, 0),
        ),
        case(
            "a restore with nothing saved is home",
            (10, 3),
            &["ab\x1b8c"],
            &["cb", "", ""],
            (1, 0),
        ),
        case("ESC D", (5, 2), &["a\x1bDb"], &["a", " b"], (2, 1)),
        case(
            "ESC D scrolls at the bottom",
            (5, 2),
            &["a\r\nb\x1bDc"],
            &["b", " c"],
            (2, 1),
        ),
        case(
            "ESC M",
            (5, 3),
            &["\x1b[3;1Ha\x1bMb"],
            &["", " b", "a"],
            (2, 1),
        ),
        case("ESC E", (5, 3), &["ab\x1bEc"], &["ab", "c", ""], (1, 1)),
    ]);
    let driver = r#"
import { Terminal } from "./terminal.js";
import { utf8 } from "./bytes.js";
export async function main() {
  const t = new Terminal(10, 3);
  t.write(utf8("\x1b[31m\x1b7\x1b[0m\x1b[2;2H\x1b8X"));
  const restored = t.cellAt(0, 0);
  t.write(utf8("\x1b[?1h\x1b[?25l\x1b[?2004h\x1b[?7l\x1b[2;3r\x1b[?1049hjunk\x1b[1mq\x1bc"));
  const fresh = new Terminal(10, 3);
  const same = JSON.stringify([t.grid, t.scrollback, t.x, t.y, t.pen, t.regionTop, t.bottom, t.alt, t.cursorVisible,
    t.appCursor, t.bracketedPaste, t.autowrap]) === JSON.stringify([fresh.grid, fresh.scrollback, fresh.x,
    fresh.y, fresh.pen, fresh.regionTop, fresh.bottom, fresh.alt, fresh.cursorVisible, fresh.appCursor,
    fresh.bracketedPaste, fresh.autowrap]);
  return { restored, same };
}"#;
    let out = Ui::start().run(driver);
    assert_eq!(
        out["restored"]["fg"],
        json!(1),
        "the saved style comes back"
    );
    assert_eq!(out["same"], json!(true), "ESC c is a new terminal");
}
