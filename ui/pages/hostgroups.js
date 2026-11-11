// Host groups: the section the Rollout page shows under its services. A table of every group (its
// packages, the keys of its policy that are set, its generation, its host count and its desired
// versions); a group's detail (each package's desired and minimum version and convergence, and
// its hosts a page at a time, each a link to the Host page); and one action, moving a host to a
// group. Groups are created and changed with `hkctl host-groups put`, not here.
// Calls: HostGroups.List (every page), HostGroups.Get on a group's name and its hosts' "Next",
// and HostGroups.Move on its button.

import { h, action, allPages, describeError, enumLabel, field, list, reasonProblem, table, trimmed } from "../view.js";

const HOSTS_PER_PAGE = 50;

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

// The section as `{view}`, a function that returns the current tree; `redraw` shows it again
// after the section's own state changes. A List that fails shows its reason in the section.
export async function section(ctx, redraw) {
  let groups = [];
  let failure = null;
  try {
    groups = await allPages((token) => ctx.api.hostGroups.list(token === "" ? {} : { pageToken: token }), "hostGroups");
  } catch (error) {
    failure = error;
  }
  const state = { picked: null, move: { nodeId: "", hostGroup: "", reason: "" } };

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
    redraw();
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
        }, nodeId)))),
      open.pageToken !== ""
        ? action(ctx, "First page", "first-hosts", () => fetchHosts(open.groupName, ""))
        : null,
      open.nextToken !== ""
        ? action(ctx, "Next", "next-hosts", () => fetchHosts(open.groupName, open.nextToken))
        : null);
  };

  const withReason = (run) => async () => {
    if (trimmed(state.move.reason) === "") {
      return "Give a reason first.";
    }
    return reasonProblem(trimmed(state.move.reason)) || run(trimmed(state.move.reason));
  };

  const moveForm = () => h("fieldset", {},
    h("legend", {}, "Move a host"),
    field("Host", "move-host", state.move, "nodeId"),
    field("Host group", "move-group", state.move, "hostGroup"),
    field("Reason", "move-reason", state.move, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" },
      action(ctx, "Move", "move", withReason(async (reason) => {
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
      }))));

  const rows = () => groups.map((held) => {
    const { group = {} } = held;
    const { name } = group;
    return h("tr", {},
      h("td", {}, action(ctx, name, "open-group", () => fetchHosts(name, ""), { class: "linklike" })),
      h("td", {}, list(group, "packages").join(", ") || "-"),
      h("td", {}, policySummary(group.policy)),
      h("td", {}, held.generation || "-"),
      h("td", {}, String(held.hosts || 0)),
      h("td", {}, desiredLine(list(held, "packages"))));
  });

  const view = () => h("section", { class: "host-groups" },
    h("h3", {}, "Host groups"),
    h("p", { class: "note" }, "Host groups are created and changed with hkctl host-groups put."),
    failure !== null ? h("p", { class: "error", role: "alert" }, `Host groups: ${describeError(failure)}`) : null,
    failure === null && groups.length === 0 ? h("p", { class: "note" }, "No host group is held yet.") : null,
    groups.length > 0
      ? table(["Group", "Packages", "Policy", "Generation", "Hosts", "Desired"], rows())
      : null,
    detail(),
    moveForm());
  return { view };
}
