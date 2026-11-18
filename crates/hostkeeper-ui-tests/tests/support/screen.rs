//! What the terminal tests share: a terminal of a given size written to, and the screen, the
//! cursor and the modes read back (`ui/terminal.js` in the engine).

use serde_json::{Value, json};

use super::Ui;

/// Runs each case `{cols, rows, writes}` (a write is a text, sent as its UTF-8 bytes, or a list of
/// raw bytes) and returns what the terminal holds after it: `lines`, `x`, `y`, the modes and the
/// history.
pub fn run(cases: Value) -> Vec<Value> {
    let driver = format!(
        r#"
import {{ Terminal }} from "./terminal.js";
import {{ utf8 }} from "./bytes.js";
const cases = {cases};
export async function main() {{
  return cases.map((c) => {{
    const t = new Terminal(c.cols, c.rows);
    for (const w of c.writes) {{
      t.write(typeof w === "string" ? utf8(w) : Uint8Array.from(w));
    }}
    const lines = [];
    for (let y = 0; y < c.rows; y++) {{
      lines.push(t.lineText(y));
    }}
    return {{
      lines, x: t.x, y: t.y, alt: t.alt, cursorVisible: t.cursorVisible, appCursor: t.appCursor,
      bracketedPaste: t.bracketedPaste, autowrap: t.autowrap,
      scrollback: t.scrollback.map((row) => row.map((cell) => cell.glyph).join("").replace(/ +$/, "")),
    }};
  }});
}}"#
    );
    let mut ui = Ui::start();
    let out = ui.run(&driver);
    out.as_array().expect("one result per case").clone()
}

/// One case: its name, size, the writes and the screen it must leave.
pub struct Case {
    pub name: &'static str,
    pub size: (u32, u32),
    pub writes: Vec<Value>,
    pub lines: Vec<&'static str>,
    pub at: (u32, u32),
}

pub fn case(
    name: &'static str,
    size: (u32, u32),
    writes: &[&str],
    lines: &[&'static str],
    at: (u32, u32),
) -> Case {
    Case {
        name,
        size,
        writes: writes.iter().map(|w| json!(w)).collect(),
        lines: lines.to_vec(),
        at,
    }
}

pub fn check(cases: Vec<Case>) {
    let input: Vec<Value> = cases
        .iter()
        .map(|c| json!({"cols": c.size.0, "rows": c.size.1, "writes": c.writes}))
        .collect();
    let results = run(json!(input));
    for (case, got) in cases.iter().zip(&results) {
        assert_eq!(got["lines"], json!(case.lines), "{}: the screen", case.name);
        assert_eq!(
            (got["x"].as_f64(), got["y"].as_f64()),
            (Some(f64::from(case.at.0)), Some(f64::from(case.at.1))),
            "{}: the cursor",
            case.name
        );
    }
}
