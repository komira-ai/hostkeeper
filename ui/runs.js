// The runs of the Maintenance page, in words, from what Maintenance.List and Maintenance.Get
// answer (docs/design/maintenance-updates.md, section 4.2): each run with its state, where it
// was scheduled, its window, batch, reason and who scheduled it, and a table of its hosts' turns
// (step, since when, and why a host was left out, halted or deferred). The page hands this module
// the calls as functions (`cancel`, `retryHost`, `putBack`) and a function that reads again; the
// module calls nothing itself.
//
// A host in its restart (`rebooting`, `not back`) has a button to its Console, or to its Desktop
// when it is a Mac. A halted host has Put back, and Retry while its run is not ended. A run not
// ended has Cancel. Each of these needs the run's typed reason and asks once. There is no pause.

import { action, describeError, enumLabel, field, fmtUnix, h, list, reasonProblem, table, trimmed } from "./view.js";
import { skipWords } from "./schedule.js";

const STEP = "MAINTENANCE_HOST_STEP_";
const STATE = "MAINTENANCE_STATE_";
const KIND = "MAINTENANCE_KIND_";

// A host's step in words.
export const STEP_WORDS = {
  [`${STEP}WAITING`]: "waiting",
  [`${STEP}STAGING`]: "staging",
  [`${STEP}DRAINING`]: "draining",
  [`${STEP}APPLYING`]: "applying",
  [`${STEP}REBOOTING`]: "rebooting",
  [`${STEP}CHECKING`]: "checking",
  [`${STEP}PUTTING_BACK`]: "putting back",
  [`${STEP}DONE`]: "done",
  [`${STEP}DEFERRED`]: "deferred",
  [`${STEP}HALTED`]: "halted",
  [`${STEP}NOT_BACK`]: "not back",
  [`${STEP}NOT_REACHED`]: "not reached",
  [`${STEP}SKIPPED`]: "skipped",
};

const STATE_WORDS = {
  [`${STATE}SCHEDULED`]: "scheduled",
  [`${STATE}RUNNING`]: "running",
  [`${STATE}HALTED`]: "halted",
  [`${STATE}DONE`]: "done",
  [`${STATE}CANCELLED`]: "cancelled",
  [`${STATE}WINDOW_ENDED`]: "window ended",
};

// A run state in words; a state this module does not know is its lower-case name.
export function stateWords(state) {
  return STATE_WORDS[state] || enumLabel(state, STATE);
}

// Whether the run has ended: done, cancelled or its window ended.
export function isEnded(run) {
  return [`${STATE}DONE`, `${STATE}CANCELLED`, `${STATE}WINDOW_ENDED`].includes((run || {}).state);
}

// A host's step in words, with what goes with it: since when a drain waits, why a turn halted or
// was deferred, why a host was left out.
export function stepCell(host) {
  const step = host.step;
  const words = STEP_WORDS[step] || enumLabel(step, STEP);
  if (step === `${STEP}DRAINING`) {
    return `${words} since ${fmtUnix(host.sinceUnix)}`;
  }
  if (step === `${STEP}HALTED` || step === `${STEP}DEFERRED`) {
    const reason = typeof host.reason === "string" ? host.reason : "";
    return reason === "" ? words : `${words}: ${reason}`;
  }
  if (step === `${STEP}SKIPPED`) {
    const why = skipWords(host.skippedBecause);
    return why === "" ? words : `${words}: ${why}`;
  }
  return words;
}

// Whether the turn is of a Mac: its report says macOS; with no report, its kinds do.
export function isMac(turn, host) {
  const report = host && host.maintenance;
  if (report && report.host && typeof report.host.osName === "string") {
    return report.host.osName === "macOS";
  }
  return list(turn, "kinds").some((kind) => kind === `${KIND}MACOS` || kind === `${KIND}XCODE`);
}

// Where the run was scheduled, in words.
function whereWords(run) {
  if (typeof run.hostGroup === "string" && run.hostGroup !== "") {
    return `group ${run.hostGroup}`;
  }
  const ids = list(run, "hostIds");
  return ids.length === 0 ? "-" : `hosts ${ids.join(", ")}`;
}

// The run's window in words: from `now` or its start, to its end or "no end".
function windowWords(run) {
  const start = Number(run.windowStartUnix || 0) > 0 ? fmtUnix(run.windowStartUnix) : "now";
  const end = Number(run.windowEndUnix || 0) > 0 ? fmtUnix(run.windowEndUnix) : "no end";
  return `${start} to ${end}`;
}

// The line that heads a run.
function headline(run) {
  const batch = run.batch === undefined ? "the group's" : String(run.batch);
  const parts = [
    `Run ${run.id}`,
    stateWords(run.state),
    whereWords(run),
    `window ${windowWords(run)}`,
    `batch ${batch}`,
  ];
  if (typeof run.reason === "string" && run.reason !== "") {
    parts.push(`reason: ${run.reason}`);
  }
  if (typeof run.byName === "string" && run.byName !== "") {
    parts.push(`by ${run.byName}`);
  }
  return parts.join("; ");
}

// The button from a host in its restart to its Console, or to its Desktop for a Mac.
function restartLink(env, turn) {
  const mac = isMac(turn, env.hosts.get(turn.hostId));
  return h("button", {
    type: "button",
    class: "linklike",
    name: mac ? "open-desktop" : "open-console",
    on: { click: () => env.ctx.navigate(mac ? "desktop" : "serialconsole", { nodeId: turn.hostId }) },
  }, mac ? "Desktop" : "Console");
}

// The typed reason of a run, or why it cannot be used: `{ reason }` or `{ problem }`.
function reasonOf(env, run) {
  const reason = trimmed(env.state[run.id]);
  if (reason === "") {
    return { problem: "Give a reason first." };
  }
  const problem = reasonProblem(reason);
  return problem === null ? { reason } : { problem };
}

// Put back and Retry of a halted host: each needs the run's reason and asks once.
function hostButtons(env, run, turn) {
  const { ctx } = env;
  const host = turn.hostId;
  const buttons = [action(ctx, "Put back", "put-back-host", async () => {
    const given = reasonOf(env, run);
    if (given.problem !== undefined) {
      return given.problem;
    }
    if (!ctx.confirm(`Put ${host} back? Its drained services' resume runs, the update stays as far as it got, and the run goes on.`)) {
      return "Not put back.";
    }
    await env.putBack({ runId: run.id, hostId: host, reason: given.reason });
    await env.after();
    return `Put ${host} back in run ${run.id}.`;
  })];
  if (!isEnded(run)) {
    buttons.unshift(action(ctx, "Retry", "retry-host", async () => {
      const given = reasonOf(env, run);
      if (given.problem !== undefined) {
        return given.problem;
      }
      if (!ctx.confirm(`Retry ${host} from the step that failed? It is drained again if needed and the update goes on.`)) {
        return "Not retried.";
      }
      await env.retryHost({ runId: run.id, hostId: host, reason: given.reason });
      await env.after();
      return `Retried ${host} in run ${run.id}.`;
    }));
  }
  return buttons;
}

// The actions cell of a host's row.
function actionsCell(env, run, turn) {
  const step = turn.step;
  if (step === `${STEP}REBOOTING` || step === `${STEP}NOT_BACK`) {
    return h("td", {}, restartLink(env, turn));
  }
  if (step === `${STEP}HALTED`) {
    return h("td", {}, hostButtons(env, run, turn));
  }
  return h("td", {});
}

// One host's row.
function hostRow(env, run, turn) {
  return h("tr", { name: "run-host" },
    h("td", {}, turn.hostId),
    h("td", {}, turn.hostGroup || "-"),
    h("td", {}, stepCell(turn)),
    h("td", {}, fmtUnix(turn.sinceUnix)),
    actionsCell(env, run, turn));
}

// The reason field and Cancel of a run: Cancel only while the run is not ended; an ended run with
// a halted host keeps the field, for Put back.
function controls(env, run, turns) {
  const { ctx } = env;
  const halted = turns.some((turn) => turn.step === `${STEP}HALTED`);
  if (isEnded(run) && !halted) {
    return [];
  }
  const out = [field("Reason", "run-reason", env.state, run.id, { maxlength: 256 })];
  if (!isEnded(run)) {
    out.push(action(ctx, "Cancel", "cancel-run", async () => {
      const given = reasonOf(env, run);
      if (given.problem !== undefined) {
        return given.problem;
      }
      if (!ctx.confirm(`Cancel run ${run.id}? No new host starts; a host in its turn finishes.`)) {
        return "Not cancelled.";
      }
      await env.cancel({ runId: run.id, reason: given.reason });
      await env.after();
      return `Cancelled run ${run.id}.`;
    }));
  }
  return out;
}

// One run: its headline, its hosts, its controls.
function runView(env, entry) {
  const { run } = entry;
  const turns = Array.isArray(entry.hosts) ? entry.hosts : [];
  const halted = list(run, "haltedGroups");
  return h("div", { class: "run", name: "maintenance-run" },
    h("p", {}, headline(run)),
    halted.length === 0 ? [] : h("p", { class: "warning" }, `halted in: ${halted.join(", ")}`),
    entry.hosts === null || entry.hosts === undefined
      ? h("p", { class: "warning", role: "alert" }, `Its hosts could not be read: ${describeError(entry.failure)}`)
      : table(["Host", "Group", "Step", "Since", ""], turns.map((turn) => hostRow(env, run, turn))),
    controls(env, run, turns));
}

// The runs section: `runs` is `[{ run, hosts }]` (`hosts` null when Maintenance.Get failed, with
// `failure` its error); `failure` is the error of Maintenance.List, or null.
export function runsSection(env, runs, failure) {
  const head = h("h3", {}, "Runs");
  if (failure) {
    return h("section", { class: "card", name: "maintenance-runs" }, head,
      h("p", { class: "warning", role: "alert", name: "runs-failure" },
        `The runs could not be read (${describeError(failure)}).`));
  }
  if (runs.length === 0) {
    return h("section", { class: "card", name: "maintenance-runs" }, head,
      h("p", { class: "quiet" }, "No maintenance run."));
  }
  return h("section", { class: "card", name: "maintenance-runs" }, head, runs.map((entry) => runView(env, entry)));
}
