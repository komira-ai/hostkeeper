// The browser's entry: starts the app with the page's document, fetch, confirm and clock, its one
// socket opener, its terminal and the operator's time zone.
import { startApp } from "./app.js";
import { socketAddress } from "./socket_address.js";
import { openTerminal } from "./terminal_port.js";
import { screenViewer } from "./screen.js";
// The vendored remote desktop viewer (`vendor/novnc/`, pinned by SHA-256): noVNC's `RFB`, which
// `screenViewer` (`screen.js`) builds on an open socket.
import RFB from "./vendor/novnc/core/rfb.js";
// The vendored terminal (`vendor/`, pinned by SHA-256): each file, run as a module, sets one name on
// the global object, `Terminal` and `AttachAddon`, which `openTerminal` (`terminal_port.js`) reads.
import "./vendor/xterm/xterm.js";
import "./vendor/xterm-addon-attach/addon-attach.js";

// The theme the operator chose is shown through the page's root element; the app keeps it in
// memory only (browser storage is not used anywhere: the scan of the sources forbids it).
const prefs = {
  apply: (theme) => {
    if (theme === "system") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.setAttribute("data-theme", theme);
    }
  },
};

// The one place that opens the socket (the scan of the sources refuses the word for it everywhere else): a
// socket to `path` of the page's own host (over TLS when the page is), at the address `socketAddress` builds,
// which refuses (throws for) a path of no fixed shape, offering the subprotocols `protocols` (a list of
// names; none when absent). `handlers.open()` runs when it is open, `handlers.message(frame)` for each
// message (a string for text, a Uint8Array for binary) and `handlers.close({code, reason})` when it has
// closed, for any reason, with the close frame's code and reason (1006 and "" when there was none). A
// text or a Uint8Array goes out through `send`, the other way round. `socket` is the open socket
// itself, which only `openTerminal` (`terminal_port.js`) and `viewScreen` (`screen.js`) take.
function openSocket(path, handlers, protocols) {
  const socket = new WebSocket(socketAddress(window.location, path), protocols);
  socket.binaryType = "arraybuffer";
  socket.addEventListener("open", () => handlers.open());
  socket.addEventListener("message", (event) => {
    const { data } = event;
    handlers.message(typeof data === "string" ? data : new Uint8Array(data));
  });
  socket.addEventListener("close", (event) => handlers.close({ code: event.code, reason: event.reason }));
  return { send: (data) => socket.send(data), close: () => socket.close(), socket };
}

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
  openSocket,
  openTerminal,
  viewScreen: screenViewer(RFB),
  now: () => Math.floor(Date.now() / 1000),
  // The operator's offset from UTC at an instant, in seconds east of UTC (UTC+01:00 is 3600).
  localOffset: (unix) => -new Date(unix * 1000).getTimezoneOffset() * 60,
  prefs,
  every: (run, milliseconds) => {
    const timer = window.setInterval(run, milliseconds);
    return () => window.clearInterval(timer);
  },
});
