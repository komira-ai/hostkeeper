// The browser's entry: starts the app with the page's document, fetch, confirm and clock.
import { startApp } from "./app.js";
import { socketAddress, socketProtocols } from "./socket_address.js";
import { screenViewer } from "./screen.js";
import RFB from "./vendor/novnc/core/rfb.js";

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
// which refuses (throws for) a path of no fixed shape, offering the subprotocols `socketProtocols` builds
// (for a console stream, its own and the sign-in's CSRF token `csrf`; it throws for a console stream
// without one). `handlers.open(socket)` runs when it is open, with the open socket itself (which a
// console page hands to its viewer, as the viewer is given no address); `handlers.message(frame)`, when
// given, for each message (a string for text, a Uint8Array for binary); and `handlers.close(code, reason)`
// when it has closed, for any reason, with the close frame's code and reason (1006 and "" when none came).
// A text or a Uint8Array goes out through `send`, the other way round.
function openSocket(path, handlers, csrf) {
  const socket = new WebSocket(socketAddress(window.location, path), socketProtocols(path, csrf));
  socket.binaryType = "arraybuffer";
  socket.addEventListener("open", () => handlers.open(socket));
  if (typeof handlers.message === "function") {
    socket.addEventListener("message", (event) => {
      const { data } = event;
      handlers.message(typeof data === "string" ? data : new Uint8Array(data));
    });
  }
  socket.addEventListener("close", (event) => handlers.close(event.code, event.reason));
  return { send: (data) => socket.send(data), close: () => socket.close() };
}

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
  openSocket,
  viewScreen: screenViewer(RFB),
  now: () => Math.floor(Date.now() / 1000),
  prefs,
  every: (run, milliseconds) => {
    const timer = window.setInterval(run, milliseconds);
    return () => window.clearInterval(timer);
  },
});
