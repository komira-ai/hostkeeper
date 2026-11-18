// The terminal's screen as view elements (`view.js`): one `pre` of rows, each row runs of text in
// a `span` whose class names carry the colours and attributes, so the page's stylesheet
// (`styles/terminal.css`) draws it and no inline style or script is needed.
//
// Classes: `tf0` to `tf15` and `tb0` to `tb15` for the 16 colours of the foreground and the
// background, `tfd` for the default background colour used as a foreground and `tbd` for the
// default foreground colour used as a background (what inverse video of a default colour is),
// `tbold`, `tdim`, `titalic` and `tunderline`, and `tcursor` on the cell the cursor is on.

import { h } from "./view.js";
import { BOLD, DIM, INVERSE, ITALIC, UNDERLINE, bgOf, fgOf } from "./terminal.js";

// The class names of a cell with `style`, shown as the cursor's cell or not.
export function classesOf(style, cursor) {
  let fg = fgOf(style);
  let bg = bgOf(style);
  if ((style & INVERSE) !== 0) {
    [fg, bg] = [bg, fg];
    fg = fg === -1 ? "d" : fg;
    bg = bg === -1 ? "d" : bg;
  }
  const names = [];
  if (fg !== -1) {
    names.push(`tf${fg}`);
  }
  if (bg !== -1) {
    names.push(`tb${bg}`);
  }
  if ((style & BOLD) !== 0) {
    names.push("tbold");
  }
  if ((style & DIM) !== 0) {
    names.push("tdim");
  }
  if ((style & ITALIC) !== 0) {
    names.push("titalic");
  }
  if ((style & UNDERLINE) !== 0) {
    names.push("tunderline");
  }
  if (cursor) {
    names.push("tcursor");
  }
  return names.join(" ");
}

// One row: runs of cells of one look as spans (plain text where the look is the default), the
// blanks at the end left out unless the cursor is among them. `cursorAt` is the cursor's column in
// this row, or -1.
function drawRow(cells, cursorAt) {
  let end = cells.length;
  while (end > 0 && end - 1 !== cursorAt && cells[end - 1].glyph === " " && cells[end - 1].st === 0) {
    end -= 1;
  }
  const runs = [];
  let run = null;
  for (let x = 0; x < end; x++) {
    const { glyph, st } = cells[x];
    const classes = classesOf(st, x === cursorAt);
    if (run !== null && run.classes === classes) {
      run.text += glyph;
    } else {
      run = { classes, text: glyph };
      runs.push(run);
    }
  }
  return h("div", { class: "tline" },
    runs.map((r) => (r.classes === "" ? r.text : h("span", { class: r.classes }, r.text))));
}

// The terminal's screen, its history above it, as the elements of a `pre`.
export function drawTerminal(terminal) {
  const { lines, cursor } = terminal.view();
  return h("pre", { class: "tscreen" }, lines.map((cells, row) => drawRow(cells, cursor !== null && cursor.row === row ? cursor.col : -1)));
}
