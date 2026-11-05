// Fleet: every host the coordinator has heard from, and the conditions that need a person.
// Calls: Hosts.List and Releases.Status, nothing else.

import { h, enumLabel, errorView, fmtUnix, list, table } from "../view.js";

// The conditions of the design's section 6.7 that the API returns, and a held host on a bad
// version (section 5.4), as `{kind, text}` in the order found. A failed canary is not listed:
// the API does not say which host is a service's canary.
export function fleetAlerts(hosts, status) {
  const alerts = [];
  const services = list(status, "services");
  for (const svc of services) {
    const halt = svc.marks && svc.marks.halt;
    const halted = halt || (svc.convergence && svc.convergence.state === "CONVERGENCE_STATE_HALTED");
    if (halted) {
      const by = halt && halt.nodeId ? ` by ${halt.nodeId}` : "";
      const check = halt && halt.check ? `, check ${halt.check}` : "";
      alerts.push({ kind: "halt", text: `${svc.service} is halted${by}${check}` });
    }
  }
  const bad = new Map(services.map((svc) => [svc.service, (svc.marks && svc.marks.bad) || []]));
  for (const host of hosts) {
    const views = list(host, "services");
    const reasons = new Set(views.map((view) => view.gateReason));
    if (reasons.has("GATE_REASON_WAITING_FOR_OPERATOR")) {
      alerts.push({ kind: "waiting_for_operator", text: `${host.nodeId} is waiting for an operator` });
    }
    if (reasons.has("GATE_REASON_DUPLICATE")) {
      alerts.push({ kind: "duplicate", text: `${host.nodeId} shares its identity with another host` });
    }
    for (const view of views) {
      const held = (view.holds || []).length > 0;
      const version = view.installedVersion;
      if (held && typeof version === "string" && (bad.get(view.service) || []).includes(version)) {
        alerts.push({
          kind: "held_on_bad_version",
          text: `${host.nodeId} is held on ${view.service} ${version}, which is marked bad`,
        });
      }
    }
  }
  for (const entry of status.audit || []) {
    if (entry.action === "exclude_abandon") {
      alerts.push({
        kind: "abandoned_while_updating",
        text: `${entry.nodeId || "a host"} was abandoned while updating ${entry.service || "a service"}`
          + (entry.reason ? `: ${entry.reason}` : ""),
      });
    }
  }
  return alerts;
}

function marks(view) {
  const out = [];
  if ((view.holds || []).length > 0) {
    out.push("held");
  }
  if (view.membership === "MEMBERSHIP_EXCLUDED" || typeof view.excludedReason === "string") {
    out.push("excluded");
  }
  if (view.holdsSlot === true) {
    out.push("holds the update slot");
  }
  const gate = enumLabel(view.gateReason, "GATE_REASON_");
  const said = (gate === "held" && out.includes("held")) || (gate === "-" || gate === "none" || gate === "up_to_date");
  if (!said) {
    out.push(gate);
  }
  return out;
}

function serviceLine(view) {
  const version = typeof view.installedVersion === "string" ? view.installedVersion : "not installed";
  const { health: reported } = view;
  const health = enumLabel(reported, "HEALTH_");
  const shown = marks(view);
  return h("li", {},
    h("strong", {}, view.service), ` ${version}, ${health}`,
    shown.length > 0 ? h("span", { class: "marks" }, ` [${shown.join(", ")}]`) : null);
}

export async function render(ctx) {
  let hosts;
  let status;
  try {
    hosts = (await ctx.api.call("hostkeeper.operator.v1.Hosts/List", {})).hosts || [];
    status = await ctx.api.call("hostkeeper.operator.v1.Releases/Status", {});
  } catch (error) {
    return errorView("Fleet", error);
  }
  const alerts = fleetAlerts(hosts, status);
  const rows = hosts.map((host) => h("tr", {},
    h("td", {},
      h("button", {
        type: "button", class: "linklike", name: "open-host",
        on: { click: () => ctx.navigate("host", { nodeId: host.nodeId }) },
      }, host.nodeId)),
    h("td", {}, host.role || "-"),
    h("td", {}, host["live"] === true ? "live" : "silent"),
    h("td", {}, fmtUnix(host.lastHeartbeatUnix)),
    h("td", {}, h("ul", { class: "plain" }, list(host, "services").map(serviceLine)))));
  return h("section", { class: "page" },
    h("h2", {}, "Fleet"),
    status.quiet === true ? h("p", { class: "note" }, "The coordinator is in its quiet start: it orders nothing yet.") : null,
    h("div", { class: "alerts", role: "status" },
      h("h3", {}, `Alerts (${alerts.length})`),
      alerts.length === 0
        ? h("p", {}, "Nothing needs a person.")
        : h("ul", {}, alerts.map((alert) => h("li", { class: "alert" },
          h("strong", {}, alert.kind.replaceAll("_", " ")), `: ${alert.text}`)))),
    h("h3", {}, `Hosts (${hosts.length})`),
    table(["Host", "Role", "Heartbeat", "Last heartbeat", "Services"], rows));
}
