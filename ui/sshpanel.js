// The Remote SSH tab of the Host page (`pages/host.js` makes the two calls and hands them in): the pin of the host's SSH host keys, and a terminal on the
// host through the coordinator (docs/design/ssh-through-coordinator.md section 13).
//
// The pin: `Ssh.HostKeys` says whether the host's keys are pinned. When none is pinned, or the host
// reports others than the pinned ones, and the host's last heartbeat reported its keys, the tab
// lists them and offers to pin them (after a question), which `Ssh.PinHostKeys` takes.
//
// The terminal: one socket to `/v1/ssh/terminal?host=<id>`, opened through `ctx.openSocket`. Its
// first message is the session's CSRF token and the terminal's size as text; after that, what the
// operator types and pastes goes out as binary messages and the host's output comes back as binary
// messages, which `terminal.js` takes and `terminal_render.js` draws. Text messages from the
// coordinator say how far it got (`status`) and, last, why the terminal ended (`closed`).

import { getCsrf } from "./session.js";
import { DEFAULT_COLS, DEFAULT_ROWS, Terminal } from "./terminal.js";
import { encodeKey, encodePaste, pieces } from "./terminal_keys.js";
import { drawTerminal } from "./terminal_render.js";
import { action, describeError, h, list, pill, redraw } from "./view.js";

// What the operator reads for each token the coordinator ends a terminal with: the refusals of
// `SshRefusal`, the causes a tunnel closes with, and the ones only a terminal has.
export const CAUSES = {
  // The refusals of the `Ssh` service.
  unauthorized: "Your sign-in is not accepted. Sign in again.",
  forbidden: "This sign-in may not open a terminal.",
  not_permitted: "You are not permitted a terminal on this host.",
  invalid_request: "The coordinator did not accept the request.",
  bad_reason: "The coordinator did not accept the reason for the certificate.",
  bad_key: "The coordinator did not accept the terminal's key.",
  unknown_host: "The coordinator does not know this host: it is not enrolled and approved.",
  not_configured: "The coordinator has no SSH certificate authority, so it cannot open terminals.",
  host_not_live: "The host has not sent a heartbeat lately, so the coordinator will not connect to it.",
  no_address: "The coordinator holds no address for the host's last heartbeat.",
  bad_serial: "The coordinator did not accept the certificate it had just made.",
  unknown_cert: "The coordinator did not find the certificate.",
  too_many_tunnels: "Too many terminals are open: you or the coordinator are at the limit, or you already have one to this host.",
  draining: "This coordinator is stopping and takes no new terminal. Try again.",
  dial_failed: "The coordinator could not connect to the host's SSH port in time.",
  store_failed: "The coordinator could not record the terminal. Nothing was done; try again.",
  audit_unavailable: "The coordinator could not write the audit entry. Nothing was done; try again.",
  not_reported: "The host keys are not the ones the host reported.",
  // The causes a tunnel ends with.
  eof: "The connection ended.",
  idle: "The terminal was idle for too long and was closed.",
  max_session: "The terminal reached the longest time a session may last and was closed.",
  token_revoked: "Your sign-in ended (you signed out, it expired, or your account was removed), so the terminal was closed.",
  revoked: "Your certificate was revoked, so the terminal was closed.",
  host_closed: "The host closed the connection.",
  coordinator_stopping: "The coordinator is stopping and closed the terminal.",
  instance_lost: "The coordinator instance that held the terminal was lost.",
  error: "The connection to the host failed.",
  // The ends only a terminal has.
  no_pin: "No host key is pinned for this host. Pin the host's keys (above), then open the terminal again.",
  host_key_mismatch: "The host presented an SSH host key that is not the pinned one, so nothing was sent to it. It may be another machine.",
  auth_refused: "The host refused the certificate.",
  shell_refused: "The host accepted the login but gave no shell.",
  bad_request: "The coordinator did not accept the first message of the terminal.",
  exit: "The shell exited.",
};

// The sentence for a token that ended a terminal; a token this page has no words for is shown as
// it came.
export function causeWords(cause) {
  return Object.prototype.hasOwnProperty.call(CAUSES, cause) ? CAUSES[cause] : `The terminal ended (${cause}).`;
}

// What the tab knows of the host's keys: the pin the coordinator holds (`{state, fingerprints}`)
// or why it could not be read (`failure`), and the fingerprints the host's last heartbeat reported.
// `readPin()` is the call of `Ssh.HostKeys` for the host.
export async function loadSsh(host, readPin) {
  const reported = list((host.heartbeat || {}).ssh, "hostKeyFingerprints");
  try {
    const reply = await readPin();
    return { keys: { state: reply.state || "", fingerprints: list(reply, "fingerprints") }, failure: null, reported };
  } catch (error) {
    return { keys: null, failure: error, reported };
  }
}

// The words under the buttons for the step the coordinator said it was at.
const STEPS = {
  signing: "The coordinator is signing a certificate for you.",
  connecting: "The coordinator is connecting to the host.",
};

function pinWords(state) {
  if (state === "pinned") {
    return "The host's SSH host keys are pinned. The coordinator connects to the host only if it presents one of them.";
  }
  if (state === "no_pin") {
    return "No host key is pinned for this host, so no terminal opens until one is.";
  }
  if (state === "changed") {
    return "The host reports SSH host keys other than the pinned ones. The pinned ones stay in force.";
  }
  return `The pin of the host's SSH host keys is "${state}".`;
}

// Ends the terminal of the panel built last, if it is still open: the page that held it was drawn
// again (a refresh, another host) and its elements are gone, so nobody can see or close it.
let endLast = null;

// The panel, as a list of elements. `loaded` is what `loadSsh` returned and `pin(fingerprints)` the
// call of `Ssh.PinHostKeys` for the host.
export function sshPanel(ctx, nodeId, loaded, pin) {
  const view = {
    keys: loaded.keys,
    phase: "idle",
    notice: "",
    conn: null,
    term: null,
  };
  let head = null;
  let box = null;
  if (endLast !== null) {
    endLast();
  }

  const paintHead = () => {
    if (head !== null) {
      redraw(head, h("div", {}, headTree()));
    }
  };
  const paintScreen = () => {
    if (box === null || view.term === null) {
      return;
    }
    if (box.isConnected === false) {
      // The tab was drawn again or left: nobody sees this terminal, so it ends.
      end("You left the terminal.");
      return;
    }
    redraw(box, drawTerminal(view.term));
    if (typeof box.scrollHeight === "number") {
      box.scrollTop = box.scrollHeight;
    }
  };

  // Ends the terminal now, on our side, with `notice` as the reason shown.
  const end = (notice) => {
    const conn = view.conn;
    view.conn = null;
    view.phase = "ended";
    view.notice = notice;
    if (conn !== null) {
      conn.socket.close();
    }
    paintHead();
  };

  endLast = () => {
    if (view.conn !== null) {
      end("You left the terminal.");
    }
  };
  // Leaving the page (another page, a sign-out) ends the terminal at once, idle or not.
  if (typeof ctx.onLeave === "function") {
    ctx.onLeave(() => {
      if (view.conn !== null) {
        end("You left the terminal.");
      }
    });
  }

  const sendBytes = (bytes) => {
    if (view.conn !== null && view.phase === "open") {
      for (const piece of pieces(bytes)) {
        view.conn.socket.send(piece);
      }
    }
  };

  const onText = (conn, text) => {
    let said = null;
    try {
      said = JSON.parse(text);
    } catch (error) {
      return;
    }
    if (said === null || typeof said !== "object") {
      return;
    }
    if (said.type === "status") {
      if (said.step === "opened") {
        view.phase = "open";
        view.notice = `Connected${typeof said.login_user === "string" ? ` as ${said.login_user}` : ""}.`;
      } else if (Object.prototype.hasOwnProperty.call(STEPS, said.step)) {
        view.notice = STEPS[said.step];
      }
      paintHead();
    } else if (said.type === "closed") {
      view.conn = null;
      view.phase = "ended";
      view.notice = causeWords(typeof said.cause === "string" ? said.cause : "unknown");
      conn.socket.close();
      paintHead();
    }
  };

  const open = () => {
    if (typeof ctx.openSocket !== "function") {
      return "This page cannot open a terminal.";
    }
    const csrf = getCsrf();
    if (csrf === null) {
      return "Sign in again to open a terminal.";
    }
    if (view.conn !== null) {
      return "A terminal is already open.";
    }
    const conn = { socket: null, opened: false };
    const live = () => view.conn === conn;
    view.term = new Terminal(DEFAULT_COLS, DEFAULT_ROWS);
    view.phase = "connecting";
    view.notice = "Connecting to the coordinator.";
    view.conn = conn;
    conn.socket = ctx.openSocket(`/v1/ssh/terminal?host=${encodeURIComponent(nodeId)}`, {
      open: () => {
        if (live()) {
          conn.opened = true;
          conn.socket.send(JSON.stringify({ csrf, cols: DEFAULT_COLS, rows: DEFAULT_ROWS }));
        }
      },
      message: (frame) => {
        if (!live()) {
          return;
        }
        if (typeof frame === "string") {
          onText(conn, frame);
        } else {
          view.term.write(frame);
          paintScreen();
        }
      },
      close: () => {
        if (live()) {
          view.conn = null;
          view.phase = "ended";
          view.notice = conn.opened
            ? "The connection to the coordinator closed without saying why."
            : "The coordinator did not accept the connection. Your sign-in may have ended: sign in again and retry.";
          paintHead();
        }
      },
    });
    paintHead();
    paintScreen();
    if (box !== null && typeof box.focus === "function") {
      box.focus();
    }
    return undefined;
  };

  const pinSection = () => {
    const parts = [h("p", { class: "quiet", name: "ssh-pin" }, loaded.failure === null
      ? pinWords(view.keys.state) : `The coordinator did not say which SSH host keys are pinned: ${describeError(loaded.failure)}.`)];
    if (view.keys !== null && view.keys.fingerprints.length > 0) {
      parts.push(h("div", { class: "keys", name: "ssh-pinned" }, view.keys.fingerprints.map((f) => h("code", {}, f))));
    }
    const pinnable = view.keys !== null && view.keys.state !== "pinned";
    if (pinnable && loaded.reported.length === 0) {
      parts.push(h("p", { class: "quiet", name: "ssh-unreported" },
        "The host has not reported its SSH host keys yet; they can be pinned after its next heartbeat."));
    } else if (pinnable) {
      parts.push(h("div", { class: "keys", name: "ssh-reported" }, loaded.reported.map((f) => h("code", {}, f))));
      parts.push(action(ctx, "Pin these host keys", "pin-host-keys", async () => {
        const question = `Pin these SSH host keys for ${nodeId}? The coordinator will connect to it only if it presents one of them.\n${loaded.reported.join("\n")}`;
        if (!ctx.confirm(question)) {
          return "Host keys not pinned.";
        }
        await pin(loaded.reported);
        view.keys = { state: "pinned", fingerprints: loaded.reported.slice() };
        paintHead();
        return `SSH host keys of ${nodeId} pinned.`;
      }, { class: "primary" }));
    }
    return parts;
  };

  const headTree = () => {
    const busy = view.conn !== null;
    return [
      pinSection(),
      h("div", { class: "row" },
        busy
          ? action(ctx, "Close", "close-terminal", async () => {
            end("You closed the terminal.");
            return undefined;
          })
          : action(ctx, "Open a terminal", "open-terminal", async () => open(), { class: "primary" }),
        view.notice === "" ? null : h("span", { name: "ssh-status", role: "status" }, view.notice),
        view.phase === "open" ? pill("connected", "ok") : null),
    ];
  };

  const keydown = (event) => {
    if (view.phase !== "open") {
      return;
    }
    const bytes = encodeKey(event, view.term.appCursor);
    if (bytes !== null) {
      event.preventDefault();
      sendBytes(bytes);
    }
  };
  const paste = (event) => {
    if (view.phase !== "open") {
      return;
    }
    const data = event.clipboardData;
    const text = data && typeof data.getData === "function" ? data.getData("text") : "";
    event.preventDefault();
    if (text !== "") {
      sendBytes(encodePaste(text, view.term.bracketedPaste));
    }
  };

  return [h("div", { class: "card sshpanel" },
    h("h3", {}, "Remote SSH"),
    h("p", { class: "quiet" },
      "A terminal on this host through the coordinator, with a certificate made for you alone. It is audited, and it ends when you sign out."),
    // The page is drawn again when the operator changes tabs: each element is found again as it is
    // mounted, and shows what the terminal holds now.
    h("div", { name: "ssh-head", on: { mounted: (e) => { head = e.currentTarget; paintHead(); } } },
      h("div", {}, headTree())),
    h("div", {
      class: "termbox", name: "terminal", tabindex: 0, role: "application", "aria-label": "terminal",
      on: { mounted: (e) => { box = e.currentTarget; paintScreen(); }, keydown, paste },
    }))];
}
