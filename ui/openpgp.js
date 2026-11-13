// The fingerprints of the keys of a binary OpenPGP keyring, read in the page so that the form of a
// package repository does not ask for what the file already says. `Repositories.Put` wants the
// fingerprint of each primary key of the keyring (or of one of its subkeys); this lists the
// primary keys', each as the 40 upper-case hex characters of a version 4 key (RFC 4880 section
// 12.2). A keyring it cannot read is answered with the reason, in words, and no fingerprint: an
// armored file (the text a `gpg --armor` writes), a packet it cannot follow, a version 6 key
// (whose fingerprint is a SHA-256 this page does not make), no public key at all, or more than
// the eight keys a repository holds. The coordinator checks the keyring itself and refuses what
// it does not accept; this reads it only to name the keys.

// The most primary keys a repository lists.
const MAX_KEYS = 8;
// The packet tag of a primary public key.
const PUBLIC_KEY = 6;

// The SHA-1 digest of `bytes` (a Uint8Array) as upper-case hex. SHA-1 names a version 4 key; it is
// not trusted for anything here.
export function sha1Hex(bytes) {
  const rotate = (x, n) => ((x << n) | (x >>> (32 - n))) >>> 0;
  const padded = new Uint8Array((((bytes.length + 8) >> 6) + 1) << 6);
  padded.set(bytes);
  padded[bytes.length] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, Math.floor((bytes.length * 8) / 0x100000000));
  view.setUint32(padded.length - 4, (bytes.length * 8) >>> 0);
  let [h0, h1, h2, h3, h4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
  const w = new Array(80);
  for (let block = 0; block < padded.length; block += 64) {
    for (let i = 0; i < 16; i++) {
      w[i] = view.getUint32(block + i * 4);
    }
    for (let i = 16; i < 80; i++) {
      w[i] = rotate(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
    }
    let [a, b, c, d, e] = [h0, h1, h2, h3, h4];
    for (let i = 0; i < 80; i++) {
      let f;
      let k;
      if (i < 20) {
        f = (b & c) | (~b & d);
        k = 0x5a827999;
      } else if (i < 40) {
        f = b ^ c ^ d;
        k = 0x6ed9eba1;
      } else if (i < 60) {
        f = (b & c) | (b & d) | (c & d);
        k = 0x8f1bbcdc;
      } else {
        f = b ^ c ^ d;
        k = 0xca62c1d6;
      }
      const next = (rotate(a, 5) + f + e + k + w[i]) >>> 0;
      [e, d, c, b, a] = [d, c, rotate(b, 30), a, next];
    }
    h0 = (h0 + a) >>> 0;
    h1 = (h1 + b) >>> 0;
    h2 = (h2 + c) >>> 0;
    h3 = (h3 + d) >>> 0;
    h4 = (h4 + e) >>> 0;
  }
  return [h0, h1, h2, h3, h4].map((x) => x.toString(16).padStart(8, "0")).join("").toUpperCase();
}

// The packet at `at`: `{tag, start, end}` (the body is `start` to `end`), or null when the bytes
// do not hold a whole packet of a length this reads (a partial-length body is not read).
function packetAt(bytes, at) {
  const first = bytes[at];
  if ((first & 0x80) === 0) {
    return null;
  }
  let tag;
  let start;
  let length;
  if ((first & 0x40) === 0) {
    tag = (first >> 2) & 15;
    const kind = first & 3;
    if (kind === 3) {
      return { tag, start: at + 1, end: bytes.length };
    }
    const size = 1 << kind;
    if (at + 1 + size > bytes.length) {
      return null;
    }
    length = 0;
    for (let i = 0; i < size; i++) {
      length = length * 256 + bytes[at + 1 + i];
    }
    start = at + 1 + size;
  } else {
    tag = first & 0x3f;
    const one = bytes[at + 1];
    if (one === undefined || one >= 224 && one < 255) {
      return null;
    }
    if (one < 192) {
      length = one;
      start = at + 2;
    } else if (one < 224) {
      if (at + 2 >= bytes.length) {
        return null;
      }
      length = ((one - 192) << 8) + bytes[at + 2] + 192;
      start = at + 3;
    } else {
      if (at + 5 >= bytes.length) {
        return null;
      }
      length = bytes[at + 2] * 16777216 + (bytes[at + 3] << 16) + (bytes[at + 4] << 8) + bytes[at + 5];
      start = at + 6;
    }
  }
  return start + length > bytes.length ? null : { tag, start, end: start + length };
}

// The fingerprints of the primary keys of `bytes`, a binary keyring: `{fingerprints}` or
// `{problem}`, a sentence.
export function keyringFingerprints(bytes) {
  if (bytes.length === 0) {
    return { problem: "The signing key file is empty." };
  }
  if (bytes[0] === 0x2d) {
    return { problem: "The signing key is armored text; export it in binary form (gpg --export, without --armor)." };
  }
  const fingerprints = [];
  let at = 0;
  while (at < bytes.length) {
    const packet = packetAt(bytes, at);
    if (packet === null) {
      return { problem: "The signing key file is not a binary OpenPGP keyring." };
    }
    if (packet.tag === PUBLIC_KEY) {
      if (bytes[packet.start] === 6) {
        return { problem: "The keyring has a version 6 key, whose fingerprint this page does not make; use hkctl repositories put." };
      }
      if (bytes[packet.start] !== 4) {
        return { problem: "The keyring has a key of a version this page does not read." };
      }
      const body = bytes.subarray(packet.start, packet.end);
      const framed = new Uint8Array(body.length + 3);
      framed[0] = 0x99;
      framed[1] = body.length >> 8;
      framed[2] = body.length & 255;
      framed.set(body, 3);
      fingerprints.push(sha1Hex(framed));
    }
    at = packet.end;
  }
  if (fingerprints.length === 0) {
    return { problem: "The signing key file holds no public key." };
  }
  if (fingerprints.length > MAX_KEYS) {
    return { problem: `A repository lists at most ${MAX_KEYS} keys; this keyring has ${fingerprints.length}.` };
  }
  return { fingerprints };
}
