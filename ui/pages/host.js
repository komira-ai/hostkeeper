// Host: one host's services, holds, marks and last update, the host-inventory record its
// enrollment matched (linked to the Inventory page) with that record's data MACs, BMC MAC and
// serial, when it enrolled, its certificate and its Sigstore trusted root as its last heartbeat
// said them, and what an operator may do to it. The page reads the time from `ctx.now()` (unix
// seconds), once per rendering, to say how soon the certificate ends.
// Calls: Hosts.Get, then Services.List for the services assigned to the host (it is called even
// when Hosts.Get fails, and each failure is shown as is), then, when a service is assigned to the
// host, Audit.List of the host's entries, page by page, to mark a service whose last uninstall was
// cancelled; on
// a button, one of Hosts.Exclude, Hosts.Include, Hosts.Abort, Hosts.SetLabels, Services.Hold,
// Services.Unhold and, after a confirm, Hosts.Retire. Retire is offered when Hosts.Get fails too,
// for an enrolled host the coordinator has not heard from since it started (a dead one).

import {
  h, action, checkbox, describeError, enumLabel, errorView, field, fmtUnix, list, reasonProblem, scopeLabel, table,
  trimmed,
} from "../view.js";

// A label key or value, as SetLabels takes it: 1 to 32 bytes of `[a-z0-9-]`.
const LABEL = /^[a-z0-9-]{1,32}$/;
// The most labels a host may have.
const MAX_LABELS = 8;

// `key=value` pairs separated by commas or spaces, as SetLabels takes them: `{labels}`, or
// `{problem}`, a sentence saying why not. Each key and value must follow the label grammar (so
// no key reaches the object's prototype), no key may be given twice, and there may be at most 8.
// An empty text is the empty set, which removes every label.
export function parseLabels(text) {
  const labels = Object.create(null);
  let count = 0;
  for (const pair of trimmed(text).split(/[\s,]+/).filter((p) => p !== "")) {
    const at = pair.indexOf("=");
    const key = pair.slice(0, at);
    const value = pair.slice(at + 1);
    if (at < 0 || !LABEL.test(key) || !LABEL.test(value)) {
      return { problem: `Labels are key=value pairs, each key and value 1 to 32 of a-z, 0-9 and -: not ${pair}.` };
    }
    if (key in labels) {
      return { problem: `The label ${key} is given twice.` };
    }
    labels[key] = value;
    count += 1;
  }
  if (count > MAX_LABELS) {
    return { problem: `A host has at most ${MAX_LABELS} labels, not ${count}.` };
  }
  return { labels };
}

// Less than this many seconds of a certificate left is a warning: 48 hours.
export const CERT_WARN_SECS = 48 * 3600;

// The state of a certificate whose last valid second is `notAfter` (unix seconds, a string or a
// number as Hosts.Get answers it; absent when the host said none) when the clock reads `now`
// (unix seconds): "unknown" without an end, "expired" once `now` is past the end, "soon" while
// less than CERT_WARN_SECS are left, and "ok" otherwise.
export function certState(notAfter, now) {
  const end = Number(notAfter);
  if (notAfter === undefined || notAfter === null || notAfter === "" || !Number.isFinite(end) || end <= 0) {
    return "unknown";
  }
  if (now > end) {
    return "expired";
  }
  return end - now < CERT_WARN_SECS ? "soon" : "ok";
}

// The certificate of `host` as its last heartbeat said it, at `now`: its end, the last renewal
// error, the count of heartbeats whose clock was off, and a warning when the end is near or past
// or a renewal error is reported.
function certificate(host, now) {
  const state = certState(host.certNotAfterUnix, now);
  const error = typeof host.lastRenewError === "string" && host.lastRenewError !== "" ? host.lastRenewError : null;
  const ends = fmtUnix(host.certNotAfterUnix);
  const warnings = [];
  if (state === "expired") {
    warnings.push(`The certificate expired at ${ends}.`);
  } else if (state === "soon") {
    warnings.push(`The certificate ends in less than 48 hours, at ${ends}.`);
  }
  if (error !== null) {
    warnings.push(`The last renewal failed: ${error}`);
  }
  const skewed = host.clockSkewBeats === undefined ? "0" : String(host.clockSkewBeats);
  return h("section", { class: "certificate" },
    h("h3", {}, "Certificate"),
    table(["", ""], [
      ["Ends", state === "unknown" ? "not reported" : ends],
      ["Last renewal error", error || "-"],
      ["Heartbeats with the clock off", skewed],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    warnings.map((text) => h("p", { class: "warning", role: "alert" }, text)));
}

// Seconds in a day, the unit a trusted root's age is shown in.
const DAY_SECS = 86400;

// The state of a trusted root whose reason Hosts.Get answers as `reason` (`trustedRootReason`,
// the enum's name; absent when unspecified): "stale", "missing" or "unparseable" when it needs
// attention, "ok" when it needs none, and "unknown" when the host's last heartbeat said nothing
// of it (absent, unspecified, or a value this page does not know).
export function rootState(reason) {
  switch (reason) {
    case "TRUSTED_ROOT_REASON_NONE":
      return "ok";
    case "TRUSTED_ROOT_REASON_STALE":
      return "stale";
    case "TRUSTED_ROOT_REASON_MISSING":
      return "missing";
    case "TRUSTED_ROOT_REASON_UNPARSEABLE":
      return "unparseable";
    default:
      return "unknown";
  }
}

// What the page says of each state of a trusted root, and the warning of each that needs
// attention.
const ROOT_SAYS = {
  ok: ["needs no attention", null],
  stale: ["stale", "The Sigstore trusted root is stale: package checks go on with it, and refuse every package "
    + "signed after a key rotation it does not hold. Update hostkeeper on the host, or copy a current root to it."],
  missing: ["missing", "The host has no Sigstore trusted root: every package check on it fails."],
  unparseable: ["unparseable", "The host's Sigstore trusted root cannot be read: every package check on it fails."],
  unknown: ["not reported", null],
};

// The age of a trusted root as Hosts.Get answers it (`trustedRootAgeSecs`, a string or a number;
// absent when the host said none): whole days and the seconds, or "not reported".
export function rootAge(secs) {
  const age = Number(secs);
  if (secs === undefined || secs === null || secs === "" || !Number.isFinite(age) || age < 0) {
    return "not reported";
  }
  return `${Math.floor(age / DAY_SECS)} days (${age} s)`;
}

// The trusted root of `host` as its last heartbeat said it: its state and age, and a warning when
// it needs attention.
function trustedRoot(host) {
  const [said, warning] = ROOT_SAYS[rootState(host.trustedRootReason)];
  return h("section", { class: "trusted-root" },
    h("h3", {}, "Trusted root"),
    table(["", ""], [
      ["State", said],
      ["Age", rootAge(host.trustedRootAgeSecs)],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    warning === null ? [] : [h("p", { class: "warning", role: "alert" }, warning)]);
}

function lastUpdate(result) {
  if (!result) {
    return h("p", {}, "No update reported yet.");
  }
  const facts = [
    ["Target", result["target"] || "-"],
    ["Verdict", enumLabel(result.outcome, "OUTCOME_") + (result.forced === true ? " (forced)" : "")],
    ["Phase", enumLabel(result.phase, "FAILURE_PHASE_")],
    ["Class", enumLabel(result.class, "FAILURE_CLASS_")],
    ["Reason", result.reason || "-"],
    ["Detail", result.detail || "-"],
    ["Try", result.orderTry === undefined ? "-" : String(result.orderTry)],
    ["Order", result.orderId || "-"],
    ["Started", fmtUnix(result.startedUnix)],
  ];
  return h("div", { class: "update" },
    table(["Last update", ""], facts.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    h("pre", { class: "log" }, (result.logTail || []).map((line) => `${line}\n`)));
}

function holds(view) {
  const list = view.holds || [];
  if (list.length === 0) {
    return h("p", {}, "Not held.");
  }
  return h("ul", {}, list.map((hold) => h("li", {},
    `Held (${scopeLabel(hold.scope)}) by ${hold.setBy || "-"} at ${fmtUnix(hold.setUnix)}: `,
    hold.reason || "-")));
}

function serviceSection(ctx, nodeId, view) {
  const service = view.service;
  const form = { reason: "", abandon: false };
  const needReason = (run) => async () => {
    if (trimmed(form.reason) === "") {
      return "Give a reason first.";
    }
    return reasonProblem(trimmed(form.reason)) || run();
  };
  const reason = () => trimmed(form.reason);
  return h("section", { class: "service" },
    h("h3", {}, service),
    table(["", ""], [
      ["Installed version", typeof view.installedVersion === "string" ? view.installedVersion : "not installed"],
      ["Health", enumLabel(view["health"], "HEALTH_")],
      ["Up", view.up === true ? "yes" : "no"],
      ["Update slot", view.holdsSlot === true ? "holds the update slot" : "-"],
      ["Gate", enumLabel(view.gateReason, "GATE_REASON_")],
      ["Membership", enumLabel(view.membership, "MEMBERSHIP_")],
      ["Excluded", typeof view.excludedReason === "string" ? `yes: ${view.excludedReason}` : "no"],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    holds(view),
    lastUpdate(view.lastUpdate),
    h("fieldset", {},
      h("legend", {}, `Act on ${service} here`),
      field("Reason", "reason", form, "reason", { maxlength: 256 }),
      checkbox("Abandon: also free the update slot this host holds for the service (the host is gone)",
        "abandon", form, "abandon"),
      h("div", { class: "buttons" },
        action(ctx, "Exclude", "exclude", needReason(async () => {
          if (form.abandon === true
            && !ctx.confirm(`Exclude ${nodeId} from ${service} and free the update slot it holds (abandon)?`)) {
            return "Not excluded.";
          }
          await ctx.api.hosts.exclude(
            { service, nodeId, reason: reason(), abandon: form.abandon === true });
          ctx.refresh();
          return `Excluded ${nodeId} from ${service}.`;
        })),
        action(ctx, "Include", "include", async () => {
          await ctx.api.hosts.include({ service, nodeId });
          ctx.refresh();
          return `Included ${nodeId} in ${service}.`;
        }),
        action(ctx, "Abort the running update", "abort", needReason(async () => {
          const reply = await ctx.api.hosts.abort({ nodeId, service, reason: reason() });
          ctx.refresh();
          return reply.queued === true
            ? `Abort of ${service} queued for ${nodeId}'s next heartbeat.`
            : `Nothing to abort: ${nodeId} runs nothing of ${service}, so no abort was queued.`;
        })),
        action(ctx, "Hold here", "hold", needReason(async () => {
          await ctx.api.services.hold(
            { scope: { host: nodeId }, service, reason: reason() });
          ctx.refresh();
          return `Held ${service} on ${nodeId}.`;
        })),
        action(ctx, "Release the hold here", "unhold", needReason(async () => {
          await ctx.api.services.unhold(
            { scope: { host: nodeId }, service, reason: reason() });
          ctx.refresh();
          return `Released the hold of ${service} on ${nodeId}.`;
        })))));
}

// The audit actions that end an uninstall: the host reported the service gone, or the service
// was assigned to the host again before it did.
const UNINSTALLED = "uninstalled";
const UNINSTALL_CANCELLED = "uninstall_cancelled";

// The services whose last uninstall on `nodeId` was cancelled, from the entries of Audit.List
// (newest last): a later `uninstalled` of the same service ends the mark.
function cancelledUninstalls(entries, nodeId) {
  const last = new Map();
  for (const entry of entries) {
    if (entry.nodeId === nodeId && typeof entry.service === "string"
      && (entry.action === UNINSTALLED || entry.action === UNINSTALL_CANCELLED)) {
      last.set(entry.service, entry.action);
    }
  }
  return new Set(Array.from(last).filter(([, action]) => action === UNINSTALL_CANCELLED).map(([s]) => s));
}

// The rows of the services Services.List assigns to `nodeId`, or orders it to uninstall.
// `cancelled` names the assigned services whose last uninstall was cancelled.
function assignedRows(entries, nodeId, cancelled) {
  const rows = [];
  for (const entry of entries) {
    const desired = typeof entry.desiredVersion === "string" ? entry.desiredVersion : null;
    const mine = list(entry, "assigned").find((a) => a.nodeId === nodeId);
    if (mine) {
      const direct = list(entry, "assignments").some((given) => given["host"] === nodeId && given.removed !== true);
      const installed = typeof mine.installedVersion === "string" ? mine.installedVersion : null;
      let state = "behind";
      if (desired === null) {
        state = "-";
      } else if (installed === null) {
        state = "not installed";
      } else if (installed === desired) {
        state = "at desired";
      }
      if (cancelled.has(entry.service)) {
        state = `${state}; uninstall cancelled`;
      }
      rows.push([entry.service, direct ? "this host" : `role ${mine.role}`, installed || "not installed",
        desired || "none set", state, mine.minVersion || "-"]);
    } else if (list(entry, "uninstalling").includes(nodeId)) {
      rows.push([entry.service, "-", "-", desired || "none set", "uninstalling", "-"]);
    }
  }
  return rows;
}

// Whether Services.List assigns any service to `nodeId`.
function assignsAny(entries, nodeId) {
  return entries.some((entry) => list(entry, "assigned").some((a) => a.nodeId === nodeId));
}

// The host's ended uninstalls, read only when a service is assigned to it (a cancelled uninstall
// is one whose service was assigned again): `{cancelled}`, a set of services, or `{cancelled,
// error}` when Audit.List failed, with none marked. Audit.List answers the oldest entries first,
// a page at a time, so the pages are read from the first, each from the `nextSeq` of the one
// before, until a page is empty or `nextSeq` does not move.
async function uninstallEnds(ctx, listing, nodeId) {
  if (listing.error || !assignsAny(listing.entries, nodeId)) {
    return { cancelled: new Set() };
  }
  const entries = [];
  try {
    let request = { nodeId };
    for (;;) {
      const page = await ctx.api.audit.list(request);
      const got = list(page, "entries");
      entries.push(...got);
      const next = typeof page.nextSeq === "string" ? page.nextSeq : "0";
      if (got.length === 0 || next === (request.sinceSeq || "0")) {
        break;
      }
      request = { nodeId, sinceSeq: next };
    }
  } catch (error) {
    return { cancelled: new Set(), error };
  }
  return { cancelled: cancelledUninstalls(entries, nodeId) };
}

// `listing` is `{entries}` of Services.List, or `{error}` when it failed; `ends` is what
// `uninstallEnds` read.
function assignedSection(listing, nodeId, ends) {
  if (listing.error) {
    return h("section", { class: "assigned" }, h("h3", {}, "Assigned services"),
      h("p", { class: "error", role: "alert" }, describeError(listing.error)));
  }
  const rows = assignedRows(listing.entries, nodeId, ends.cancelled);
  return h("section", { class: "assigned" }, h("h3", {}, "Assigned services"),
    rows.length === 0
      ? h("p", {}, "No service is assigned to this host.")
      : table(["Service", "Through", "Installed", "Desired", "State", "Minimum"],
        rows.map((cells) => h("tr", {}, cells.map((c) => h("td", {}, c))))),
    ends.error
      ? h("p", { class: "note" }, `Cancelled uninstalls could not be read: ${describeError(ends.error)}`)
      : null);
}

// The facts the host inventory gives of `host`: the record its enrollment matched, as a link to
// the Inventory page's enrolled records with that record marked, the record's MACs and serial,
// when the host's approval was made, and when the coordinator first heard the host since then
// ("not yet" for an enrolled host it has not heard since). Each is "-" when there is none.
function inventoryFacts(ctx, host) {
  const recordId = typeof host.inventoryRecordId === "string" ? host.inventoryRecordId : "";
  const record = recordId === "" ? "none matched" : h("button", {
    type: "button", class: "linklike", name: "open-record",
    on: { click: () => ctx.navigate("inventory", { enrolled: "true", record: recordId }) },
  }, recordId);
  const enrolled = fmtUnix(host.enrolledUnix) !== "-";
  const firstHeard = fmtUnix(host.firstHeartbeatUnix);
  return [
    ["Inventory record", record],
    ["Data MACs", list(host, "dataMacs").join(", ") || "-"],
    ["BMC MAC", host.bmcMac || "-"],
    ["Serial", host.serial || "-"],
    ["Enrolled", enrolled ? fmtUnix(host.enrolledUnix) : "not enrolled"],
    ["First heartbeat", firstHeard === "-" && enrolled ? "not yet" : firstHeard],
  ];
}

function chooser(ctx) {
  const form = { nodeId: "" };
  return h("section", { class: "page" },
    h("h2", {}, "Host"),
    h("p", {}, "Choose a host on the Fleet page, or name one."),
    field("Host id", "node-id", form, "nodeId"),
    action(ctx, "Open", "open", async () => {
      if (trimmed(form.nodeId) === "") {
        return "Name a host first.";
      }
      ctx.navigate("host", { nodeId: trimmed(form.nodeId) });
      return undefined;
    }));
}

export async function render(ctx) {
  const nodeId = ctx.params && typeof ctx.params.nodeId === "string" ? ctx.params.nodeId : "";
  if (nodeId === "") {
    return chooser(ctx);
  }
  let host;
  let failure = null;
  try {
    host = (await ctx.api.hosts.get({ nodeId })).host || {};
  } catch (error) {
    failure = error;
  }
  let listing;
  try {
    listing = { entries: list(await ctx.api.services.list({}), "services") };
  } catch (error) {
    listing = { error };
  }
  const ends = await uninstallEnds(ctx, listing, nodeId);
  if (failure !== null) {
    return h("section", { class: "page" },
      errorView(`Host ${nodeId}`, failure).children, assignedSection(listing, nodeId, ends),
      retireSection(ctx, nodeId));
  }
  const now = ctx.now();
  const agent = host.heartbeat && host.heartbeat.agent;
  const labelsForm = {
    labels: Object.entries(host.labels || {}).map(([k, v]) => `${k}=${v}`).join(", "),
  };
  return h("section", { class: "page" },
    h("h2", {}, `Host ${host.nodeId || nodeId}`),
    table(["", ""], [
      ["Role", host.role || "-"],
      ["Heartbeat", host["live"] === true ? "live" : "silent"],
      ["Last heartbeat", fmtUnix(host.lastHeartbeatUnix)],
      ["Agent", agent ? `${agent["name"] || "-"} ${agent.version || ""}` : "-"],
      ["Labels", labelsForm.labels || "-"],
      ["Excluded from", (host.exclusions || []).map((x) => `${x.service} (${x.reason || "-"})`).join(", ") || "-"],
      ...inventoryFacts(ctx, host),
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    certificate(host, now),
    trustedRoot(host),
    h("fieldset", {},
      h("legend", {}, "Labels"),
      field("Labels (key=value, separated by commas)", "labels", labelsForm, "labels"),
      action(ctx, "Set labels", "set-labels", async () => {
        const parsed = parseLabels(labelsForm.labels);
        if (parsed.problem) {
          return parsed.problem;
        }
        const { labels } = parsed;
        const had = Object.keys(host.labels || {}).length;
        if (Object.keys(labels).length === 0 && had > 0
          && !ctx.confirm(`Remove every label of ${nodeId}?`)) {
          return "Labels not changed.";
        }
        await ctx.api.hosts.setLabels({ nodeId, labels });
        ctx.refresh();
        return `Labels of ${nodeId} set.`;
      })),
    assignedSection(listing, nodeId, ends),
    list(host, "services").map((view) => serviceSection(ctx, nodeId, view)),
    retireSection(ctx, nodeId));
}

// Retire: the reason is optional; the host is refused from then on, so it asks first. The answer
// says whether an update in flight still holds the host's slot.
function retireSection(ctx, nodeId) {
  const form = { reason: "" };
  return h("fieldset", {},
    h("legend", {}, "Retire"),
    h("p", { class: "warning" },
      "Retire revokes the host: its heartbeats and renewals are refused, and its own service assignments are "
      + "dropped. Nothing is uninstalled."),
    field("Reason (optional)", "retire-reason", form, "reason", { maxlength: 256 }),
    action(ctx, "Retire this host", "retire", async () => {
      const reason = trimmed(form.reason);
      const problem = reason === "" ? null : reasonProblem(reason);
      if (problem) {
        return problem;
      }
      if (!ctx.confirm(`Retire ${nodeId}? Its heartbeats and renewals are refused from then on.`)) {
        return "Not retired.";
      }
      const reply = await ctx.api.hosts.retire({ nodeId, reason });
      ctx.refresh();
      return reply.updateInFlight === true
        ? `Retired ${nodeId}: its update slot is freed when it reports its update final.`
        : `Retired ${nodeId}.`;
    }));
}
