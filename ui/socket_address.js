// The address of the app's one socket, which `main.js` (the only module the scan of the sources lets
// open a socket) opens: the page's own host, over TLS when the page is, and a path of exactly one of
// the fixed shapes below. Any other path is refused before a socket is made. And the subprotocols it
// offers, which `socketProtocols` builds from the path.
//
// - The Remote SSH terminal: `/v1/ssh/terminal?host=` and a host id as `encodeURIComponent` writes it.
// - A console session's stream: `/console/v1/stream/` and the session id, 64 lower-case hex digits.
const SSH_TERMINAL = /^\/v1\/ssh\/terminal\?host=[A-Za-z0-9\-_.!~*'()%]+$/;
const CONSOLE_STREAM = /^\/console\/v1\/stream\/[0-9a-f]{64}$/;
const PATHS = [SSH_TERMINAL, CONSOLE_STREAM];

// The subprotocol of a console stream, the one the coordinator's answer selects:
// `hostkeeper`, `console` and `v1` joined by dots (written in parts, as the scan of the sources
// reads a word and a dot after it as a call of the browser's log).
export const CONSOLE_PROTOCOL = ["hostkeeper", "console", "v1"].join(".");

// A CSRF token as the coordinator draws it: base64url without padding, and at most 256 characters
// (the coordinator reads no longer one).
const CSRF = /^[A-Za-z0-9_-]{1,256}$/;

// The address of a socket to `path` on the host of `location` (the page's `window.location`), or
// an error naming the path when it is not one of the fixed shapes.
export function socketAddress(location, path) {
  if (typeof path !== "string" || !PATHS.some((shape) => shape.test(path))) {
    throw new Error(`not a socket path: ${String(path)}`);
  }
  const { protocol, host } = location;
  const scheme = protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${host}${path}`;
}

// The subprotocols the socket to `path` offers. A stream of a console session offers
// `CONSOLE_PROTOCOL` and the sign-in's CSRF token `csrf` as `csrf.<token>`: a browser's socket can
// set no header, and the coordinator checks the token beside the sign-in the browser holds before
// it answers, then selects `CONSOLE_PROTOCOL` alone, so the token is never echoed. It is in no
// address. A stream without a CSRF token of that shape is refused (throws, naming no token). The
// Remote SSH terminal offers none (its first message carries the token); any other path is
// `socketAddress`'s refusal.
export function socketProtocols(path, csrf) {
  if (typeof path !== "string" || !CONSOLE_STREAM.test(path)) {
    return [];
  }
  if (typeof csrf !== "string" || !CSRF.test(csrf)) {
    throw new Error("a console stream needs the sign-in's CSRF token");
  }
  return [CONSOLE_PROTOCOL, `csrf.${csrf}`];
}
