// The browser's entry: starts the app with the page's document, fetch, confirm and clock, its one
// socket opener and its terminal.
import { startApp } from "./app.js";
import { socketAddress } from "./socket_address.js";
// The vendored terminal (`vendor/`, pinned by SHA-256): each file, run as a module, sets one name on
// the global object, `Terminal` and `AttachAddon`, which `openTerminal` below reads.
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
// itself, which only `openTerminal` takes.
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

// A terminal in `element`, on the open socket of `opened` (what `openSocket` returned): the
// vendored xterm.js, with its attach addon handed the socket itself (never an address), which
// writes what the socket brings and sends what the operator types. `show(element)` moves the
// terminal into another element (the page was drawn again), `focus()` gives it the keys and
// `dispose()` ends it; the socket stays the page's to close.
function openTerminal(element, opened) {
  const terminal = new globalThis.Terminal({ convertEol: false, scrollback: 2000 });
  terminal.open(element);
  terminal.loadAddon(new globalThis.AttachAddon.AttachAddon(opened.socket));
  return {
    show: (into) => {
      if (terminal.element && terminal.element.parentNode !== into) {
        into.appendChild(terminal.element);
        terminal.refresh(0, terminal.rows - 1);
      }
    },
    focus: () => terminal.focus(),
    dispose: () => terminal.dispose(),
  };
}

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
  openSocket,
  openTerminal,
  now: () => Math.floor(Date.now() / 1000),
  prefs,
  every: (run, milliseconds) => {
    const timer = window.setInterval(run, milliseconds);
    return () => window.clearInterval(timer);
  },
});
