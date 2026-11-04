// Host: one host's services, holds, marks and last update, and what an operator may do to it.
// Calls: Hosts.Get; on a button, one of Hosts.Exclude, Hosts.Include, Hosts.Abort,
// Hosts.SetLabels, Services.Hold and Services.Unhold.

import {
  h, action, checkbox, enumLabel, errorView, field, fmtUnix, list, reasonProblem, scopeLabel, table,
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
          await ctx.api.call("hostkeeper.operator.v1.Hosts/Exclude",
            { service, nodeId, reason: reason(), abandon: form.abandon === true });
          ctx.refresh();
          return `Excluded ${nodeId} from ${service}.`;
        })),
        action(ctx, "Include", "include", async () => {
          await ctx.api.call("hostkeeper.operator.v1.Hosts/Include", { service, nodeId });
          ctx.refresh();
          return `Included ${nodeId} in ${service}.`;
        }),
        action(ctx, "Abort the running update", "abort", needReason(async () => {
          const reply = await ctx.api.call("hostkeeper.operator.v1.Hosts/Abort", { nodeId, service, reason: reason() });
          ctx.refresh();
          return reply.queued === true
            ? `Abort of ${service} queued for ${nodeId}'s next heartbeat.`
            : `Nothing to abort: ${nodeId} runs nothing of ${service}, so no abort was queued.`;
        })),
        action(ctx, "Hold here", "hold", needReason(async () => {
          await ctx.api.call("hostkeeper.operator.v1.Services/Hold",
            { scope: { host: nodeId }, service, reason: reason() });
          ctx.refresh();
          return `Held ${service} on ${nodeId}.`;
        })),
        action(ctx, "Release the hold here", "unhold", needReason(async () => {
          await ctx.api.call("hostkeeper.operator.v1.Services/Unhold",
            { scope: { host: nodeId }, service, reason: reason() });
          ctx.refresh();
          return `Released the hold of ${service} on ${nodeId}.`;
        })))));
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
  try {
    host = (await ctx.api.call("hostkeeper.operator.v1.Hosts/Get", { nodeId })).host || {};
  } catch (error) {
    return errorView(`Host ${nodeId}`, error);
  }
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
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
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
        await ctx.api.call("hostkeeper.operator.v1.Hosts/SetLabels", { nodeId, labels });
        ctx.refresh();
        return `Labels of ${nodeId} set.`;
      })),
    list(host, "services").map((view) => serviceSection(ctx, nodeId, view)));
}
