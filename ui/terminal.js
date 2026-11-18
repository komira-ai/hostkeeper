// A terminal of the UI's own: a pure model of an xterm-like screen, fed the bytes a host's shell
// writes and holding the grid they draw. It has no DOM and no third-party code; `terminal_render.js`
// draws it and `terminal_keys.js` turns the operator's keys into the bytes a shell reads.
//
// What it understands: UTF-8 text (a character may be split across two writes); the controls CR,
// LF (also VT and FF), BS, HT and BEL; ESC 7 and 8 (save and restore the cursor), ESC c (reset),
// ESC D, M and E (index, reverse index, next line); the CSI sequences A B C D E F G H f J K L M P
// @ X S T d r s u and m (SGR: reset, bold, dim, italic, underline, inverse, the 16 colours, the
// default colours, 256 colours and truecolor); DEC private modes ?1 (application cursor keys),
// ?7 (autowrap), ?25 (cursor visible), ?47, ?1047 and ?1049 (the alternate screen) and ?2004
// (bracketed paste, which is state only here); a scroll region and scrolling. Operating system
// commands and device, privacy, application and start-of-string strings are swallowed up to their
// terminator, and any other sequence is ignored without a trace.
//
// Colours are the 16 of the terminal's palette. A 256-colour or truecolor choice is mapped to the
// nearest of the 16 by its red, green and blue.

export const DEFAULT_COLS = 120;
export const DEFAULT_ROWS = 36;

// The most lines kept above the screen once they scroll off its top.
export const SCROLLBACK = 200;

// A cell's style is one integer: the flags below, then the foreground and the background as a
// colour number plus one (0 is the default colour), each in five bits.
export const BOLD = 1;
export const DIM = 2;
export const ITALIC = 4;
export const UNDERLINE = 8;
export const INVERSE = 16;
const FG_SHIFT = 5;
const BG_SHIFT = 10;
const BG_MASK = 31 << BG_SHIFT;

// The foreground colour number (0 to 15) of a style, or -1 for the default.
export function fgOf(style) {
  return ((style >> FG_SHIFT) & 31) - 1;
}

// The background colour number (0 to 15) of a style, or -1 for the default.
export function bgOf(style) {
  return ((style >> BG_SHIFT) & 31) - 1;
}

// The 16 colours as red, green and blue, which a mapped colour is compared with.
const PALETTE = [
  [0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0], [0, 0, 238], [205, 0, 205], [0, 205, 205], [229, 229, 229],
  [127, 127, 127], [255, 0, 0], [0, 255, 0], [255, 255, 0], [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
];
const CUBE = [0, 95, 135, 175, 215, 255];

// The colour of the 16 nearest to a red, green and blue (the lowest number on a tie).
export function nearest(red, green, blue) {
  let best = 0;
  let bestDistance = Infinity;
  PALETTE.forEach(([r, g, b], number) => {
    const distance = (r - red) ** 2 + (g - green) ** 2 + (b - blue) ** 2;
    if (distance < bestDistance) {
      best = number;
      bestDistance = distance;
    }
  });
  return best;
}

// The colour of the 16 a colour of the 256-colour palette stands for; -1 for a number outside it.
export function fromPalette(index) {
  if (!Number.isInteger(index) || index < 0 || index > 255) {
    return -1;
  }
  if (index < 16) {
    return index;
  }
  if (index < 232) {
    const n = index - 16;
    return nearest(CUBE[Math.floor(n / 36)], CUBE[Math.floor(n / 6) % 6], CUBE[n % 6]);
  }
  const level = 8 + 10 * (index - 232);
  return nearest(level, level, level);
}

// How many columns a code point takes: 0 for a mark that joins the character before it, 2 for a
// wide (East Asian, emoji) character, 1 for any other.
export function widthOf(point) {
  if ((point >= 0x300 && point <= 0x36f) || (point >= 0x200b && point <= 0x200f)
    || (point >= 0xfe00 && point <= 0xfe0f)) {
    return 0;
  }
  if ((point >= 0x1100 && point <= 0x115f) || (point >= 0x2e80 && point <= 0xa4cf)
    || (point >= 0xac00 && point <= 0xd7a3) || (point >= 0xf900 && point <= 0xfaff)
    || (point >= 0xfe30 && point <= 0xfe6f) || (point >= 0xff00 && point <= 0xff60)
    || (point >= 0xffe0 && point <= 0xffe6) || (point >= 0x1f300 && point <= 0x1f64f)
    || (point >= 0x1f900 && point <= 0x1f9ff) || (point >= 0x20000 && point <= 0x3fffd)) {
    return 2;
  }
  return 1;
}

// The parts of a CSI sequence that are kept at most, so that one that never ends is dropped.
const CSI_LIMIT = 64;

// The longest text one cell holds, in UTF-16 units: its character and the combining and zero width
// code points joined to it. The rest of a longer run is dropped, so a host cannot grow one cell
// without end.
const GLYPH_LIMIT = 16;

export class Terminal {
  constructor(cols = DEFAULT_COLS, rows = DEFAULT_ROWS) {
    this.cols = cols;
    this.rows = rows;
    this.reset();
  }

  // Everything as at the start: an empty screen, the cursor at the top left, no style, no mode.
  reset() {
    this.grid = [];
    for (let y = 0; y < this.rows; y++) {
      this.grid.push(this.blankRow(0));
    }
    this.scrollback = [];
    this.x = 0;
    this.y = 0;
    this.pen = 0;
    this.wrapPending = false;
    this.regionTop = 0;
    this.bottom = this.rows - 1;
    this.saved = { x: 0, y: 0, pen: 0 };
    this.stash = null;
    this.cursorVisible = true;
    this.appCursor = false;
    this.bracketedPaste = false;
    this.autowrap = true;
    this.alt = false;
    this.mode = "ground";
    this.csi = "";
    this.need = 0;
    this.accumulated = 0;
    this.least = 0;
  }

  blank(style) {
    return { glyph: " ", st: style & BG_MASK };
  }

  blankRow(style) {
    const row = [];
    for (let x = 0; x < this.cols; x++) {
      row.push(this.blank(style));
    }
    return row;
  }

  // Feeds `bytes` (a Uint8Array) of the host's output to the terminal.
  write(bytes) {
    for (const byte of bytes) {
      this.decode(byte);
    }
  }

  // One byte of UTF-8. A broken sequence is one U+FFFD, and the byte that broke it is read again.
  decode(byte) {
    if (this.need > 0) {
      if ((byte & 0xc0) === 0x80) {
        this.accumulated = (this.accumulated << 6) | (byte & 0x3f);
        this.need -= 1;
        if (this.need === 0) {
          const point = this.accumulated;
          const whole = point >= this.least && point <= 0x10ffff && !(point >= 0xd800 && point <= 0xdfff);
          this.feed(whole ? point : 0xfffd);
        }
        return;
      }
      this.need = 0;
      this.feed(0xfffd);
    }
    if (byte < 0x80) {
      this.feed(byte);
    } else if (byte >= 0xc2 && byte <= 0xdf) {
      this.need = 1;
      this.accumulated = byte & 0x1f;
      this.least = 0x80;
    } else if (byte >= 0xe0 && byte <= 0xef) {
      this.need = 2;
      this.accumulated = byte & 0x0f;
      this.least = 0x800;
    } else if (byte >= 0xf0 && byte <= 0xf4) {
      this.need = 3;
      this.accumulated = byte & 0x07;
      this.least = 0x10000;
    } else {
      this.feed(0xfffd);
    }
  }

  // One code point, through the state of the sequence being read.
  feed(point) {
    if (point === 0x18 || point === 0x1a) {
      this.mode = "ground";
      return;
    }
    switch (this.mode) {
      case "ground":
        this.ground(point);
        break;
      case "esc":
        this.escape(point);
        break;
      case "csi":
        this.collect(point);
        break;
      case "osc":
        if (point === 0x07) {
          this.mode = "ground";
        } else if (point === 0x1b) {
          this.mode = "oscEsc";
        }
        break;
      case "str":
        if (point === 0x1b) {
          this.mode = "oscEsc";
        }
        break;
      case "drop":
        if (point >= 0x40 && point <= 0x7e) {
          this.mode = "ground";
        }
        break;
      case "oscEsc":
        if (point === 0x5c) {
          this.mode = "ground";
        } else {
          this.mode = "esc";
          this.escape(point);
        }
        break;
      default:
        // One more character of ESC ( B and the like, which choose a character set or a mode
        // this terminal has no use for.
        this.mode = "ground";
    }
  }

  // A C0 control. Returns whether `point` was one.
  control(point) {
    switch (point) {
      case 0x0d:
        this.moveTo(0, this.y);
        break;
      case 0x0a:
      case 0x0b:
      case 0x0c:
        this.index();
        break;
      case 0x08:
        this.moveTo(Math.max(this.x - 1, 0), this.y);
        break;
      case 0x09:
        this.moveTo(Math.min((Math.floor(this.x / 8) + 1) * 8, this.cols - 1), this.y);
        break;
      default:
        // BEL, SO, SI and the rest do nothing here.
        break;
    }
  }

  ground(point) {
    if (point === 0x1b) {
      this.mode = "esc";
    } else if (point < 0x20 || (point >= 0x7f && point <= 0x9f)) {
      this.control(point);
    } else {
      this.print(point);
    }
  }

  escape(point) {
    this.mode = "ground";
    switch (String.fromCodePoint(point)) {
      case "[":
        this.mode = "csi";
        this.csi = "";
        break;
      case "]":
        this.mode = "osc";
        break;
      case "P":
      case "X":
      case "^":
      case "_":
        this.mode = "str";
        break;
      case "(": case ")": case "*": case "+": case "#": case "%": case " ":
        this.mode = "skip";
        break;
      case "7":
        this.save();
        break;
      case "8":
        this.restore();
        break;
      case "c":
        this.reset();
        break;
      case "D":
        this.index();
        break;
      case "M":
        this.reverseIndex();
        break;
      case "E":
        this.moveTo(0, this.y);
        this.index();
        break;
      case "\u001b":
        this.mode = "esc";
        break;
      default:
        // ESC =, ESC >, ESC H and the rest do nothing here.
        if (point < 0x20) {
          this.control(point);
        }
    }
  }

  // A character of a CSI sequence: a control runs at once, a parameter, an intermediate or a
  // private marker is kept, and a final character runs the sequence.
  collect(point) {
    if (point === 0x1b) {
      this.mode = "esc";
    } else if (point < 0x20) {
      this.control(point);
    } else if (point >= 0x40 && point <= 0x7e) {
      this.mode = "ground";
      this.dispatch(String.fromCharCode(point), this.csi);
    } else if (point < 0x40 && this.csi.length < CSI_LIMIT) {
      this.csi += String.fromCharCode(point);
    } else {
      // One that never ends is dropped up to its final character; a character that cannot be part
      // of a sequence ends it.
      this.mode = point < 0x40 ? "drop" : "ground";
    }
  }

  // The numbers of `text` (`1;2;;4`); an empty one is `fallback`.
  numbers(text, fallback) {
    return text.split(";").map((part) => {
      const number = /^[0-9]{1,5}$/.test(part) ? Number(part) : NaN;
      return Number.isNaN(number) ? fallback : number;
    });
  }

  dispatch(final, body) {
    const marker = /^[<=>?]/.test(body) ? body[0] : "";
    const rest = body.slice(marker.length);
    if (/[ -/]/.test(rest)) {
      return;
    }
    if (marker === "?") {
      this.privateMode(final, this.numbers(rest, 0));
    } else if (marker === "") {
      this.sequence(final, rest);
    }
  }

  // The sequences without a private marker.
  sequence(final, text) {
    const args = this.numbers(text, 0);
    const second = args[1] === undefined ? 0 : args[1];
    const n = Math.max(args[0], 1);
    switch (final) {
      case "A":
        this.moveTo(this.x, Math.max(this.y - n, this.y >= this.regionTop ? this.regionTop : 0));
        break;
      case "B":
        this.moveTo(this.x, Math.min(this.y + n, this.y <= this.bottom ? this.bottom : this.rows - 1));
        break;
      case "C":
        this.moveTo(Math.min(this.x + n, this.cols - 1), this.y);
        break;
      case "D":
        this.moveTo(Math.max(this.x - n, 0), this.y);
        break;
      case "E":
        this.moveTo(0, Math.min(this.y + n, this.y <= this.bottom ? this.bottom : this.rows - 1));
        break;
      case "F":
        this.moveTo(0, Math.max(this.y - n, this.y >= this.regionTop ? this.regionTop : 0));
        break;
      case "G":
        this.moveTo(Math.min(n - 1, this.cols - 1), this.y);
        break;
      case "H":
      case "f":
        this.moveTo(Math.min(Math.max(second, 1) - 1, this.cols - 1), Math.min(n - 1, this.rows - 1));
        break;
      case "d":
        this.moveTo(this.x, Math.min(n - 1, this.rows - 1));
        break;
      case "J":
        this.eraseDisplay(args[0]);
        break;
      case "K":
        this.eraseLine(args[0]);
        break;
      case "L":
        this.insertLines(n);
        break;
      case "M":
        this.deleteLines(n);
        break;
      case "P":
        this.deleteChars(n);
        break;
      case "@":
        this.insertChars(n);
        break;
      case "X":
        this.eraseChars(n);
        break;
      case "S":
        this.scrollUp(n);
        break;
      case "T":
        this.scrollDown(n);
        break;
      case "r":
        this.setRegion(args[0], second);
        break;
      case "s":
        this.save();
        break;
      case "u":
        this.restore();
        break;
      case "m":
        this.sgr(text);
        break;
      default:
        // Any other final character is ignored.
        break;
    }
  }

  privateMode(final, modes) {
    if (final !== "h" && final !== "l") {
      return;
    }
    const on = final === "h";
    for (const mode of modes) {
      if (mode === 1) {
        this.appCursor = on;
      } else if (mode === 7) {
        this.autowrap = on;
      } else if (mode === 25) {
        this.cursorVisible = on;
      } else if (mode === 2004) {
        this.bracketedPaste = on;
      } else if (mode === 47 || mode === 1047 || mode === 1049) {
        this.screen(on, mode === 1049);
      }
    }
  }

  // Moves to the alternate screen (`on`) or back to the main one. `keep` saves the cursor on the
  // way in and restores it on the way out (mode 1049).
  screen(on, keep) {
    if (on === this.alt) {
      return;
    }
    if (on) {
      if (keep) {
        this.save();
      }
      this.stash = { grid: this.grid, regionTop: this.regionTop, bottom: this.bottom };
      this.grid = [];
      for (let y = 0; y < this.rows; y++) {
        this.grid.push(this.blankRow(0));
      }
      this.regionTop = 0;
      this.bottom = this.rows - 1;
      this.alt = true;
    } else {
      ({ grid: this.grid, regionTop: this.regionTop, bottom: this.bottom } = this.stash);
      this.stash = null;
      this.alt = false;
      if (keep) {
        this.restore();
      }
    }
    this.wrapPending = false;
  }

  moveTo(x, y) {
    this.x = x;
    this.y = y;
    this.wrapPending = false;
  }

  save() {
    this.saved = { x: this.x, y: this.y, pen: this.pen };
  }

  restore() {
    this.pen = this.saved.pen;
    this.moveTo(Math.min(this.saved.x, this.cols - 1), Math.min(this.saved.y, this.rows - 1));
  }

  // A line feed: down a row, or the region scrolls up when the cursor is on its last row.
  index() {
    this.wrapPending = false;
    if (this.y === this.bottom) {
      this.scrollUp(1);
    } else if (this.y < this.rows - 1) {
      this.y += 1;
    }
  }

  reverseIndex() {
    this.wrapPending = false;
    if (this.y === this.regionTop) {
      this.scrollDown(1);
    } else if (this.y > 0) {
      this.y -= 1;
    }
  }

  scrollUp(count) {
    const n = Math.min(count, this.bottom - this.regionTop + 1);
    for (let i = 0; i < n; i++) {
      const gone = this.grid.splice(this.regionTop, 1)[0];
      if (!this.alt && this.regionTop === 0 && this.bottom === this.rows - 1) {
        this.scrollback.push(gone);
        if (this.scrollback.length > SCROLLBACK) {
          this.scrollback.shift();
        }
      }
      this.grid.splice(this.bottom, 0, this.blankRow(this.pen));
    }
  }

  scrollDown(count) {
    const n = Math.min(count, this.bottom - this.regionTop + 1);
    for (let i = 0; i < n; i++) {
      this.grid.splice(this.bottom, 1);
      this.grid.splice(this.regionTop, 0, this.blankRow(this.pen));
    }
  }

  // Sets the scroll region to rows `top` to `bottom` (1-based; 0 is the default) and homes the
  // cursor. A region of fewer than two rows is refused.
  setRegion(top, bottom) {
    const first = Math.max(top, 1) - 1;
    const last = (bottom === 0 ? this.rows : Math.min(bottom, this.rows)) - 1;
    if (first < last) {
      this.regionTop = first;
      this.bottom = last;
      this.moveTo(0, 0);
    }
  }

  insideRegion() {
    return this.y >= this.regionTop && this.y <= this.bottom;
  }

  insertLines(n) {
    if (!this.insideRegion()) {
      return;
    }
    const count = Math.min(n, this.bottom - this.y + 1);
    for (let i = 0; i < count; i++) {
      this.grid.splice(this.bottom, 1);
      this.grid.splice(this.y, 0, this.blankRow(this.pen));
    }
    this.moveTo(0, this.y);
  }

  deleteLines(n) {
    if (!this.insideRegion()) {
      return;
    }
    const count = Math.min(n, this.bottom - this.y + 1);
    for (let i = 0; i < count; i++) {
      this.grid.splice(this.y, 1);
      this.grid.splice(this.bottom, 0, this.blankRow(this.pen));
    }
    this.moveTo(0, this.y);
  }

  insertChars(n) {
    const row = this.grid[this.y];
    const count = Math.min(n, this.cols - this.x);
    row.splice(this.cols - count, count);
    for (let i = 0; i < count; i++) {
      row.splice(this.x, 0, this.blank(this.pen));
    }
    this.wrapPending = false;
  }

  deleteChars(n) {
    const row = this.grid[this.y];
    const count = Math.min(n, this.cols - this.x);
    row.splice(this.x, count);
    for (let i = 0; i < count; i++) {
      row.push(this.blank(this.pen));
    }
    this.wrapPending = false;
  }

  eraseChars(n) {
    const row = this.grid[this.y];
    const end = Math.min(this.x + n, this.cols);
    for (let x = this.x; x < end; x++) {
      row[x] = this.blank(this.pen);
    }
    this.wrapPending = false;
  }

  // ESC [ K: 0 from the cursor to the end of the line, 1 from its start to the cursor, 2 all of it.
  eraseLine(mode) {
    const row = this.grid[this.y];
    if (mode > 2) {
      return;
    }
    const from = mode === 0 ? this.x : 0;
    const to = mode === 1 ? this.x : this.cols - 1;
    for (let x = from; x <= to; x++) {
      row[x] = this.blank(this.pen);
    }
  }

  // ESC [ J: 0 from the cursor to the end of the screen, 1 from the start to the cursor, 2 and 3
  // all of it.
  eraseDisplay(mode) {
    if (mode === 0) {
      this.eraseLine(0);
      for (let y = this.y + 1; y < this.rows; y++) {
        this.grid[y] = this.blankRow(this.pen);
      }
    } else if (mode === 1) {
      this.eraseLine(1);
      for (let y = 0; y < this.y; y++) {
        this.grid[y] = this.blankRow(this.pen);
      }
    } else if (mode === 2 || mode === 3) {
      for (let y = 0; y < this.rows; y++) {
        this.grid[y] = this.blankRow(this.pen);
      }
      if (mode === 3) {
        this.scrollback = [];
      }
    }
  }

  sgr(text) {
    const groups = text === "" ? ["0"] : text.split(";");
    for (let i = 0; i < groups.length; i++) {
      if (groups[i].includes(":")) {
        continue;
      }
      const code = groups[i] === "" ? 0 : Number(groups[i]);
      if (code === 38 || code === 48) {
        i = this.extended(code === 38, groups, i);
      } else {
        this.setAttribute(code);
      }
    }
  }

  // 38 and 48 with their arguments (`5;n` or `2;r;g;b`, at `groups[from + 1...]`); returns the
  // index of the last group read. Arguments that are missing end the sequence.
  extended(foreground, groups, from) {
    const mode = groups[from + 1];
    const take = (count) => groups.slice(from + 2, from + 2 + count).map((g) => (g === "" ? 0 : Number(g)));
    let colour = -1;
    let used = 1;
    if (mode === "5") {
      used = 2;
      const [index] = take(1);
      colour = index === undefined ? -1 : fromPalette(index);
    } else if (mode === "2") {
      used = 4;
      const rgb = take(3);
      colour = rgb.length === 3 && rgb.every((v) => Number.isFinite(v)) ? nearest(rgb[0], rgb[1], rgb[2]) : -1;
    }
    if (colour >= 0) {
      this.setColour(foreground, colour);
    }
    return Math.min(from + used, groups.length - 1);
  }

  setColour(foreground, colour) {
    const shift = foreground ? FG_SHIFT : BG_SHIFT;
    this.pen = (this.pen & ~(31 << shift)) | ((colour + 1) << shift);
  }

  setAttribute(code) {
    if (code === 0) {
      this.pen = 0;
    } else if (code === 1) {
      this.pen |= BOLD;
    } else if (code === 2) {
      this.pen |= DIM;
    } else if (code === 3) {
      this.pen |= ITALIC;
    } else if (code === 4) {
      this.pen |= UNDERLINE;
    } else if (code === 7) {
      this.pen |= INVERSE;
    } else if (code === 22) {
      this.pen &= ~(BOLD | DIM);
    } else if (code === 23) {
      this.pen &= ~ITALIC;
    } else if (code === 24) {
      this.pen &= ~UNDERLINE;
    } else if (code === 27) {
      this.pen &= ~INVERSE;
    } else if (code >= 30 && code <= 37) {
      this.setColour(true, code - 30);
    } else if (code === 39) {
      this.pen &= ~(31 << FG_SHIFT);
    } else if (code >= 40 && code <= 47) {
      this.setColour(false, code - 40);
    } else if (code === 49) {
      this.pen &= ~BG_MASK;
    } else if (code >= 90 && code <= 97) {
      this.setColour(true, code - 90 + 8);
    } else if (code >= 100 && code <= 107) {
      this.setColour(false, code - 100 + 8);
    }
  }

  // Draws a printable code point at the cursor and moves it on, wrapping first when the last
  // column was filled and autowrap is on.
  print(point) {
    const width = widthOf(point);
    if (width === 0) {
      const before = this.grid[this.y][this.wrapPending ? this.x : Math.max(this.x - 1, 0)];
      if (before.glyph.length < GLYPH_LIMIT) {
        before.glyph += String.fromCodePoint(point);
      }
      return;
    }
    if (this.wrapPending || (width === 2 && this.x === this.cols - 1 && this.autowrap)) {
      if (this.autowrap) {
        this.moveTo(0, this.y);
        this.index();
      }
      this.wrapPending = false;
    }
    if (width === 2 && this.x === this.cols - 1) {
      return;
    }
    const row = this.grid[this.y];
    row[this.x] = { glyph: String.fromCodePoint(point), st: this.pen };
    if (width === 2) {
      row[this.x + 1] = { glyph: "", st: this.pen };
    }
    const next = this.x + width;
    if (next >= this.cols) {
      this.x = this.cols - 1;
      this.wrapPending = this.autowrap;
    } else {
      this.x = next;
    }
  }

  // The text of row `y` of the screen, without the blanks at its end.
  lineText(y) {
    return this.grid[y].map((cell) => cell.glyph).join("").replace(/ +$/, "");
  }

  // The cell at row `y`, column `x` as the page shows it: its text and, by name, its style.
  cellAt(y, x) {
    const { glyph, st } = this.grid[y][x];
    return {
      glyph,
      fg: fgOf(st),
      bg: bgOf(st),
      bold: (st & BOLD) !== 0,
      dim: (st & DIM) !== 0,
      italic: (st & ITALIC) !== 0,
      underline: (st & UNDERLINE) !== 0,
      inverse: (st & INVERSE) !== 0,
    };
  }

  // What is drawn: the lines that scrolled off the top (none on the alternate screen), then the
  // screen, and the cursor's place in them, or null when the cursor is hidden.
  view() {
    const lines = this.alt ? this.grid : this.scrollback.concat(this.grid);
    const above = lines.length - this.rows;
    return { lines, cursor: this.cursorVisible ? { row: above + this.y, col: this.x } : null };
  }
}
