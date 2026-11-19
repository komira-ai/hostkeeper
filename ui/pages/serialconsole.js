// Console: a Linux server's serial port (COM2, ttyS1), through its BMC's Serial over LAN and the
// coordinator (docs/design/console-broker.md), in a terminal on the page, with the host's Power
// and boot section beside it and the list of every open session. Home's server list links here
// for a Linux server (`{ nodeId }`).
// Calls: Console.List when the page is drawn and after each open and close; on a button,
// Console.Open with the operator's reason, then the session's socket; Console.Close of a listed
// session after a confirm. The Power and boot section is the host page's own (Console.Power and
// Console.SetBootDevice).
//
// The socket: one, to `/console/v1/stream/<session>` through `ctx.openSocket`, offering the
// session's subprotocol and `csrf.<the sign-in's CSRF token>`: the browser sends its sign-in as for
// every call, the CSRF token travels only in that offer, and the coordinator answers with the
// first name alone. Once it is open the page hands that socket to the terminal
// (`ctx.openTerminal`), which writes what it brings and sends what the operator types, and sends
// one carriage return so that a waiting login prompt is drawn again. A host whose console is on
// another port shows nothing at all: when no byte has come for the session's quiet seconds, the
// page says so. When the socket closes, the code and reason it closed with are said in words.

import { getCsrf } from "../session.js";
import {
  h, action, describeError, field, fmtUnix, list, pill, reasonProblem, redraw, table, trimmed,
} from "../view.js";
import { crumbs } from "../lab.js";
import { powerSection } from "./host.js";

// The name of the session's subprotocol, which the coordinator selects. It is spelled in pieces
// because the scan of the sources reads its middle word, followed by a dot, as the browser's log.
export const PROTOCOL = ["hostkeeper", "console", "v1"].join(".");

// How long the page waits for a first byte when the coordinator does not say (an older one).
export const DEFAULT_QUIET_SECS = 10;

// The note a silent console shows.
export const QUIET_NOTE = "No output yet: the host's serial port must be ttyS1.";

// What the operator reads for the reason a console closed with (the coordinator's close codes
// 4001 to 4013 and the three of the protocol it uses, each with its reason token).
export const CLOSES = {
  device_unreachable: "The BMC did not answer at the host's address, or its serial stream ended.",
  bmc_sol_busy: "Someone else holds the BMC's Serial over LAN (a person in the lab or another tool), or took it from this session.",
  bmc_busy: "The BMC has no free session. Try again in a minute.",
  bmc_auth_failed: "The BMC refused the coordinator's BMC credential.",
  idle: "Nothing crossed the console for too long, so it was closed.",
  total_limit: "The console reached the longest time a session may last and was closed.",
  closed_by_operator: "An operator closed this console session.",
  token_revoked: "Your sign-in or your console access ended, so the console was closed.",
  address_mismatch: "The BMC at the host's address reports another MAC than the host's record, so nothing was sent to it.",
  coordinator_stopping: "The coordinator is stopping and closed the session.",
  mac_screen_sharing_off: "The Mac refused the connection: Screen Sharing is off.",
  mac_unreachable_or_asleep: "The Mac did not answer: it is asleep, off or not at its address.",
  bmc_failed: "The coordinator could not run the BMC tool or read its answer.",
  device_closed: "The device closed the connection.",
  protocol_error: "The coordinator closed the console: it did not understand what the page sent.",
  message_too_big: "The coordinator closed the console: the page sent a message over 64 KiB.",
};

// The words for a close with `code` and `reason`; `opened` says whether the socket had opened.
// A close the coordinator gives no reason for is said by whether it had opened: before, the
// coordinator refused the connection (its answer is not readable by a page); after, it was lost.
export function closeWords(code, reason, opened) {
  if (typeof reason === "string" && Object.prototype.hasOwnProperty.call(CLOSES, reason)) {
    return CLOSES[reason];
  }
  if (typeof reason === "string" && reason !== "") {
    return `The console ended (${code} ${reason}).`;
  }
  if (!opened) {
    return "The coordinator did not accept the connection. Your sign-in may have ended: sign in again and open the console again.";
  }
  return code === 1006 ? "The connection to the coordinator was lost." : `The console ended (${code}).`;
}

// The words for a refusal of Console.Open that the operator can act on; any other is shown as its
// code and token.
const REFUSALS = {
  no_console_access: "You are not named in the coordinator's console operators file, so you cannot open one.",
  console_busy: "Someone already holds this host's console: see the open sessions below.",
  too_many_sessions: "The coordinator holds as many console sessions as it allows. Close one and try again.",
  no_address: "The coordinator's console targets file gives no address for this host's BMC.",
  host_not_found: "The coordinator knows no host of this id.",
  audit_unavailable: "The coordinator could not write the audit entry. Nothing was done; try again.",
};

function refusalWords(error) {
  const reason = error && typeof error.reason === "string" ? error.reason : "";
  return Object.prototype.hasOwnProperty.call(REFUSALS, reason) ? REFUSALS[reason] : `The console did not open: ${describeError(error)}.`;
}

// The seconds the session asks the page to wait for a first byte: its `quietSecs`, else the default.
export function quietSecsOf(reply) {
  const secs = Number(reply.quietSecs);
  return Number.isInteger(secs) && secs > 0 ? secs : DEFAULT_QUIET_SECS;
}

// The console part of the page: the reason, Open or Close, what the console is doing in words,
// the quiet note and the terminal.
function consoleSection(ctx, nodeId, sessions) {
  const form = { reason: "" };
  const view = { phase: "idle", notice: "", quiet: false, conn: null, viewer: null, left: false };
  let head = null;
  let box = null;

  const paintHead = () => {
    if (head !== null) {
      redraw(head, h("div", {}, headTree()));
    }
  };
  // The terminal goes into the box the page shows now: made once the socket is open, moved when
  // the page was drawn again (the box is then another element).
  const placeViewer = () => {
    const conn = view.conn;
    if (box === null) {
      return;
    }
    if (view.viewer !== null) {
      view.viewer.show(box);
    } else if (conn !== null && conn.opened) {
      view.viewer = ctx.openTerminal(box, conn.socket);
      view.viewer.focus();
    }
  };

  const stopQuiet = (conn) => {
    if (conn.stopQuiet !== null) {
      conn.stopQuiet();
      conn.stopQuiet = null;
    }
  };

  // Ends the console now, on our side, with `notice` as the reason shown.
  const end = (notice) => {
    const conn = view.conn;
    view.conn = null;
    view.phase = "ended";
    view.notice = notice;
    view.quiet = false;
    if (conn !== null) {
      stopQuiet(conn);
      conn.socket.close();
    }
    paintHead();
  };
  // Leaving the page (another page, a sign-out, the session ending) closes the console at once.
  if (typeof ctx.onLeave === "function") {
    ctx.onLeave(() => {
      view.left = true;
      if (view.conn !== null) {
        end("You left the page, so the session closed.");
      }
      if (view.viewer !== null) {
        view.viewer.dispose();
        view.viewer = null;
      }
    });
  }

  const attach = (reply, csrf) => {
    const conn = { socket: null, opened: false, heard: false, stopQuiet: null };
    const live = () => view.conn === conn;
    const quietSecs = quietSecsOf(reply);
    if (view.viewer !== null) {
      view.viewer.dispose();
      view.viewer = null;
    }
    view.conn = conn;
    view.phase = "connecting";
    view.notice = "Connecting to the serial port.";
    view.quiet = false;
    conn.socket = ctx.openSocket(`/console/v1/stream/${reply.session}`, {
      open: () => {
        if (!live()) {
          return;
        }
        conn.opened = true;
        view.phase = "open";
        view.notice = "Connected to the serial port.";
        placeViewer();
        conn.socket.send("\r");
        conn.stopQuiet = ctx.every(() => {
          stopQuiet(conn);
          if (live() && !conn.heard) {
            view.quiet = true;
            paintHead();
          }
        }, quietSecs);
        paintHead();
      },
      message: () => {
        if (!live() || conn.heard) {
          return;
        }
        conn.heard = true;
        stopQuiet(conn);
        if (view.quiet) {
          view.quiet = false;
          paintHead();
        }
      },
      close: (closed) => {
        if (!live()) {
          return;
        }
        const { code, reason } = closed || {};
        stopQuiet(conn);
        view.conn = null;
        view.phase = "ended";
        view.quiet = false;
        view.notice = closeWords(code, reason, conn.opened);
        paintHead();
        sessions.load();
      },
    }, [PROTOCOL, `csrf.${csrf}`]);
  };

  const open = async () => {
    if (typeof ctx.openSocket !== "function" || typeof ctx.openTerminal !== "function") {
      return "This page cannot open a terminal.";
    }
    if (view.conn !== null) {
      return "A console is already open.";
    }
    const reason = trimmed(form.reason);
    if (reason === "") {
      return "Give a reason first.";
    }
    const problem = reasonProblem(reason);
    if (problem !== null) {
      return problem;
    }
    let reply;
    try {
      reply = await ctx.api.console.open({ hostId: nodeId, reason });
    } catch (error) {
      view.notice = refusalWords(error);
      paintHead();
      sessions.load();
      return undefined;
    }
    const csrf = getCsrf();
    if (view.left) {
      // The operator left the page while the session opened: nobody would see it, so it is
      // closed at once instead of attached.
      await ctx.api.console.close({ hostId: nodeId }).catch(() => undefined);
      return undefined;
    }
    if (reply.kind !== "CONSOLE_KIND_SERIAL" || csrf === null) {
      // The session is not one this page can show (a Mac's screen), or the sign-in ended while it
      // opened: it is closed at once instead of left to its window.
      view.notice = csrf === null ? "Sign in again, then open the console again."
        : `${nodeId} is not a Linux server: its console is not a serial port, so it was closed.`;
      paintHead();
      await ctx.api.console.close({ hostId: nodeId }).catch(() => undefined);
      sessions.load();
      return undefined;
    }
    attach(reply, csrf);
    paintHead();
    sessions.load();
    return undefined;
  };

  const headTree = () => [
    h("div", { class: "row" },
      view.conn !== null
        ? action(ctx, "Close", "close-console", async () => {
          end("You closed the session.");
          await sessions.load();
          return undefined;
        })
        : action(ctx, "Open the console", "open-console", open, { class: "primary" }),
      view.notice === "" ? null : h("span", { name: "console-status", role: "status" }, view.notice),
      view.phase === "open" ? pill("connected", "ok") : null),
    view.quiet ? h("p", { class: "note", name: "console-quiet", role: "status" }, QUIET_NOTE) : null,
  ];

  return h("section", { class: "card serialconsole" },
    h("h3", {}, "Serial console"),
    h("p", { class: "quiet" },
      "The server's second serial port (COM2, ttyS1) through its BMC's Serial over LAN, reached by the coordinator. "
      + "Opening it is audited with the reason; what is typed is the host's own login. It closes when you leave this page or sign out."),
    field("Reason", "console-reason", form, "reason", { maxlength: 256 }),
    h("div", { name: "console-head", on: { mounted: (e) => { head = e.currentTarget; paintHead(); } } },
      h("div", {}, headTree())),
    h("div", {
      class: "consolebox", name: "terminal", role: "application", "aria-label": "serial console",
      on: { mounted: (e) => { box = e.currentTarget; placeViewer(); } },
    }));
}

// Every open console session, with a Close for each; drawn again in place after each open and close.
function sessionsSection(ctx, nodeId) {
  const state = { sessions: null, failure: null };
  let at = null;
  const paint = () => {
    if (at !== null) {
      redraw(at, h("div", {}, sessionsTree()));
    }
  };
  const load = async () => {
    try {
      state.sessions = list(await ctx.api.console.list({}), "sessions");
      state.failure = null;
    } catch (error) {
      state.sessions = null;
      state.failure = error;
    }
    paint();
  };
  const closeOne = (session) => action(ctx, "Close", "close-session", async () => {
    const yours = session.hostId === nodeId ? " It is this host's own session." : "";
    if (!ctx.confirm(`Close the console session of ${session.hostId}, held by ${session.openedBy}?${yours}`)) {
      return "Not closed.";
    }
    await ctx.api.console.close({ hostId: session.hostId });
    await load();
    return `The console session of ${session.hostId} is closed.`;
  });
  const sessionsTree = () => {
    if (state.failure !== null) {
      return [h("p", { class: "error", name: "sessions-failed" }, `The open sessions could not be read: ${describeError(state.failure)}.`)];
    }
    if (state.sessions === null) {
      return [h("p", { class: "quiet" }, "Reading the open sessions.")];
    }
    if (state.sessions.length === 0) {
      return [h("p", { class: "quiet", name: "no-sessions" }, "No console session is open.")];
    }
    return [table(["Host", "Held by", "Since", "Attached", ""], state.sessions.map((s) => h("tr", {},
      h("td", { class: "mono" }, s.hostId || "-"),
      h("td", {}, s.openedBy || "-"),
      h("td", {}, fmtUnix(s.openedUnix)),
      h("td", {}, s.attached === true ? "yes" : "no"),
      h("td", {}, closeOne(s)))))];
  };
  return {
    load,
    tree: h("section", { class: "card" },
      h("h3", {}, "Open sessions"),
      h("p", { class: "quiet" }, "One console per host. Closing another's session is audited with your name."),
      h("div", { name: "sessions", on: { mounted: (e) => { at = e.currentTarget; paint(); } } },
        h("div", {}, sessionsTree()))),
  };
}

export async function render(ctx) {
  const nodeId = typeof ctx.params.nodeId === "string" ? ctx.params.nodeId : "";
  const sessions = sessionsSection(ctx, nodeId);
  await sessions.load();
  return h("section", { class: "page" },
    crumbs({ racks: [] }, { host: nodeId, leaf: "Console" }, ctx.navigate),
    h("h2", {}, `Console of ${nodeId}`),
    h("div", { class: "consolepage" },
      consoleSection(ctx, nodeId, sessions),
      h("div", { class: "consoleside" }, powerSection(ctx, nodeId), sessions.tree)));
}
