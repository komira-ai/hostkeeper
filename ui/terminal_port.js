// The Console page's terminal (`ctx.openTerminal`, which `main.js` hands the app): the vendored
// xterm.js in an element, on the open socket of what `ctx.openSocket` returned, with xterm.js's
// attach addon handed the socket itself (never an address). The addon writes what the socket
// brings (text as text, binary as bytes) and sends what the operator types (text) and what the
// terminal reports as binary (bytes); it lets go of the socket when the socket closes.
//
// `libs` holds the two constructors, `Terminal` and `AttachAddon` (the addon's module, whose
// `AttachAddon` is the class): in the browser the global object, on which each vendored file,
// run as a module, sets its name.

// How many lines the terminal keeps above its screen once they scroll off its top.
export const SCROLLBACK = 2000;

// A terminal in `element` on the open socket of `opened`. `show(into)` moves the terminal into
// another element (the page was drawn again) and draws it again, `focus()` gives it the keys and
// `dispose()` ends it; the socket stays the page's to close.
export function openTerminal(element, opened, libs = globalThis) {
  const terminal = new libs.Terminal({ convertEol: false, scrollback: SCROLLBACK });
  terminal.open(element);
  terminal.loadAddon(new libs.AttachAddon.AttachAddon(opened.socket));
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
