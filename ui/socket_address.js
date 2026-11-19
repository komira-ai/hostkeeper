// The address of the app's one socket, which `main.js` (the only module the scan of the sources lets
// open a socket) opens: the page's own host, over TLS when the page is, and a path of exactly one of
// the fixed shapes below. Any other path is refused before a socket is made.
//
// - The Remote SSH terminal: `/v1/ssh/terminal?host=` and a host id as `encodeURIComponent` writes it.
// - A console session's stream: `/console/v1/stream/` and the session id, 64 lower-case hex digits.
const PATHS = [
  /^\/v1\/ssh\/terminal\?host=[A-Za-z0-9\-_.!~*'()%]+$/,
  /^\/console\/v1\/stream\/[0-9a-f]{64}$/,
];

// The address of a socket to `path` on the host of `location` (the page's `window.location`), or
// an error naming the path when it is not one of the fixed shapes.
export function socketAddress(location, path) {
  if (typeof path !== "string" || !PATHS.some((shape) => shape.test(path))) {
    throw new Error(`not a socket path: ${String(path)}`);
  }
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}${path}`;
}
