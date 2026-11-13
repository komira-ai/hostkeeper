// Host groups: what each host group deploys. A table of every group (its packages, the keys of its
// policy that are set, its generation, its host count and its desired versions), the built-in
// group `all` that every host is in first and marked as built in; a group's detail
// (each package's desired and minimum version and convergence, and its hosts a page at a time,
// each a link to the Host page); a form that creates a group or changes one's ordered package list
// (the packages are the services deployed to each of its hosts: one per line, in install order;
// `all` lists `hostkeeper`, which a specific group need not); and moving a host into a specific
// group, the group chosen from a dropdown of the groups held but `all`, which has every host. A group's rollout policy is not edited here: a change keeps the policy the
// group holds, and a new group has the default one (`hkctl host-groups put` sets one).
// Calls: HostGroups.List (every page), HostGroups.Get on a group's name and its hosts' "Next",
// HostGroups.Put on Save, which replaces the group named over the generation List gave (or
// creates it), and HostGroups.Move on its button, and with an empty group on "Remove from group"
// beside each host of a group's detail (a host in no group runs the packages of `all` alone). The reason of a Put is optional here: it is
// sent as a default when left empty.

import {
  h, action, allPages, area, describeError, enumLabel, field, list, reasonProblem, select, table,
  trimmed, words,
} from "../view.js";

const HOSTS_PER_PAGE = 50;

// The built-in group every host is in: it cannot be removed, and a host is not moved into it.
const ALL = "all";

// The keys of a group's policy that are set, in words; "default" when none is.
export function policySummary(policy) {
  const p = policy || {};
  const parts = [];
  const stages = list(p, "stages");
  if (stages.length > 0) {
    parts.push("stages " + stages.map((s) => `${s.percent || 0}%${s.advance === "STAGE_ADVANCE_MANUAL" ? " manual" : ""}`).join("/"));
  }
  if (p.canary) {
    parts.push(`canary ${p.canary.count || 0}`);
  }
  if (p.bakeBeats !== undefined) {
    parts.push(`bake ${p.bakeBeats} beats`);
  }
  if (p.rollback !== undefined) {
    parts.push(`rollback ${enumLabel(p.rollback, "ROLLBACK_MODE_")}`);
  }
  if (p.rollbackAfterFailed) {
    const t = p.rollbackAfterFailed;
    parts.push(`rollback after ${t.percent || 0}% failed, at least ${t.minHosts || 0} hosts`);
  }
  if (p.deferBackoff) {
    const d = p.deferBackoff;
    parts.push(`defer backoff ${d.initialSecs || 0}s x${d.factor || 0} to ${d.maxSecs || 0}s`);
  }
  if (p.maxUpdating !== undefined) {
    parts.push(`max updating ${p.maxUpdating}`);
  }
  if (p.minUp !== undefined) {
    parts.push(`min up ${p.minUp}`);
  }
  if (p.liveRollouts !== undefined) {
    parts.push(`live rollouts ${p.liveRollouts}`);
  }
  return parts.length > 0 ? parts.join(", ") : "default";
}

function desiredLine(packages) {
  const lines = packages.map((p) => `${p.package} ${p.desiredVersion || "none set"}`);
  return lines.length > 0 ? lines.join(", ") : "-";
}

// The counts of a package's convergence, summed over its roles.
function counts(convergence) {
  const sum = (key) => list(convergence, "roles").reduce((n, r) => n + (r[key] || 0), 0);
  return `${sum("atDesired")} at desired, ${sum("updating")} updating, ${sum("failed")} failed, ${sum("down")} down`;
}

// The reasons sent when the operator gives none: the API wants one, and the audit log names the
// operator anyway.
const DEFAULT_PUT_REASON = "set in the console";
// The reason of a Move out of a group, which the button does not ask for.
const DEFAULT_REMOVE_REASON = "removed from the group in the console";

// The body of a group as List answers it (its name, packages and policy).
const bodyOf = (one) => (one || {})["group"] || {};

// The Host groups page: a table of the groups, the detail of the one opened, the form that
// creates or changes a group, the hosts in no group, and the form that moves a host into a group.
// A List that fails shows its reason in the page.
export async function render(ctx) {
  let groups = [];
  let failure = null;
  try {
    groups = await allPages((token) => ctx.api.hostGroups.list(token === "" ? {} : { pageToken: token }), "hostGroups");
  } catch (error) {
    failure = error;
  }
  // The hosts in no group, which a Move puts in one; a List that fails leaves none shown.
  let ungrouped = [];
  let hostsFailure = null;
  try {
    const hosts = await allPages((token) => ctx.api.hosts.list(token === "" ? {} : { pageToken: token }), "hosts");
    ungrouped = hosts.filter((host) => !host.hostGroup).map((host) => host.nodeId);
  } catch (error) {
    hostsFailure = error;
  }
  const state = {
    picked: null,
    move: { nodeId: "", hostGroup: "", reason: "" },
    edit: { name: "", packages: "", reason: "" },
  };
  const show = () => ctx.show(page());
  const held = (name) => groups.find((one) => bodyOf(one).name === name);

  const fetchHosts = async (name, pageToken) => {
    const request = pageToken === "" ? { hostGroup: name, limit: HOSTS_PER_PAGE } : { hostGroup: name, limit: HOSTS_PER_PAGE, pageToken };
    try {
      const reply = await ctx.api.hostGroups.get(request);
      state.picked = {
        groupName: name,
        pageToken,
        reply,
        error: null,
        nextToken: typeof reply.nextPageToken === "string" ? reply.nextPageToken : "",
      };
    } catch (error) {
      state.picked = { groupName: name, pageToken, reply: null, error, nextToken: "" };
    }
    show();
  };

  const detail = () => {
    const open = state.picked;
    if (open === null) {
      return null;
    }
    if (open.error !== null) {
      return h("p", { class: "error", role: "alert" }, `${open.groupName}: ${describeError(open.error)}`);
    }
    const group = open.reply.hostGroup || {};
    const packages = list(group, "packages");
    const nodeIds = list(open.reply, "nodeIds");
    return h("div", { class: "group-detail" },
      h("h4", {}, `${open.groupName}, generation ${group.generation || "-"}`),
      table(["Package", "Desired", "Minimum", "State", "Hosts"], packages.map((p) => h("tr", {},
        h("td", {}, p.package),
        h("td", {}, p.desiredVersion || "none set"),
        h("td", {}, p.minVersion || "-"),
        h("td", {}, enumLabel((p.convergence || {}).state, "CONVERGENCE_STATE_")),
        h("td", {}, counts(p.convergence))))),
      h("h4", {}, `Hosts (${group.hosts || 0})`),
      h("ul", { class: "plain" }, nodeIds.map((nodeId) => h("li", {},
        h("button", {
          type: "button", class: "linklike", name: "open-host",
          on: { click: () => ctx.navigate("host", { nodeId }) },
        }, nodeId),
        // Every host is in all, which a host is not removed from.
        open.groupName === ALL ? null : action(ctx, "Remove from group", "remove-from-group", async () => {
          if (!ctx.confirm(`Remove ${nodeId} from host group ${open.groupName}? It then runs the packages of all alone, and what ${open.groupName} lists and all does not is uninstalled.`)) {
            return "Not removed.";
          }
          await ctx.api.hostGroups.move({ nodeId, hostGroup: "", reason: DEFAULT_REMOVE_REASON });
          ctx.refresh();
          return `Removed ${nodeId} from host group ${open.groupName}: it runs the packages of all alone.`;
        })))),
      open.pageToken !== ""
        ? action(ctx, "First page", "first-hosts", () => fetchHosts(open.groupName, ""))
        : null,
      open.nextToken !== ""
        ? action(ctx, "Next", "next-hosts", () => fetchHosts(open.groupName, open.nextToken))
        : null);
  };

  // Loads the group `name` into the form that changes a group: its name and its packages, in order.
  const edit = (name) => {
    const one = held(name);
    state.edit = { name, packages: list(bodyOf(one), "packages").join("\n"), reason: "" };
    show();
  };

  // The reason to send: the one typed, or `fallback` when none is; or a sentence when it breaks
  // the API's rule.
  const reasonOf = (typed, fallback) => {
    const reason = trimmed(typed);
    if (reason === "") {
      return { reason: fallback };
    }
    const problem = reasonProblem(reason);
    return problem === null ? { reason } : { problem };
  };

  const editForm = () => h("fieldset", {},
    h("legend", {}, "Create or change a host group"),
    h("p", { class: "note" }, "The packages are the services deployed to each host of the group, one per line, in install order. A name that is held changes that group and keeps its policy. A group's own packages run after those of all."),
    field("Name", "group-name", state.edit, "name"),
    area("Packages", "group-packages", state.edit, "packages"),
    field("Reason (optional)", "group-reason", state.edit, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" },
      action(ctx, "Save", "put-group", async () => {
        const { name: typed } = state.edit;
        const name = trimmed(typed);
        const packages = words(state.edit.packages);
        if (name === "") {
          return "Name the host group.";
        }
        if (name === ALL && !packages.includes("hostkeeper")) {
          return "The packages of all must include hostkeeper.";
        }
        const given = reasonOf(state.edit.reason, DEFAULT_PUT_REASON);
        if (given.problem) {
          return given.problem;
        }
        const body = { hostGroup: { name, packages }, reason: given.reason };
        const existing = held(name);
        if (existing !== undefined) {
          const before = list(bodyOf(existing), "packages");
          const dropped = before.filter((p) => !packages.includes(p));
          if (dropped.length > 0 && !ctx.confirm(
            `Change host group ${name}? ${dropped.join(", ")} will be uninstalled from its hosts.`)) {
            return "Not saved.";
          }
          body.hostGroup.policy = bodyOf(existing).policy;
          body.ifGeneration = existing.generation;
        }
        const reply = await ctx.api.hostGroups.put(body);
        ctx.refresh();
        return `Saved host group ${name} (generation ${reply.generation || "-"}).`;
      })));

  const moveForm = () => h("fieldset", {},
    h("legend", {}, "Move a host"),
    ungrouped.length > 0
      ? h("div", {},
        h("p", { class: "note" }, "Hosts in no host group run the packages of all alone until they are moved into one."),
        h("ul", { class: "plain" }, ungrouped.map((nodeId) => h("li", {},
          action(ctx, nodeId, "pick-host", () => {
            state.move.nodeId = nodeId;
            show();
          }, { class: "linklike" })))))
      : null,
    hostsFailure === null ? null : h("p", { class: "error", role: "alert" }, `Hosts: ${describeError(hostsFailure)}`),
    field("Host", "move-host", state.move, "nodeId"),
    select("Host group", "move-group", state.move, "hostGroup",
      [["", "Choose a group"], ...groups.map((one) => bodyOf(one).name).filter((name) => name !== ALL).map((name) => [name, name])]),
    field("Reason", "move-reason", state.move, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" },
      action(ctx, "Move", "move", async () => {
        if (trimmed(state.move.reason) === "") {
          return "Give a reason first.";
        }
        const problem = reasonProblem(trimmed(state.move.reason));
        if (problem) {
          return problem;
        }
        const reason = trimmed(state.move.reason);
        const nodeId = trimmed(state.move.nodeId);
        const hostGroup = trimmed(state.move.hostGroup);
        if (nodeId === "" || hostGroup === "") {
          return "Name the host and the host group.";
        }
        if (!ctx.confirm(`Move ${nodeId} to host group ${hostGroup}? It is ordered that group's packages at its next heartbeat.`)) {
          return "Not moved.";
        }
        const reply = await ctx.api.hostGroups.move({ nodeId, hostGroup, reason });
        ctx.refresh();
        return `Moved ${nodeId} from ${reply.fromHostGroup || "no group"} to ${hostGroup}.`;
      })));

  const rows = () => groups.map((one) => {
    const { group = {} } = one;
    const { name } = group;
    return h("tr", {},
      h("td", {},
        action(ctx, name, "open-group", () => fetchHosts(name, ""), { class: "linklike" }),
        name === ALL ? " (built in: every host)" : null),
      h("td", {}, list(group, "packages").join(", ") || "-"),
      h("td", {}, policySummary(group.policy)),
      h("td", {}, one.generation || "-"),
      h("td", {}, String(one.hosts || 0)),
      h("td", {}, desiredLine(list(one, "packages"))),
      h("td", {}, action(ctx, "Edit", "edit-group", () => edit(name))));
  });

  const page = () => h("section", { class: "page host-groups" },
    h("h2", {}, "Host groups"),
    h("p", { class: "note" }, "A host group lists the services deployed to its hosts. Every host is in all, and may be in one host group besides."),
    failure !== null ? h("p", { class: "error", role: "alert" }, `Host groups: ${describeError(failure)}`) : null,
    failure === null && groups.length === 0 ? h("p", { class: "note" }, "No host group is held yet.") : null,
    groups.length > 0
      ? table(["Group", "Packages", "Policy", "Generation", "Hosts", "Desired", ""], rows())
      : null,
    detail(),
    editForm(),
    moveForm());
  return page();
}
