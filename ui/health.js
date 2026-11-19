// Health: why a host is not healthy, in plain words. Each reason is one condition the coordinator
// holds of the host, with the time it began when one is known and the one action that fixes it:
// `clear-halt` (Releases.ClearHalt of the service), `retry` (Hosts.Retry of the host and the
// service), or none. The Host page shows each reason with its button, a server's row on Home its
// reasons in short, and Home's fleet view the degraded hosts with their reasons grouped.
//
// A reason is `{kind, key, group, title, said, since, sinceIs, tone, service, fix, taint}`: `kind`
// one of `halt`, `failed`, `unhealthy`, `prerequisite` and `alert`; `group` its short words, which a
// row shows and the fleet view groups by; `title` its sentence; `said` what the coordinator or the
// host said of it (null for nothing); `since` unix seconds (absent or 0 for not known) and
// `sinceIs` whose clock that is; `tone` `bad` or `warn`; `service` the service it concerns, or null;
// `fix` the action; `taint` null, or `{said, log}` for a taint.
//
// The reasons come from two places, so that what one says is never lost to the other:
// - every alert of Alerts.List that names the host (`hostId`), and
// - the host's own services as Hosts.Get or Hosts.List answers them: a halt the host holds or
//   reported, a failed last update, a service that reports itself unhealthy.
// A halt alert and the failed halt report of the same host and service are one reason. A failed
// update whose reason is `tainted` (the halt a js-agent's update hook reports for a tainted agent)
// carries the taint's facts as hostkeeper holds them: what the agent said and its log lines.
// Everything here is a pure function of the replies it is given; no call is made here.

import { enumLabel, list } from "./view.js";

// The halt reason a js-agent's update hook reports when the agent is tainted.
export const TAINTED = "tainted";

// The gate reasons of a host that holds a halt of the service of its own.
const HALTED_HERE = ["GATE_REASON_HALTED_HERE", "GATE_REASON_STALE_HALT_HERE"];

// A text the API answered, or null when it is absent or empty.
function given(text) {
  return typeof text === "string" && text !== "" ? text : null;
}

// The words of an alert's kind, as a reason's title and group name it (`ALERT_KIND_SILENT_HOST`
// reads `silent host`).
function kindWords(kind) {
  return enumLabel(kind, "ALERT_KIND_").replaceAll("_", " ");
}

// The taint facts of a failed update whose reason is `tainted`, or null for any other. The hook
// reports the taint in its detail and log lines; hostkeeper is told no check and no run id.
function taintOf(last) {
  if (!last || last.reason !== TAINTED) {
    return null;
  }
  return { said: given(last.detail), log: list(last, "logTail") };
}

// Whether `last` is a failed update.
function failed(last) {
  return Boolean(last) && last.outcome === "OUTCOME_FAILED";
}

// A halt of `service` that stands for the host. `alert` is the halt alert that names the host, or
// null when none does (the host holds the halt of its own, or the halt it reported is the
// service's while the alert names another host); `view` the host's view of the service, if any.
function haltReason(service, view, alert) {
  const last = view && failed(view.lastUpdate) ? view.lastUpdate : null;
  const check = last === null ? null : given(last.reason);
  const here = view !== undefined && HALTED_HERE.includes(view.gateReason);
  return {
    kind: "halt",
    key: `halt:${service}`,
    group: check === null ? `${service} halted` : `${service} halted: ${check}`,
    title: here && alert === null
      ? `${service} is halted on this host: the host holds a halt of its own`
      : `${service} is halted${check === null ? "" : ` by the check ${check}`}`,
    said: alert === null ? (last === null ? null : given(last.detail)) : given(alert.message),
    since: alert === null ? (last === null ? null : last.startedUnix) : alert.firstObservedUnix,
    sinceIs: alert === null ? "started on the host" : "first seen by the coordinator",
    tone: "bad",
    service,
    fix: "clear-halt",
    taint: taintOf(last),
  };
}

// A failed update of the service `view` names that no halt stands for: Retry orders it again.
function failedReason(view) {
  const last = view.lastUpdate;
  const reason = given(last.reason);
  const target = given(last["target"]);
  return {
    kind: "failed",
    key: `failed:${view.service}`,
    group: `${view.service} update failed: ${reason || "no reason given"}`,
    title: `The update of ${view.service}${target === null ? "" : ` to ${target}`} failed`
      + (reason === null ? "" : `: ${reason}`),
    said: given(last.detail),
    since: last.startedUnix,
    sinceIs: "started on the host",
    tone: "bad",
    service: view.service,
    fix: "retry",
    taint: taintOf(last),
  };
}

// A service the host reports unhealthy. Its last heartbeat says so; it says no time.
function unhealthyReason(view) {
  return {
    kind: "unhealthy",
    key: `unhealthy:${view.service}`,
    group: `${view.service} unhealthy`,
    title: `${view.service} reports itself unhealthy`,
    said: null,
    since: null,
    sinceIs: null,
    tone: "bad",
    service: view.service,
    fix: null,
    taint: null,
  };
}

// An alert that names the host and is no halt: its words are the coordinator's.
function alertReason(alert) {
  const service = given(alert.service);
  const kind = kindWords(alert.kind);
  const prerequisite = alert.kind === "ALERT_KIND_PREREQUISITE_FAILED";
  return {
    kind: prerequisite ? "prerequisite" : "alert",
    key: `alert:${kind}:${service || ""}`,
    group: prerequisite ? "host prerequisite failed" : (service === null ? kind : `${kind}: ${service}`),
    title: prerequisite ? "A host prerequisite failed" : (service === null ? kind : `${kind} (${service})`),
    said: given(alert.message),
    since: alert.firstObservedUnix,
    sinceIs: "first seen by the coordinator",
    tone: alert.severity === "ALERT_SEVERITY_CRITICAL" ? "bad" : "warn",
    service,
    fix: null,
    taint: null,
  };
}

// Every reason `host` (a `Host` of Hosts.Get or Hosts.List) is not healthy, given the alerts of
// Alerts.List (`alerts`, a list; empty when they could not be read): first the alerts that name
// the host, in the coordinator's order, then the reasons of its services, by the host's order of
// them. For each service, at most one of: a halt (a halt alert naming the host, a halt the host
// holds of its own, or a failed report of the halt phase while the service is halted), else a
// failed update; and, apart, unhealthy when it says so.
export function reasonsOf(host, alerts) {
  const id = given(host.nodeId);
  const views = new Map(list(host, "services").map((view) => [view.service, view]));
  const out = [];
  const halted = new Set();
  for (const alert of id === null ? [] : alerts) {
    if (alert.hostId !== id) {
      continue;
    }
    if (alert.kind === "ALERT_KIND_HALT" && given(alert.service) !== null) {
      halted.add(alert.service);
      out.push(haltReason(alert.service, views.get(alert.service), alert));
    } else {
      out.push(alertReason(alert));
    }
  }
  for (const view of views.values()) {
    const last = view.lastUpdate;
    const haltStands = HALTED_HERE.includes(view.gateReason)
      || (view.gateReason === "GATE_REASON_HALTED" && failed(last) && last.phase === "FAILURE_PHASE_HALT");
    if (halted.has(view.service)) {
      // The halt alert above is this service's reason.
    } else if (haltStands) {
      out.push(haltReason(view.service, view, null));
    } else if (failed(last)) {
      out.push(failedReason(view));
    }
    if (view.health === "HEALTH_UNHEALTHY") {
      out.push(unhealthyReason(view));
    }
  }
  return out;
}

// Whether a host is degraded: live (heard from, and within the last 45 seconds) and with a reason.
// A host that is not live is `offline` or `pending` whatever its reasons; its page still shows them.
export function isDegraded(host, reasons) {
  const heard = host.lastHeartbeatUnix !== undefined && Number(host.lastHeartbeatUnix) !== 0;
  return heard && host.live === true && reasons.length > 0;
}

// The degraded hosts of a fleet (`hosts` of Hosts.List, `alerts` of Alerts.List): `{degraded,
// groups}`, `degraded` the node ids of the live hosts with a reason, by the hosts' order, and
// `groups` their reasons, each `{label, hosts}` with the node ids of the hosts that have it, most
// hosts first, then by label. A host with two reasons is in two groups and counted once.
export function fleetHealth(hosts, alerts) {
  const degraded = [];
  const groups = new Map();
  for (const host of hosts) {
    const reasons = reasonsOf(host, alerts);
    if (!isDegraded(host, reasons)) {
      continue;
    }
    degraded.push(host.nodeId);
    for (const label of new Set(reasons.map(({ group }) => group))) {
      if (!groups.has(label)) {
        groups.set(label, []);
      }
      groups.get(label).push(host.nodeId);
    }
  }
  const sorted = Array.from(groups, ([label, ids]) => ({ label, hosts: ids }))
    .sort((a, b) => b.hosts.length - a.hosts.length || a.label.localeCompare(b.label));
  return { degraded, groups: sorted };
}
