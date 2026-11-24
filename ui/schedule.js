// The Update button and the Schedule form of the maintenance pages (docs/design/maintenance-updates.md,
// section 4.2): what a click asks of Maintenance.Schedule, in words and in the request it sends. This
// module calls nothing itself: the page gives each control `schedule(request)`, a function that makes
// the call, and `after()`, which reads the page again once a run is stored.
//
// - `updateButton` is the one-click way: update what is due, now, with the group's batch and drain
//   deadline. It needs a reason, asks once, and sends the request `updateRequest` builds. The reason
//   is typed in a field the page draws (`state.reason`), so one field can serve several buttons;
//   `updateControls` is a Reason field and the button together.
// - `scheduleForm` is the form for the rest: which kinds, which hosts, now or in a window (typed in the
//   operator's own time zone, `ctx.localOffset`, and shown again as UTC), a batch, a drain deadline,
//   and whether to take a group's last host. Its Check button asks the coordinator for the run as it
//   would be (a request without `confirm`) and the form shows each host it would take, each it leaves
//   out and why, and a host whose Xcode default would move; its Schedule button stores the run. The
//   form's state, from `newScheduleState`, is kept by the page across repaints. A due kind that is new
//   since the form was opened is ticked, as the others were. A window field left empty is said to
//   be not set.
// - `refusalWords` says a refusal of the Maintenance calls in words when the operator can act on it
//   (nothing due, the group's last host, a host already in a run...), and as its code and token
//   otherwise; the Update button, Check and Schedule here and the runs' buttons (`runs.js`) report it.
// - `localToUnix` and `unixToLocal` convert a wall-clock time and unix seconds through the zone's
//   offset at an instant; `skipWords`, `kindWords` and `dueKinds` say the coordinator's enums in words.

import { h, action, checkbox, describeError, enumLabel, field, fmtUnix, list, reasonProblem, select, table, trimmed } from "./view.js";
import { isDue } from "./updates.js";

const KIND = "MAINTENANCE_KIND_";
const SKIP = "MAINTENANCE_SKIP_";
const STEP = "MAINTENANCE_HOST_STEP_";

// The question's second sentence, after what is updated.
export const UPDATE_QUESTION_TAIL = "It drains, updates, restarts if needed, puts back.";

// The kinds, in the order the form lists them.
const KIND_ORDER = ["LINUX", "REBOOT", "MACOS", "XCODE"].map((name) => `${KIND}${name}`);

const KIND_WORDS = {
  [`${KIND}LINUX`]: "Linux updates",
  [`${KIND}REBOOT`]: "restart",
  [`${KIND}MACOS`]: "macOS",
  [`${KIND}XCODE`]: "Xcode",
};

const SKIP_WORDS = {
  [`${SKIP}NO_MAINTENANCE_V1`]: "older daemon: it does not announce maintenance-v1",
  [`${SKIP}HELD`]: "a package hold stands on it",
  [`${SKIP}EXCLUDED`]: "excluded from a package",
  [`${SKIP}MDM_CONFLICT`]: "mdm_conflict: a device-management setting hostkeeper must not update under",
  [`${SKIP}NO_UPDATE_ACCOUNT`]: "no update account (missing, not an administrator, or no Secure Token)",
  [`${SKIP}IN_PACKAGE_UPDATE`]: "a package update of it is outstanding",
  [`${SKIP}GROUP_CHANGED`]: "its host group changed while it waited",
  [`${SKIP}FILEVAULT_NOT_READY`]: "FileVault is on and not ready for an authenticated restart",
};

// A skip reason in words; "" for none (absent or unspecified), the lower-cased name for one this
// module does not know.
export function skipWords(value) {
  if (typeof value !== "string" || value === "" || value === `${SKIP}UNSPECIFIED`) {
    return "";
  }
  return SKIP_WORDS[value] || enumLabel(value, SKIP);
}

// The words for a refusal of Maintenance.Schedule, Cancel, RetryHost or PutBack that the operator can
// act on; any other is said as its code and token.
const REFUSALS = {
  nothing_due: "Nothing of what was asked is due on those hosts, or every host with something due would be skipped: no run was stored.",
  last_host: "That would take every host of the group at once: no run was stored. To take them all, use the Schedule form of the Maintenance page or the host page with \"Take the group's last host\" ticked.",
  host_in_maintenance: "A host it would take is already in a run that has not ended: no run was stored.",
  window_ended: "The window has already ended: no run was stored.",
  unknown_group: "The coordinator knows no host group of this name: no run was stored.",
  unknown_host: "The coordinator knows no host of this id: no run was stored.",
  unknown_run: "The coordinator knows no run of this id.",
  not_in_run: "This host is not in the run.",
  run_ended: "The run has already ended.",
  not_halted: "This host is not halted any more.",
  audit_unavailable: "The coordinator could not write the audit entry. Nothing was done; try again.",
};

// A refusal (or any error) of a Maintenance call in words: the sentence of `REFUSALS` for its
// token, else what `describeError` says of it.
export function refusalWords(error) {
  const reason = error && typeof error.reason === "string" ? error.reason : "";
  return Object.prototype.hasOwnProperty.call(REFUSALS, reason) ? REFUSALS[reason] : describeError(error);
}

// A kind of maintenance in words.
export function kindWords(kind) {
  return KIND_WORDS[kind] || enumLabel(kind, KIND);
}

// Whether a due item is the kernel's: a Linux item that needs a restart, or the restart `kernel`.
function isKernelItem(item) {
  return (item.kind === `${KIND}LINUX` && item.needsReboot === true)
    || (item.kind === `${KIND}REBOOT` && item.label === "kernel");
}

// The kinds due across `hosts`, each once: Linux, restart, macOS, Xcode, then any other in the order
// first seen. A host with a kernel item counts Linux updates and restart.
export function dueKinds(hosts) {
  const seen = [];
  const add = (kind) => {
    if (typeof kind === "string" && !seen.includes(kind)) {
      seen.push(kind);
    }
  };
  for (const host of hosts) {
    for (const item of list(host.maintenance, "due")) {
      add(item.kind);
      if (isKernelItem(item)) {
        add(`${KIND}LINUX`);
        add(`${KIND}REBOOT`);
      }
    }
  }
  return KIND_ORDER.filter((kind) => seen.includes(kind)).concat(seen.filter((kind) => !KIND_ORDER.includes(kind)));
}

// A `datetime-local` value as `[year, month, day, hour, minute, second]`, or null.
function parts(text) {
  const found = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/.exec(trimmed(text));
  return found === null ? null : found.slice(1).map((digits) => Number(digits || 0));
}

// The unix seconds of the wall-clock time `text` (`YYYY-MM-DDTHH:MM`, seconds optional) in the zone
// whose offset from UTC at an instant `offsetAt(unix)` says; null when the text is no such time (a
// bad shape, a month 13, a February 30). The offset is looked up twice, so a zone whose offset
// changes within the day lands on the right side of the change.
export function localToUnix(text, offsetAt) {
  const got = parts(text);
  if (got === null) {
    return null;
  }
  const [year, month, day, hour, minute, second] = got;
  const ms = Date.UTC(year, month - 1, day, hour, minute, second);
  const date = new Date(ms);
  if (Number.isNaN(ms) || date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 || date.getUTCDate() !== day
    || date.getUTCHours() !== hour || date.getUTCMinutes() !== minute || date.getUTCSeconds() !== second) {
    return null;
  }
  const wall = ms / 1000;
  const first = wall - offsetAt(wall);
  return wall - offsetAt(first);
}

// The wall-clock time of unix seconds `unix` in the zone `offsetAt` says, as `YYYY-MM-DDTHH:MM`.
export function unixToLocal(unix, offsetAt) {
  return new Date((unix + offsetAt(unix)) * 1000).toISOString().slice(0, 16);
}

// The request of the Update button: everything due, now, the group's batch and deadline, and never
// the group's last host.
export function updateRequest(target, reason) {
  const where = target.hostGroup !== undefined ? { hostGroup: target.hostGroup } : { hostIds: target.hostIds };
  return { ...where, reason: trimmed(reason), confirm: true };
}

// What a request takes, for the question: the host, the hosts counted, or the group's due hosts.
function whatOf(request) {
  if (typeof request.hostGroup === "string") {
    return `the due hosts of ${request.hostGroup}`;
  }
  const ids = list(request, "hostIds");
  return ids.length === 1 ? `the host ${ids[0]}` : `${ids.length} hosts`;
}

// What a stored run says to the operator: its id, how many hosts wait, and which were skipped.
function scheduled(answer) {
  const hosts = list(answer, "hosts");
  const skipped = hosts.filter((host) => host.step === `${STEP}SKIPPED`);
  const waiting = hosts.length - skipped.length;
  const id = answer && answer["run"] ? answer["run"]["id"] : "";
  const said = `Scheduled run ${id}: ${waiting} host${waiting === 1 ? "" : "s"} waiting`;
  if (skipped.length === 0) {
    return said;
  }
  const why = skipped.map((host) => `${host.hostId} (${skipWords(host.skippedBecause)})`).join(", ");
  return `${said}; skipped: ${why}`;
}

// The reason typed, trimmed, or the words that say it is missing or wrong; null when it is fine.
function reasonWords(reason) {
  const said = trimmed(reason);
  if (said === "") {
    return "Give a reason first.";
  }
  return reasonProblem(said);
}

// The Update button for `target` (`{ hostIds }` or `{ hostGroup }`). `env` is `{ ctx, schedule, state,
// after }`: `schedule(request)` makes the call, `state.reason` is the typed reason, `after()` (if
// given) reads the page again once a run is stored.
export function updateButton(env, target, label) {
  return action(env.ctx, label || "Update", "update", async () => {
    const problem = reasonWords(env.state.reason);
    if (problem !== null) {
      return problem;
    }
    const request = updateRequest(target, env.state.reason);
    if (!env.ctx.confirm(`Update ${whatOf(request)}? ${UPDATE_QUESTION_TAIL}`)) {
      return "Not updated.";
    }
    let answer;
    try {
      answer = await env.schedule(request);
    } catch (error) {
      return refusalWords(error);
    }
    if (env.after) {
      await env.after();
    }
    return scheduled(answer);
  }, { class: "primary" });
}

// A Reason field and the Update button for `target`, side by side.
export function updateControls(env, target, label) {
  return h("div", { name: "update-controls" },
    field("Reason", "update-reason", env.state, "reason", { maxlength: 256 }),
    updateButton(env, target, label));
}

// The state of a Schedule form: every due kind ticked, the default place, now, the hosts with
// something due ticked, nothing typed, no check made.
export function newScheduleState(scope) {
  const kinds = {};
  for (const kind of dueKinds(scope.hosts)) {
    kinds[kind] = true;
  }
  const picks = {};
  for (const host of scope.hosts) {
    picks[host.nodeId] = isDue(host);
  }
  return {
    kinds,
    where: scope.hostId ? "host" : scope["group"] ? "group" : "hosts",
    picks,
    when: "now",
    windowStart: "",
    windowEnd: "",
    batch: "",
    deadline: "",
    takeLast: false,
    reason: "",
    preview: null,
  };
}

// The hosts a check left out, by node id, with why.
function skippedByCheck(form) {
  const out = new Map();
  for (const host of form.preview || []) {
    if (host.step === `${STEP}SKIPPED`) {
      out.set(host.hostId, host.skippedBecause);
    }
  }
  return out;
}

// The places the form can name, as `[value, label]` pairs, in the order the select lists them.
function whereOptions(scope) {
  const options = [];
  if (scope.hostId) {
    options.push(["host", `this host: ${scope.hostId}`]);
  }
  if (scope["group"]) {
    options.push(["group", `every host of ${scope["group"]}`]);
  }
  options.push(["hosts", "the hosts ticked"]);
  return options;
}

// A whole number of 0 or more typed as digits, as a BigInt; null for anything else.
function digits(text) {
  return /^\d+$/.test(text) ? BigInt(text) : null;
}

// The most hosts one Maintenance.Schedule names.
const MOST_HOSTS = 32;

// The request the form describes without `confirm`, or `{ problem }` in words. `offsetAt` is the
// operator's zone (`ctx.localOffset`). A host the last check left out is named in the request of a
// check (`checking`), so the check says again whether it is left out, and not in the request that
// stores the run.
export function scheduleRequest(form, scope, offsetAt, checking) {
  const request = {};
  const due = dueKinds(scope.hosts);
  const ticked = due.filter((kind) => form.kinds[kind] !== false);
  if (due.length > 0 && ticked.length === 0) {
    return { problem: "Tick at least one kind." };
  }
  if (ticked.length < due.length) {
    request.kinds = ticked;
  }
  const where = whereOptions(scope).some(([value]) => value === form.where) ? form.where : whereOptions(scope)[0][0];
  if (where === "host") {
    request.hostIds = [scope.hostId];
  } else if (where === "group") {
    request.hostGroup = scope["group"];
  } else {
    const left = checking === true ? new Map() : skippedByCheck(form);
    const ids = scope.hosts.map((host) => host.nodeId).filter((id) => form.picks[id] === true && !left.has(id));
    if (ids.length === 0) {
      return { problem: "Tick at least one host." };
    }
    if (ids.length > MOST_HOSTS) {
      return { problem: `Tick at most ${MOST_HOSTS} hosts, or choose the group.` };
    }
    request.hostIds = ids;
  }
  if (form.when === "window") {
    const start = localToUnix(form.windowStart, offsetAt);
    if (start === null) {
      return { problem: "The window's start is not a date and time." };
    }
    const end = localToUnix(form.windowEnd, offsetAt);
    if (end === null) {
      return { problem: "The window's end is not a date and time." };
    }
    if (end <= start) {
      return { problem: "The window ends before it starts." };
    }
    request.windowStartUnix = String(start);
    request.windowEndUnix = String(end);
  }
  const batch = trimmed(form.batch);
  if (batch !== "") {
    const count = digits(batch);
    if (count === null || count < 1n || count > 10000n) {
      return { problem: "The batch is a whole number from 1 to 10000." };
    }
    request.batch = Number(count);
  }
  const deadline = trimmed(form.deadline);
  if (deadline !== "") {
    const secs = digits(deadline);
    if (secs === null || secs > 9223372036854775807n) {
      return { problem: "The drain deadline is a whole number of seconds, 0 or more." };
    }
    request.drainDeadlineSecs = secs.toString();
  }
  if (form.takeLast === true) {
    request.takeLastHost = true;
  }
  if (trimmed(form.reason) !== "") {
    request.reason = trimmed(form.reason);
  }
  return { request };
}

// The label of the batch field: the group's batch when known.
function batchLabel(policy) {
  return policy ? `Batch (the group's: ${policy.batch || 1})` : "Batch (the group's default)";
}

// The label of the drain deadline field.
function deadlineLabel(policy) {
  if (!policy) {
    return "Drain deadline, seconds (the group's default)";
  }
  const secs = Number(policy.drainDeadlineSecs || 0);
  return `Drain deadline, seconds (the group's: ${secs === 0 ? "none" : secs})`;
}

// A select whose change redraws the page, as it changes what the form shows.
function choosing(env, labelled) {
  const control = labelled.children[1];
  const bound = control.on.change;
  control.on.change = (e) => {
    bound(e);
    env.repaint();
  };
  return labelled;
}

// A window field whose committed change brings the line of converted times up to date, in the tree
// and in the page as it is drawn, without drawing the page again (which would take the focus from
// the field the operator moves to).
function committing(form, offsetAt, line, labelled) {
  labelled.children[1].on.change = (e) => {
    const said = windowWords(form, offsetAt);
    line.children = [{ text: said }];
    const target = e && e.target;
    const fieldset = target && typeof target.closest === "function" ? target.closest("fieldset") : null;
    const shown = fieldset ? fieldset.querySelector("[name=window-utc]") : null;
    if (shown) {
      shown.textContent = said;
    }
  };
  return labelled;
}

// The Xcode a host's report marks selected: its version, "" for none.
function selectedXcode(host) {
  const found = list(host && host.maintenance, "xcodes").find((xcode) => xcode.selected === true);
  return found && typeof found.version === "string" ? found.version : "";
}

// The lines for the hosts of the check whose turn moves the default Xcode off the one selected now.
function movesDefault(form, scope) {
  if (!scope.policy || !scope["group"]) {
    return [];
  }
  const want = typeof scope.policy.xcodeDefault === "string" && scope.policy.xcodeDefault !== ""
    ? scope.policy.xcodeDefault : "latest";
  const out = [];
  for (const turn of form.preview || []) {
    const host = scope.hosts.find((candidate) => candidate.nodeId === turn.hostId);
    if (turn.hostGroup !== scope["group"] || !list(turn, "kinds").includes(`${KIND}XCODE`)) {
      continue;
    }
    const selected = selectedXcode(host);
    if (selected !== want) {
      const to = want === "latest" ? "the latest kept" : want;
      out.push(h("p", { class: "note", name: "moves-default" },
        `${turn.hostId}: moves the default Xcode from ${selected || "none"} to ${to}`));
    }
  }
  return out;
}

// The table of what a check answered, and the lines of the default Xcode that would move.
function previewView(form, scope) {
  const rows = form.preview.map((turn) => h("tr", {},
    h("td", {}, turn.hostId),
    h("td", {}, turn.hostGroup || "-"),
    h("td", {}, list(turn, "kinds").map(kindWords).join(", ") || "-"),
    h("td", {}, skipWords(turn.skippedBecause) || "-")));
  const shown = table(["Host", "Group", "Kinds", "Left out"], rows);
  shown.attrs["name"] = "schedule-preview";
  return h("div", {}, shown, movesDefault(form, scope));
}

// The hosts of the "hosts ticked" choice: a checkbox each, and a row with its reason for a host the
// last check left out.
function hostPicks(form, scope) {
  const left = skippedByCheck(form);
  return scope.hosts.map((host) => {
    const id = host.nodeId;
    if (left.has(id)) {
      return h("p", { class: "note", name: "skipped-host" }, `${id}: left out (${skipWords(left.get(id))})`);
    }
    return checkbox(id, "pick", form.picks, id);
  });
}

// The window's converted times, as UTC, in words; a field left empty is not set.
function windowWords(form, offsetAt) {
  const said = (text) => {
    if (trimmed(text) === "") {
      return "not set";
    }
    const unix = localToUnix(text, offsetAt);
    return unix === null ? "not a date and time" : fmtUnix(unix);
  };
  return `Start ${said(form.windowStart)}; end ${said(form.windowEnd)}`;
}

// The Schedule form. `env` is `{ ctx, schedule, after, repaint, form }`; `scope` is `{ hosts, group,
// policy, hostId }`.
export function scheduleForm(env, scope) {
  const { ctx, form } = env;
  const options = whereOptions(scope);
  if (!options.some(([value]) => value === form.where)) {
    form.where = options[0][0];
  }
  const due = dueKinds(scope.hosts);
  for (const kind of due) {
    if (form.kinds[kind] === undefined) {
      form.kinds[kind] = true;
    }
  }
  const what = due.length === 0
    ? [h("p", { class: "note", name: "schedule-nothing-due" }, "Nothing is reported due.")]
    : due.map((kind) => checkbox(kindWords(kind), "kind", form.kinds, kind));
  const body = [
    h("legend", {}, "Schedule maintenance"),
    h("div", {}, h("strong", {}, "What"), what),
    choosing(env, select("Where", "where", form, "where", options)),
    form.where === "hosts" ? h("div", {}, hostPicks(form, scope)) : null,
    choosing(env, select("When", "when", form, "when", [["now", "now"], ["window", "in a window"]])),
  ];
  if (form.when === "window") {
    const line = h("p", { class: "note", name: "window-utc" }, windowWords(form, ctx.localOffset));
    body.push(
      committing(form, ctx.localOffset, line,
        field("Window start (your time)", "window-start", form, "windowStart", { type: "datetime-local" })),
      committing(form, ctx.localOffset, line,
        field("Window end (your time)", "window-end", form, "windowEnd", { type: "datetime-local" })),
      line);
  }
  body.push(
    field(batchLabel(scope.policy), "batch", form, "batch"),
    field(deadlineLabel(scope.policy), "drain-deadline", form, "deadline"),
    checkbox("Take the group's last host", "take-last-host", form, "takeLast"),
    field("Reason", "schedule-reason", form, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" }, action(ctx, "Check", "schedule-check", async () => {
      const built = scheduleRequest(form, scope, ctx.localOffset, true);
      if (built.problem !== undefined) {
        return built.problem;
      }
      const problem = trimmed(form.reason) === "" ? null : reasonProblem(trimmed(form.reason));
      if (problem !== null) {
        return problem;
      }
      let answer;
      try {
        answer = await env.schedule(built.request);
      } catch (error) {
        return refusalWords(error);
      }
      form.preview = list(answer, "hosts");
      env.repaint();
      const skipped = form.preview.filter((turn) => turn.step === `${STEP}SKIPPED`).length;
      return `Checked: ${form.preview.length - skipped} hosts would be taken, ${skipped} skipped.`;
    }),
    action(ctx, "Schedule", "schedule-confirm", async () => {
      const built = scheduleRequest(form, scope, ctx.localOffset);
      if (built.problem !== undefined) {
        return built.problem;
      }
      const problem = reasonWords(form.reason);
      if (problem !== null) {
        return problem;
      }
      const request = { ...built.request, confirm: true };
      if (!ctx.confirm(`Schedule maintenance of ${whatOf(request)}? ${UPDATE_QUESTION_TAIL}`)) {
        return "Not scheduled.";
      }
      let answer;
      try {
        answer = await env.schedule(request);
      } catch (error) {
        return refusalWords(error);
      }
      if (env.after) {
        await env.after();
      }
      return scheduled(answer);
    }, { class: "primary" })),
    form.preview === null ? null : previewView(form, scope));
  return h("fieldset", { name: "schedule-form" }, body);
}
