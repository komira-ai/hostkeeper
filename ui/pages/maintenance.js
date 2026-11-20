// Maintenance: per host group, its hosts and what is due on each, from the maintenance report
// Hosts.List answers for each host (`updates.js` says it in words), with the age of each host's last
// look and whether it is stale. Read only: nothing here changes anything.
//
// The page opens with the fleet's count ("Updates due on 7 hosts in 2 groups"), then one card per
// group, in name order and Unassigned (the hosts in no group) last; a card lists the group's hosts
// that have something due first, then the others, each in name order, each a link to its host page.
// A host whose daemon sends no report says so; a stale look is said to be stale.
// Calls: Hosts.List, every page of it, when the page is read and again every REFRESH_SECONDS through
// the frame's `ctx.every`; a refresh that fails keeps the last answer and says the refresh failed.

import { h, allPages, describeError, errorView, table } from "../view.js";
import { duePhrases, isDue, isStale, lookWords } from "../updates.js";

// How often the page reads the hosts again, in seconds.
export const REFRESH_SECONDS = 60;

// The name of the card of the hosts in no group.
const UNASSIGNED = "Unassigned";

function groupOf(host) {
  return typeof host.hostGroup === "string" && host.hostGroup !== "" ? host.hostGroup : "";
}

function nameOf(host) {
  return typeof host.nodeId === "string" ? host.nodeId : "";
}

// The hosts by group: `[[group, hosts]]`, groups in name order and "" (no group) last, each group's
// hosts with something due first, then by name.
export function byGroup(hosts) {
  const groups = new Map();
  for (const host of hosts) {
    const group = groupOf(host);
    if (!groups.has(group)) {
      groups.set(group, []);
    }
    groups.get(group).push(host);
  }
  const order = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
  return [...groups.entries()]
    .sort(([a], [b]) => (a === "" ? 1 : b === "" ? -1 : order(a, b)))
    .map(([group, members]) => [group, members.slice().sort((a, b) => (
      Number(isDue(b)) - Number(isDue(a)) || order(nameOf(a), nameOf(b))))]);
}

// The fleet's count of what is due, in words.
export function fleetWords(grouped) {
  const due = grouped.map(([, members]) => members.filter(isDue).length);
  const hosts = due.reduce((sum, n) => sum + n, 0);
  if (hosts === 0) {
    return "No host reports anything due.";
  }
  const groups = due.filter((n) => n > 0).length;
  return `Updates due on ${hosts} ${hosts === 1 ? "host" : "hosts"} in ${groups} ${groups === 1 ? "group" : "groups"}`;
}

// What the row of `host` says is due.
function dueWords(host) {
  if (host.maintenance === undefined) {
    return "no report";
  }
  const phrases = duePhrases(host.maintenance);
  if (phrases.length > 0) {
    return phrases.join("; ");
  }
  return isStale(host) ? "nothing listed (stale)" : "nothing due";
}

function groupCard(ctx, group, members) {
  const due = members.filter(isDue).length;
  return h("section", { class: "card", name: "maintenance-group" },
    h("h3", {}, group === "" ? UNASSIGNED : group),
    h("p", { class: "note" }, `${due} of ${members.length} ${members.length === 1 ? "host" : "hosts"} with updates due`),
    table(["Host", "Due", "Last look"], members.map((host) => h("tr", { name: isDue(host) ? "due-host" : "host" },
      h("td", {}, h("button", {
        type: "button", class: "linklike", name: "open-host",
        on: { click: () => ctx.navigate("host", { nodeId: nameOf(host) }) },
      }, nameOf(host))),
      h("td", {}, dueWords(host)),
      h("td", {}, lookWords(host))))));
}

function hostsOf(ctx) {
  return allPages((pageToken) => ctx.api.hosts.list(pageToken === "" ? {} : { pageToken }), "hosts");
}

export async function render(ctx) {
  let hosts;
  try {
    hosts = await hostsOf(ctx);
  } catch (error) {
    return errorView("Maintenance", error);
  }
  let refreshFailure = null;
  const page = () => {
    const grouped = byGroup(hosts);
    return h("section", { class: "page" },
      h("h2", {}, "Maintenance"),
      h("p", { class: "note" },
        "What each host's daemon last reported as due. Read only: an update is not started from here yet."),
      refreshFailure === null ? null : h("p", { class: "warning", role: "alert" },
        `The hosts could not be read again (${describeError(refreshFailure)}): this is the last answer.`),
      h("p", { name: "fleet-due" }, h("strong", {}, fleetWords(grouped))),
      grouped.length === 0
        ? h("p", { class: "quiet" }, "No host is enrolled.")
        : grouped.map(([group, members]) => groupCard(ctx, group, members)));
  };
  ctx.every(async () => {
    try {
      hosts = await hostsOf(ctx);
      refreshFailure = null;
    } catch (error) {
      refreshFailure = error;
    }
    ctx.show(page());
  }, REFRESH_SECONDS);
  return page();
}
