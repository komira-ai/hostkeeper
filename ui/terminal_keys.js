// What the operator types, as the bytes a shell reads: a key event to bytes, and pasted text to
// bytes. Both return a Uint8Array, or null for a key that sends nothing (a modifier alone, a
// browser shortcut, a key this terminal has no code for).

import { utf8 } from "./bytes.js";

// The most bytes a message to the coordinator may carry.
export const MESSAGE_LIMIT = 16384;

const ESC = "\u001b";

// The keys that send a final letter, after `ESC [` (or `ESC O` in application mode).
const LETTERS = {
  ArrowUp: "A", ArrowDown: "B", ArrowRight: "C", ArrowLeft: "D", Home: "H", End: "F",
  F1: "P", F2: "Q", F3: "R", F4: "S",
};
// The keys that send a number and a tilde.
const TILDES = {
  Insert: 2, Delete: 3, PageUp: 5, PageDown: 6,
  F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24,
};
// The keys that send one fixed text.
const FIXED = { Enter: "\r", Backspace: "\u007f", Tab: "\t", Escape: ESC };

// The control character of Ctrl and `key`, or null for a key with none.
function controlOf(key) {
  if (/^[a-zA-Z]$/.test(key)) {
    return String.fromCharCode(key.toUpperCase().charCodeAt(0) - 64);
  }
  const others = { "@": 0, " ": 0, "[": 27, "\\": 28, "]": 29, "^": 30, "6": 30, "_": 31, "/": 31, "?": 127 };
  return Object.prototype.hasOwnProperty.call(others, key) ? String.fromCharCode(others[key]) : null;
}

// The modifier number of a sequence: 1 plus Shift (1), Alt (2) and Ctrl (4).
function modifierOf(event) {
  return 1 + (event.shiftKey ? 1 : 0) + (event.altKey ? 2 : 0) + (event.ctrlKey ? 4 : 0);
}

// The text a key event sends, or null. `appCursor` is the terminal's application cursor keys mode.
export function textOf(event, appCursor) {
  const { key } = event;
  if (event.metaKey || event.isComposing || typeof key !== "string") {
    return null;
  }
  const modifier = modifierOf(event);
  if (Object.prototype.hasOwnProperty.call(LETTERS, key)) {
    if (modifier > 1) {
      return `${ESC}[1;${modifier}${LETTERS[key]}`;
    }
    const fkey = key.startsWith("F");
    return `${ESC}${appCursor || fkey ? "O" : "["}${LETTERS[key]}`;
  }
  if (Object.prototype.hasOwnProperty.call(TILDES, key)) {
    return modifier > 1 ? `${ESC}[${TILDES[key]};${modifier}~` : `${ESC}[${TILDES[key]}~`;
  }
  if (Object.prototype.hasOwnProperty.call(FIXED, key)) {
    if (key === "Tab" && event.shiftKey) {
      return `${ESC}[Z`;
    }
    return event.altKey ? `${ESC}${FIXED[key]}` : FIXED[key];
  }
  if ([...key].length !== 1) {
    return null;
  }
  if (event.ctrlKey && event.altKey) {
    // AltGr, which a keyboard reports as both: the character it makes.
    return key;
  }
  if (event.ctrlKey) {
    // Ctrl+V and Ctrl+Shift+a letter are the browser's (paste, copy and the like), not the shell's.
    const browsers = key.toLowerCase() === "v" || (event.shiftKey && /^[a-zA-Z]$/.test(key));
    return browsers ? null : controlOf(key);
  }
  return event.altKey ? `${ESC}${key}` : key;
}

// The bytes of a key event, or null.
export function encodeKey(event, appCursor) {
  const text = textOf(event, appCursor);
  return text === null ? null : utf8(text);
}

// The bytes of pasted `text`: line breaks as a carriage return, and, when the host asked for
// bracketed paste, between the markers that tell it this is a paste (an escape in the text is
// dropped then, so it cannot end the paste early).
export function encodePaste(text, bracketed) {
  const lines = text.replace(/\r\n|\n/g, "\r");
  return utf8(bracketed ? `${ESC}[200~${lines.replace(/\u001b/g, "")}${ESC}[201~` : lines);
}

// `bytes` in pieces of at most MESSAGE_LIMIT.
export function pieces(bytes) {
  const out = [];
  for (let at = 0; at < bytes.length; at += MESSAGE_LIMIT) {
    out.push(bytes.slice(at, at + MESSAGE_LIMIT));
  }
  return out;
}
