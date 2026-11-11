// Bytes for the API's `bytes` fields, which canonical JSON writes as base64: a text as its UTF-8
// bytes, and bytes as base64. The page reads the operator's file or typed text into bytes here, sends
// them, and keeps nothing.

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// The UTF-8 bytes of `text`. A lone surrogate becomes U+FFFD, as a text encoder writes it.
export function utf8(text) {
  const out = [];
  for (const ch of text) {
    let point = ch.codePointAt(0);
    if (point >= 0xd800 && point <= 0xdfff) {
      point = 0xfffd;
    }
    if (point < 0x80) {
      out.push(point);
    } else if (point < 0x800) {
      out.push(0xc0 | (point >> 6), 0x80 | (point & 0x3f));
    } else if (point < 0x10000) {
      out.push(0xe0 | (point >> 12), 0x80 | ((point >> 6) & 0x3f), 0x80 | (point & 0x3f));
    } else {
      out.push(
        0xf0 | (point >> 18),
        0x80 | ((point >> 12) & 0x3f),
        0x80 | ((point >> 6) & 0x3f),
        0x80 | (point & 0x3f),
      );
    }
  }
  return Uint8Array.from(out);
}

// `bytes` (a Uint8Array) as standard base64 with padding.
export function toBase64(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const rest = bytes.length - i;
    const chunk = (bytes[i] << 16) | ((rest > 1 ? bytes[i + 1] : 0) << 8) | (rest > 2 ? bytes[i + 2] : 0);
    out += ALPHABET[(chunk >> 18) & 63] + ALPHABET[(chunk >> 12) & 63];
    out += rest > 1 ? ALPHABET[(chunk >> 6) & 63] : "=";
    out += rest > 2 ? ALPHABET[chunk & 63] : "=";
  }
  return out;
}
