// Maintenance: per host group, its hosts and what is due on each, from the maintenance report
// Hosts.List answers for each host (`updates.js` says it in words), with the age of each host's last
// look and whether it is stale, and the runs of maintenance (`runs.js`) with what each host of a run
// is doing; from here an update is started and a run is cancelled or a halted host retried or put back.
//
// The page opens with the fleet's count ("Updates due on 7 hosts in 2 groups"), then the runs, then one
// card per group, in name order and Unassigned (the hosts in no group) last; a card lists the group's
// hosts that have something due first, then the others, each in name order, each a link to its host
// page. A due host's row has an Update button; the card has one Reason field for its buttons, an Update
// button for the group (not on Unassigned) and a Schedule... button that opens the Schedule form
// (`schedule.js`) for the group's hosts, with the group's policy read when it is opened. A host whose
// daemon sends no report says so; a stale look is said to be stale.
// Calls: Hosts.List, every page of it, when the page is read and again every REFRESH_SECONDS through
// the frame's `ctx.every`; a refresh that fails keeps the last answer and says the refresh failed.
// Maintenance.List (the last 10 runs) and Maintenance.Get of each run that is not done, after the hosts
// are read and again every RUNS_REFRESH_SECONDS (the page is drawn again only when the runs changed)
// and after a stored run; a run list that fails is said
// in the runs section and the rest of the page stays. Maintenance.Schedule (Update, Schedule),
// Maintenance.Cancel, RetryHost and PutBack, one call for each click, and HostGroups.Get when a
// Schedule form is opened.

import { h, action, allPages, describeError, errorView, field, list, table } from "../view.js";
import { duePhrases, isDue, isStale, lookWords } from "../updates.js";
import { newScheduleState, scheduleForm, updateButton } from "../schedule.js";
import { runsSection } from "../runs.js";

// How often the page reads the hosts again, in seconds.
export const REFRESH_SECONDS = 60;

// How often the page reads the runs again, in seconds.
export const RUNS_REFRESH_SECONDS = 10;

// How many runs the page lists.
const RUNS_LIMIT = 10;

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

// The group's card: its hosts' rows, the Reason field and Update button of the group, and the
// Schedule form when it is open. `env` holds what the page keeps across repaints.
function groupCard(env, group, members) {
  const { ctx } = env;
  const due = members.filter(isDue).length;
  const key = group;
  if (!env.reasons.has(key)) {
    env.reasons.set(key, {});
  }
  const state = env.reasons.get(key);
  const updating = { ctx, schedule: env.schedule, state, after: env.after };
  const open = env.forms.get(key);
  return h("section", { class: "card", name: "maintenance-group" },
    h("h3", {}, group === "" ? UNASSIGNED : group),
    h("p", { class: "note" }, `${due} of ${members.length} ${members.length === 1 ? "host" : "hosts"} with updates due`),
    table(["Host", "Due", "Last look", ""], members.map((host) => h("tr", { name: isDue(host) ? "due-host" : "host" },
      h("td", {}, h("button", {
        type: "button", class: "linklike", name: "open-host",
        on: { click: () => ctx.navigate("host", { nodeId: nameOf(host) }) },
      }, nameOf(host))),
      h("td", {}, dueWords(host)),
      h("td", {}, lookWords(host)),
      h("td", {}, isDue(host) ? updateButton(updating, { hostIds: [nameOf(host)] }) : null)))),
    h("div", { class: "actions", name: "group-actions" },
      field("Reason", "update-reason", state, "reason", { maxlength: 256 }),
      group === "" ? null : updateButton(updating, { hostGroup: group }, "Update the group"),
      action(ctx, open === undefined ? "Schedule..." : "Close the form", "open-schedule", async () => {
        if (open !== undefined) {
          env.forms.delete(key);
        } else {
          const policy = await policyOf(ctx, group);
          env.forms.set(key, { form: newScheduleState({ hosts: members, group, policy, hostId: "" }), policy });
        }
        env.repaint();
      })),
    open === undefined ? null : scheduleForm({
      ctx, schedule: env.schedule, after: env.after, repaint: env.repaint, form: open.form,
    }, { hosts: members, group, policy: open.policy, hostId: "" }));
}

// The maintenance policy of `group`, read when its form is opened; null for no group or when it
// could not be read.
async function policyOf(ctx, group) {
  if (group === "") {
    return null;
  }
  try {
    const reply = await ctx.api.hostGroups.get({ hostGroup: group, limit: 1 });
    const policy = ((((reply || {}).hostGroup || {}).group || {}).policy) || {};
    return policy.maintenance || null;
  } catch (error) {
    return null;
  }
}

// The last runs and, for each that is not done, its hosts: `{ runs: [{ run, hosts }], failure }`;
// `hosts` is null, with `failure` on the entry, when the run's Get failed.
async function readRuns(ctx) {
  let reply;
  try {
    reply = await ctx.api.maintenance.list({ limit: RUNS_LIMIT });
  } catch (error) {
    return { runs: [], failure: error };
  }
  const runs = [];
  for (const run of list(reply, "runs")) {
    if (run.state === "MAINTENANCE_STATE_DONE") {
      runs.push({ run, hosts: [] });
      continue;
    }
    try {
      const got = await ctx.api.maintenance.get({ runId: run["id"] });
      runs.push({ run, hosts: list(got, "hosts") });
    } catch (error) {
      runs.push({ run, hosts: null, failure: error });
    }
  }
  return { runs, failure: null };
}

// Whether two reads of the runs say the same, so a repeat that finds nothing new does not draw the
// page again (which would take the focus from a field the operator is typing in).
function sameRuns(a, b) {
  const said = (read) => JSON.stringify([
    read.runs.map(({ run, hosts, failure }) => [run, hosts, failure ? describeError(failure) : null]),
    read.failure ? describeError(read.failure) : null,
  ]);
  return said(a) === said(b);
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
  let read = await readRuns(ctx);
  const runsState = {};
  const env = {
    ctx,
    reasons: new Map(),
    forms: new Map(),
    schedule: (request) => ctx.api.maintenance.schedule(request),
    after: async () => {
      read = await readRuns(ctx);
      ctx.show(page());
    },
    repaint: () => ctx.show(page()),
  };
  const byId = () => new Map(hosts.map((host) => [nameOf(host), host]));
  const page = () => {
    const grouped = byGroup(hosts);
    return h("section", { class: "page" },
      h("h2", {}, "Maintenance"),
      h("p", { class: "note" },
        "What each host's daemon last reported as due, and the runs that update it. An update drains a host, updates it, restarts it if needed and puts it back."),
      refreshFailure === null ? null : h("p", { class: "warning", role: "alert" },
        `The hosts could not be read again (${describeError(refreshFailure)}): this is the last answer.`),
      h("p", { name: "fleet-due" }, h("strong", {}, fleetWords(grouped))),
      runsSection({
        ctx,
        hosts: byId(),
        cancel: (request) => ctx.api.maintenance.cancel(request),
        retryHost: (request) => ctx.api.maintenance.retryHost(request),
        putBack: (request) => ctx.api.maintenance.putBack(request),
        after: env.after,
        state: runsState,
      }, read.runs, read.failure),
      grouped.length === 0
        ? h("p", { class: "quiet" }, "No host is enrolled.")
        : grouped.map(([group, members]) => groupCard(env, group, members)));
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
  ctx.every(async () => {
    const next = await readRuns(ctx);
    if (sameRuns(next, read)) {
      return;
    }
    read = next;
    ctx.show(page());
  }, RUNS_REFRESH_SECONDS);
  return page();
}
