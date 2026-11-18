//! The terminal's screen as elements (`ui/terminal_render.js`): rows of runs with class names, no
//! inline style, and every tag and attribute one the page's `mount` accepts.

mod support;

use serde_json::json;
use support::Ui;

/// Proves: the screen draws as one `pre` of `tline` rows, each a list of runs, where a run of the
/// default look is plain text and any other a `span` whose classes name its colours and
/// attributes; inverse video swaps the colours (the default ones become `tfd` and `tbd`); the
/// cursor's cell is a run of its own with `tcursor`; the blanks at the end of a row are left out,
/// unless the cursor is among them; and the scrollback comes first.
///
/// Catches: a colour class that is lost on a wrapped run, inverse video that shows nothing for
/// default colours, a cursor that is not drawn, and a row that carries its trailing blanks.
#[test]
fn the_screen_is_rows_of_runs_with_classes_for_colours_attributes_and_the_cursor() {
    let driver = r#"
import { Terminal } from "./terminal.js";
import { drawTerminal } from "./terminal_render.js";
import { utf8 } from "./bytes.js";
import { nodes, text } from "../test/kit.js";
const describe = (tree) => tree.children.map((row) => row.children.map((run) =>
  (typeof run.text === "string" ? { plain: run.text } : { classes: run.attrs["class"], text: text(run) })));
export async function main() {
  const t = new Terminal(12, 3);
  t.write(utf8("plain \u001b[1;31mred\u001b[0m\r\n\u001b[7mrev\u001b[0m \u001b[7;32;44mgb\u001b[0m\r\n\u001b[2;3;4mx"));
  const first = describe(drawTerminal(t));
  const tree = drawTerminal(t);
  const tags = [...new Set(nodes(tree).map((n) => n.tag))];
  const attributes = [...new Set(nodes(tree).flatMap((n) => Object.keys(n.attrs)))];
  const scrolled = new Terminal(3, 2);
  scrolled.write(utf8("a\r\nb\r\nc\u001b[?25l"));
  const hidden = describe(drawTerminal(scrolled));
  const blank = describe(drawTerminal(new Terminal(3, 2)));
  const alone = new Terminal(3, 1);
  alone.write(utf8("\u001b[44m  "));
  return { first, tags, attributes, hidden, blank, tree: tree.attrs["class"], solid: describe(drawTerminal(alone)) };
}"#;
    let out = Ui::start().run(driver);
    assert_eq!(
        out["first"],
        json!([
            [{"plain": "plain "}, {"classes": "tf1 tbold", "text": "red"}],
            [{"classes": "tfd tbd", "text": "rev"}, {"plain": " "}, {"classes": "tf4 tb2", "text": "gb"}],
            [{"classes": "tdim titalic tunderline", "text": "x"}, {"classes": "tcursor", "text": " "}],
        ])
    );
    assert_eq!(out["tags"], json!(["pre", "div", "span"]));
    assert_eq!(out["attributes"], json!(["class"]), "no style attribute");
    assert_eq!(out["tree"], json!("tscreen"));
    assert_eq!(
        out["hidden"],
        json!([[{"plain": "a"}], [{"plain": "b"}], [{"plain": "c"}]]),
        "the scrollback line, then the screen; no cursor when it is hidden"
    );
    assert_eq!(
        out["blank"],
        json!([[{"classes": "tcursor", "text": " "}], []])
    );
    assert_eq!(
        out["solid"],
        json!([[{"classes": "tb4", "text": "  "}, {"classes": "tcursor", "text": " "}]])
    );
}
