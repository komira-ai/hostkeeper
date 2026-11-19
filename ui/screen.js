// The viewer of a Mac's screen: noVNC's `RFB` over the console stream's open socket. `main.js`
// imports the vendored library and hands `screenViewer(RFB)` to the app as `viewScreen`, which a
// page finds as `ctx.viewScreen`, so the pages never load the library and the tests run them with
// a fake.
//
// `viewScreen(target, socket, events)` draws the screen in `target` (an element of the page) from
// `socket`, the open socket of `ctx.openSocket` (the library is given no address). It is called in
// the socket's `open` handler: a VNC server speaks first, and the library must be listening
// before its first byte arrives. `events` is told what the library says:
//
// - `connect()`: the Mac's screen is shown;
// - `credentials(types)`: the Mac asks for its login (`types` lists `username` and `password`, as
//   the library asks them);
// - `securityFailure(reason)`: the Mac refused the login (`reason` is the Mac's words, or "");
// - `disconnect(clean)`: the viewer ended, cleanly or not.
//
// It answers `{ sendCredentials({username, password}), disconnect(), focus() }`.
export function screenViewer(RFB) {
  return (target, socket, events) => {
    const rfb = new RFB(target, socket);
    // The Mac's screen is scaled down to the page, and the Mac's own resolution is left alone.
    rfb.scaleViewport = true;
    rfb.resizeSession = false;
    rfb.addEventListener("connect", () => events.connect());
    rfb.addEventListener("credentialsrequired", (event) => {
      const types = event.detail && Array.isArray(event.detail.types) ? event.detail.types : ["password"];
      events.credentials(types.filter((type) => type === "username" || type === "password"));
    });
    rfb.addEventListener("securityfailure", (event) => {
      const reason = event.detail && typeof event.detail.reason === "string" ? event.detail.reason : "";
      events.securityFailure(reason);
    });
    rfb.addEventListener("disconnect", (event) => events.disconnect(Boolean(event.detail && event.detail.clean)));
    return {
      sendCredentials: ({ username, password }) => rfb.sendCredentials({ username, password }),
      disconnect: () => rfb.disconnect(),
      focus: () => rfb.focus(),
    };
  };
}
