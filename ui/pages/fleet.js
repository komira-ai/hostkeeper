// Fleet: every host the coordinator lists (heard from since it started, or enrolled and not heard,
// not live), and the conditions that need a person.
// Calls: Hosts.List, Releases.Status and Alerts.List, nothing else.

import { h, allPages, describeError, enumLabel, errorView, fmtUnix, list, table } from "../view.js";

// The alerts box: each alert of Alerts.List as the coordinator derived it (its kind, what
// happened, and when the coordinator first saw it), or the refusal when the call failed. The
// page derives no alert of its own.
function alertsBox(reply, failure) {
  if (failure !== null) {
    return h("div", { class: "alerts", role: "status" },
      h("h3", {}, "Alerts"),
      h("p", { class: "error", role: "alert" }, `The alerts could not be read: ${describeError(failure)}`));
  }
  const alerts = list(reply, "alerts");
  return h("div", { class: "alerts", role: "status" },
    h("h3", {}, `Alerts (${alerts.length})`),
    alerts.length === 0
      ? h("p", {}, "Nothing needs a person.")
      : h("ul", {}, alerts.map((alert) => h("li", { class: "alert" },
        h("strong", {}, enumLabel(alert.kind, "ALERT_KIND_").replaceAll("_", " ")),
        `${alert.severity === "ALERT_SEVERITY_CRITICAL" ? " (critical)" : ""}: ${alert.message || ""}`,
        h("small", {}, ` (since ${fmtUnix(alert.firstObservedUnix)})`)))));
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

// Every host of Hosts.List, a page (at most 256 hosts) at a time.
function listHosts(api) {
  return allPages((pageToken) => api.hosts.list(
    pageToken === "" ? {} : { pageToken }), "hosts");
}

export async function render(ctx) {
  let hosts;
  let status;
  try {
    hosts = await listHosts(ctx.api);
    status = await ctx.api.releases.status({});
  } catch (error) {
    return errorView("Fleet", error);
  }
  let alerts = null;
  let failure = null;
  try {
    alerts = await ctx.api.alerts.list({});
  } catch (error) {
    failure = error;
  }
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
    alertsBox(alerts, failure),
    h("h3", {}, `Hosts (${hosts.length})`),
    table(["Host", "Role", "Heartbeat", "Last heartbeat", "Services"], rows));
}
