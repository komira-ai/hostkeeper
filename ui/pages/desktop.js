// Desktop: a Mac's screen through the coordinator (docs/design/console-broker.md, sections 3 and
// 11.2). The operator gives a reason and opens it: Console.Open takes the Mac's one console slot and
// writes the audit line, then the page opens the session's stream, the app's one socket kind, on the
// coordinator (`/console/v1/stream/<session>`, offering the sign-in's CSRF token in its subprotocol;
// the address is never given to the viewer), and in the socket's `open` handler hands the open socket
// to the viewer (`ctx.viewScreen`, noVNC), which must be listening before the Mac's first byte. The
// Mac's own login is typed into the form the page shows when the viewer asks for it, and goes to the
// Mac through the viewer; the page keeps no copy once it is sent.
//
// One desktop per page: a second Open while one is open sends nothing. Leaving the page (another
// page, a sign-out, the page drawn again) closes it; so do the Close button and the coordinator,
// whose close code the page shows in the Console page's words (`closeWords` of `serialconsole.js`).
//
// Params: `nodeId`, the Mac's host id.
// Calls: Console.List when the page is read (who holds the Mac's console, with a Close for it);
// on a button, Console.Open, Console.Close (a holder's session, or the one just opened when the host
// is not a Mac).

import { h, action, describeError, field, fmtUnix, list, reasonProblem, redraw, trimmed } from "../view.js";
import { getCsrf } from "../session.js";
import { PROTOCOL, closeWords } from "./serialconsole.js";

// The path of a console session's stream.
const STREAM = "/console/v1/stream/";

// The open session of `nodeId` that Console.List names, `null` when none, or why it could not be read.
async function holderOf(ctx, nodeId) {
  try {
    const reply = await ctx.api.console.list({});
    const held = list(reply, "sessions").find((s) => s.hostId === nodeId);
    return { held: held === undefined ? null : held, failure: null };
  } catch (error) {
    return { held: null, failure: error };
  }
}

// What the page says of the session someone else holds.
function holderWords(nodeId, held) {
  const by = held.openedBy || "an operator";
  const since = held.openedUnix ? ` since ${fmtUnix(held.openedUnix)}` : "";
  const state = held.attached === true ? "attached" : "not attached yet";
  const why = held.reason ? `: ${held.reason}` : "";
  return `${by} holds the console of ${nodeId}${since} (${state})${why}.`;
}

export async function render(ctx) {
  const nodeId = typeof ctx.params.nodeId === "string" ? ctx.params.nodeId : "";
  if (nodeId === "") {
    return h("section", { class: "page" },
      h("h2", {}, "Desktop"),
      h("p", { class: "error", role: "alert" }, "No host was named."));
  }
  const holder = await holderOf(ctx, nodeId);
  const form = { reason: "", username: "", password: "" };
  // `phase`: idle, opening (Console.Open sent), connecting (the stream asked for, or the login
  // sent), login (the Mac asks for its login), open (the screen is shown), ended.
  const view = { phase: "idle", notice: "", conn: null, asks: [] };
  let head = null;
  let screen = null;
  // Whether a Console.Open is on its way and not answered yet.
  let asking = false;

  const paintHead = () => {
    if (head !== null) {
      redraw(head, h("div", {}, headTree()));
    }
  };

  // Takes the viewer's elements out of the page: the viewer removes them only from the element it
  // was given, which a page mounted again has replaced.
  const clearScreen = () => {
    if (screen !== null) {
      screen.replaceChildren();
    }
  };

  // Ends the desktop now, on this side, with `notice` as what the page says.
  const end = (notice) => {
    const conn = view.conn;
    view.conn = null;
    view.phase = "ended";
    view.notice = notice;
    view.asks = [];
    form.password = "";
    if (conn !== null) {
      if (conn.viewer !== null) {
        conn.viewer.disconnect();
      }
      if (conn.socket !== null) {
        conn.socket.close();
      }
    }
    clearScreen();
    paintHead();
  };

  // Leaving the page (another page, a sign-out, this page drawn again) closes the desktop.
  if (typeof ctx.onLeave === "function") {
    ctx.onLeave(() => {
      if (view.conn !== null) {
        end("You left the desktop.");
      }
    });
  }

  // What the viewer of `conn` tells the page.
  const viewerEvents = (conn) => {
    const live = () => view.conn === conn;
    return {
      connect: () => {
        if (live()) {
          view.phase = "open";
          view.asks = [];
          view.notice = `The screen of ${nodeId} is shown.`;
          paintHead();
        }
      },
      credentials: (types) => {
        if (live()) {
          view.phase = "login";
          view.asks = types.length === 0 ? ["password"] : types;
          view.notice = "The Mac asks for its own login: type it below. It goes to the Mac through the viewer and is not kept.";
          paintHead();
        }
      },
      securityFailure: (reason) => {
        if (live()) {
          view.notice = reason === "" ? "The Mac refused the login." : `The Mac refused the login: ${reason}`;
          // The viewer closes the socket itself after a refused login: that close carries no word
          // of the coordinator's, and the Mac's refusal stays what the page says.
          conn.said = view.notice;
          paintHead();
        }
      },
      // The socket's close says why the desktop ended; the viewer's own word adds nothing.
      disconnect: () => {},
    };
  };

  // Gives back the session just opened, which no stream will attach to.
  const giveBack = async () => {
    try {
      await ctx.api.console.close({ hostId: nodeId });
    } catch (error) {
      // An unattached session ends by itself when its window passes.
    }
  };

  const open = async () => {
    if (view.conn !== null) {
      return "A desktop is already open.";
    }
    if (asking) {
      // An Open the operator closed before it was answered is given back by host when it is: a
      // second one now could be the session given back.
      return "The coordinator has not answered the last Open yet.";
    }
    if (typeof ctx.openSocket !== "function" || typeof ctx.viewScreen !== "function") {
      return "This page cannot open a desktop.";
    }
    const reason = trimmed(form.reason);
    if (reason === "") {
      return "Give a reason first.";
    }
    const problem = reasonProblem(reason);
    if (problem !== null) {
      return problem;
    }
    const csrf = getCsrf();
    if (csrf === null) {
      return "Sign in again to open a desktop.";
    }
    const conn = { socket: null, viewer: null, opened: false, said: null };
    const live = () => view.conn === conn;
    view.conn = conn;
    view.phase = "opening";
    view.notice = "Asking the coordinator for a session.";
    paintHead();
    clearScreen();
    let reply;
    asking = true;
    try {
      reply = await ctx.api.console.open({ hostId: nodeId, reason });
    } catch (error) {
      asking = false;
      if (live()) {
        view.conn = null;
        view.phase = "ended";
        view.notice = `The desktop did not open: ${describeError(error)}.`;
        paintHead();
      }
      return undefined;
    }
    if (!live()) {
      // The operator left or closed while the coordinator answered.
      await giveBack();
      asking = false;
      return undefined;
    }
    asking = false;
    if (reply.kind !== "CONSOLE_KIND_SCREEN" || screen === null) {
      view.conn = null;
      view.phase = "ended";
      view.notice = reply.kind !== "CONSOLE_KIND_SCREEN"
        ? `${nodeId} is not a Mac: its console is not a screen. The session was given back.`
        : "The page has no place to draw the screen. The session was given back.";
      paintHead();
      await giveBack();
      return undefined;
    }
    try {
      conn.socket = ctx.openSocket(`${STREAM}${reply.session}`, {
        open: () => {
          if (live() && conn.viewer === null) {
            conn.opened = true;
            view.phase = "connecting";
            view.notice = "Connected to the coordinator; waiting for the Mac.";
            conn.viewer = ctx.viewScreen(screen, conn.socket, viewerEvents(conn));
            paintHead();
          }
        },
        // The viewer reads the stream itself; the page takes no message.
        message: () => {},
        close: (closed) => {
          if (live()) {
            const code = closed && typeof closed.code === "number" ? closed.code : 1006;
            const why = closed && typeof closed.reason === "string" ? closed.reason : "";
            view.conn = null;
            view.phase = "ended";
            view.asks = [];
            form.password = "";
            view.notice = why === "" && conn.said !== null ? conn.said : closeWords(code, why, conn.opened);
            clearScreen();
            paintHead();
          }
        },
      }, [PROTOCOL, `csrf.${csrf}`]);
    } catch (error) {
      // The opener refused the session's path: the coordinator's answer is not a session id.
      view.conn = null;
      view.phase = "ended";
      view.notice = "The coordinator's answer named no session this page can open. The session was given back.";
      paintHead();
      await giveBack();
      return undefined;
    }
    view.phase = "connecting";
    view.notice = "Connecting to the coordinator.";
    paintHead();
    return undefined;
  };

  const signIn = async () => {
    const conn = view.conn;
    if (conn === null || conn.viewer === null || view.phase !== "login") {
      return undefined;
    }
    const creds = { username: form.username, password: form.password };
    form.password = "";
    view.phase = "connecting";
    view.notice = "Login sent to the Mac.";
    conn.viewer.sendCredentials(creds);
    paintHead();
    return undefined;
  };

  const closeHolder = async () => {
    if (!ctx.confirm(`Close the console of ${nodeId}, which ${holder.held.openedBy || "an operator"} holds?`)) {
      return "Not closed.";
    }
    await ctx.api.console.close({ hostId: nodeId });
    ctx.refresh();
    return `The console of ${nodeId} was closed.`;
  };

  const holderSection = () => {
    if (holder.failure !== null) {
      return h("p", { class: "quiet", name: "desktop-holder" },
        `Who holds a console could not be read: ${describeError(holder.failure)}.`);
    }
    if (holder.held === null) {
      return h("p", { class: "quiet", name: "desktop-holder" }, `No console of ${nodeId} is open.`);
    }
    return h("div", { class: "row" },
      h("p", { name: "desktop-holder" }, holderWords(nodeId, holder.held)),
      action(ctx, "Close it", "close-holder", closeHolder, { class: "danger" }));
  };

  const loginForm = () => {
    const parts = [];
    if (view.asks.includes("username")) {
      parts.push(field("Mac user name", "mac-username", form, "username"));
    }
    parts.push(field("Mac password", "mac-password", form, "password", { type: "password" }));
    parts.push(action(ctx, "Sign in to the Mac", "mac-sign-in", signIn, { class: "primary" }));
    return h("div", { class: "buttons" }, parts);
  };

  const headTree = () => {
    const busy = view.conn !== null;
    return [
      view.phase === "idle" ? holderSection() : null,
      busy ? null : field("Reason", "desktop-reason", form, "reason", { maxlength: 256 }),
      h("div", { class: "row" },
        busy
          ? action(ctx, "Close", "close-desktop", async () => {
            end("You closed the desktop.");
            return undefined;
          })
          : action(ctx, "Open the desktop", "open-desktop", open, { class: "primary" })),
      view.phase === "login" ? loginForm() : null,
      h("p", { class: "quiet", name: "desktop-status", role: "status" }, view.notice),
    ];
  };

  // The element the viewer draws in. When the app mounts the page again (a status line, the menu
  // folded) while a desktop is open, the viewer's elements move into the new one.
  const adopt = (element) => {
    if (screen !== null && screen !== element && view.conn !== null) {
      element.replaceChildren(...Array.from(screen.children));
    }
    screen = element;
  };

  return h("section", { class: "page desktop" },
    h("div", { class: "row" },
      h("h2", {}, `Desktop: ${nodeId}`),
      h("button", { type: "button", class: "linklike", name: "open-host", on: { click: () => ctx.navigate("host", { nodeId }) } },
        "Host page")),
    h("p", { class: "note" },
      "The Mac's screen through the coordinator, with Screen Sharing. Opening it takes the Mac's one console "
      + "and is audited with the reason; the Mac's own login is typed into the viewer. A Mac that is asleep, off, "
      + "or not booted has no desktop."),
    // Drawn again from the page's state each time it is mounted, so a page mounted again shows
    // where the desktop is now.
    h("div", { name: "desktop-head", on: { mounted: (e) => { head = e.currentTarget; paintHead(); } } }, headTree()),
    h("div", { class: "desktop-screen", name: "desktop-screen", on: { mounted: (e) => adopt(e.currentTarget) } }));
}
