// Package: one package of the registry, reached by clicking it on the Packages page. A header
// with its repository, its latest version and what is desired; one rollout card with a big bar
// (hosts updated of all hosts of the package, the failed ones named) and, per host group, a small
// bar with the versions the hosts run, the state in words and the plan; the acts on it behind
// buttons that open a small form (set the version, pause, resume or advance the plan, hold, check
// convergence); the manifest of a version in a collapsible code view, with the entry each host of
// the package is picked; the holds that stand on the package; its trusted publishers, the GitHub
// Actions runs that may set its desired version, added from a form behind a button; and, in a collapsed danger zone,
// the acts that cut running work or set a bad mark: mark a version bad, halt, and force. These are
// what the Rollout, Release and Holds pages did; their routes lead here.
//
// Every state the coordinator names is shown in words (`installer_dropped` as "Installer
// missing", `in_progress` as "Updating"); a count is a count of hosts in exactly one state, so the
// states of a group add up to its hosts.
//
// While a group has a rollout under way the page asks Releases.Status again every
// `REFRESH_SECONDS` through the frame's `ctx.every` and draws the new counts; it stops when no
// rollout is under way, and the frame stops it when the operator leaves the page.
//
// Calls: Releases.Status (for this package), Services.List, Repositories.List, Releases.GetRelease
// and Hosts.List (every page of it, once for each version shown); on a button, Releases.SetDesired
// (through `setDesired` of the Rollout module, which sets one host group when the entry names
// one), Services.ListTrustedPublishers, Services.PutTrustedPublisher, Services.RemoveTrustedPublisher
// (a package's trusted publishers: the GitHub Actions runs that may set its desired version; an entry
// is a repository, a workflow file, a branch and, if named, an environment, one added or removed
// at a time, and an entry the coordinator imported from its allow-list file shows what that file
// pinned), Rollouts.Pause, Rollouts.Resume, Rollouts.Advance, Services.Hold, Services.Unhold,
// Releases.MarkBad, Releases.UnmarkBad, Releases.Halt, Releases.ClearHalt, Releases.Converge and
// Releases.Force.

import {
  h, action, allPages, describeError, errorView, field, fmtUnix, list, reasonProblem, scopeLabel, select,
  table, trimmed,
} from "../view.js";
import { holdScope } from "./holds.js";
import { digestCell, picksSection, platformOrder, platformSection } from "./release.js";
import {
  forceSection, planButtons, planSummary, setDesired, stageProgress, waitStage,
} from "./rollout.js";

// How often a page with a rollout under way asks for the status again.
export const REFRESH_SECONDS = 5;

function param(ctx, name) {
  return ctx.params && typeof ctx.params[name] === "string" ? ctx.params[name] : "";
}

// ---- words ----

// A coordinator's word as a label: the known ones as they read best, any other with its
// underscores as spaces and a capital.
function wordFor(names, key) {
  if (typeof key !== "string" || key === "") {
    return "-";
  }
  if (Object.prototype.hasOwnProperty.call(names, key)) {
    return names[key];
  }
  const spaced = key.toLowerCase().replace(/_/g, " ");
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

const WAITS = {
  MANUAL_ADVANCE: "Waiting for Advance", FAILED_HOST: "A host failed",
  BELOW_MIN_UP: "Below the minimum up", UPDATING: "Updates under way", DEFERRING: "A host deferred",
  BAKING: "Baking", HALTED: "Halted", QUIET: "Quiet start", ROLLOUT_ELSEWHERE: "Another rollout is live",
};

// Why the plan orders no new host, in words: `Baking, stage 1`, `Paused by ops: freeze`; "-" when
// it waits for nothing.
export function waitingText(plan) {
  if (!plan) {
    return "-";
  }
  const parts = [];
  const wait = typeof plan.waiting === "string" ? plan.waiting.replace("PLAN_WAIT_", "") : "";
  if (!["", "PAUSED", "UNSPECIFIED", "NONE"].includes(wait)) {
    const stage = waitStage(plan);
    const on = list(plan, "waitingOn").join(", ");
    parts.push(wordFor(WAITS, wait) + (stage === null ? "" : `, stage ${stage}`) + (on === "" ? "" : ` on ${on}`));
  }
  if (plan.paused === true) {
    parts.push(`Paused by ${plan.pausedBy || "an operator"}${plan.pauseReason ? `: ${plan.pauseReason}` : ""}`);
  }
  return parts.join("; ") || "-";
}

// ---- versions ----

// The numbers of a dotted version, for ordering; a part that is no number counts 0.
function versionParts(version) {
  return String(version).split(/[.+-]/).map((part) => (/^[0-9]+$/.test(part) ? Number(part) : 0));
}

// Orders versions from the highest to the lowest; equal numbers fall back to the text.
export function byVersionDescending(a, b) {
  const x = versionParts(a);
  const y = versionParts(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (y[i] || 0) - (x[i] || 0);
    if (d !== 0) {
      return d;
    }
  }
  return a < b ? 1 : a > b ? -1 : 0;
}

// The versions the status of a package names: what is desired in a group, the last good one, the
// bad ones, the plan's, a force's and the ones the hosts run; highest first, each once.
export function knownVersions(statuses) {
  const found = new Set();
  const add = (version) => {
    if (typeof version === "string" && version !== "") {
      found.add(version);
    }
  };
  for (const svc of statuses) {
    add(svc.desired && svc.desired.version);
    add(svc.plan && svc.plan.version);
    const marks = svc.marks || {};
    add(marks.lastGood);
    list(marks, "bad").forEach(add);
    list(marks, "badByForce").forEach(add);
    for (const host of list(svc, "hosts")) {
      add(host.view && host.view.installedVersion);
    }
  }
  return Array.from(found).sort(byVersionDescending);
}

// ---- progress ----

function desiredOf(svc) {
  return svc.desired && typeof svc.desired.version === "string" ? svc.desired.version : "";
}

// The one state a host of a status entry is counted in, against the version `desired`:
// `updated` (runs it), else `failed` (the update failed on it), else `updating` (holds an update
// slot, or is ordered the update), else `held` (an operator holds the package on it), else `down`
// (not live), else `pending` (waits its turn).
export function hostBucket(host, desired) {
  const view = host.view || {};
  if (desired !== "" && view.installedVersion === desired) {
    return "updated";
  }
  if (view.gateReason === "GATE_REASON_FAILED_HERE") {
    return "failed";
  }
  if (view.holdsSlot === true || view.gateReason === "GATE_REASON_UPDATING") {
    return "updating";
  }
  if (list(view, "holds").length > 0 || view.gateReason === "GATE_REASON_HELD") {
    return "held";
  }
  if (host["live"] === false) {
    return "down";
  }
  return "pending";
}

// How far the rollout of one status entry (one host group of the package) has come, worked out
// from the hosts' states: `total` hosts of the group, and how many of them are `updated`,
// `updating`, `failed`, `held`, `down` or `pending`: each host is in exactly one of these, so the
// six add up to `total`. Also `waiting` (the plan's reason for ordering no new host, "-" when
// none), `paused`, `finished` (every host runs the desired version, or the plan says every host
// rolled), `active` (a desired version is set and the rollout is not finished: the page polls
// while one is), and `percent` of `total`.
export function rolloutProgress(svc) {
  const desired = desiredOf(svc);
  const plan = svc.plan || null;
  const progress = {
    total: 0, updated: 0, updating: 0, failed: 0, held: 0, down: 0, pending: 0,
    waiting: "-", paused: false, finished: false, active: false, percent: 0,
  };
  for (const host of list(svc, "hosts")) {
    progress["total"] += 1;
    progress[hostBucket(host, desired)] += 1;
  }
  progress.paused = Boolean(plan) && plan.paused === true;
  if (plan && plan.waiting !== undefined) {
    progress.waiting = waitingText(plan);
  }
  progress.finished = desired !== "" && (Boolean(plan) && plan.finished === true
    || (progress["total"] > 0 && progress.updated === progress["total"]));
  progress.active = desired !== "" && !progress.finished && (progress["total"] > 0 || plan !== null);
  progress.percent = progress["total"] === 0 ? 0 : Math.floor((100 * progress.updated) / progress["total"]) | 0;
  return progress;
}

// The counts of all the groups of the package added up (a host that two groups list counts in
// each: the groups are separate rollouts).
export function totalProgress(all) {
  const sum = { total: 0, updated: 0, updating: 0, failed: 0, held: 0, down: 0, pending: 0 };
  for (const one of all) {
    for (const key of Object.keys(sum)) {
      sum[key] += one[key];
    }
  }
  return sum;
}

// The counts in words, the states that have a host only: `8 of 13 updated, 1 updating, 1 failed`.
function countWords(progress) {
  const counts = [`${progress.updated} of ${progress["total"]} updated`];
  for (const key of ["updating", "failed", "held", "down"]) {
    if (progress[key] > 0) {
      counts.push(`${progress[key]} ${key}`);
    }
  }
  return counts.join(", ");
}

// The word and tone of the state of one group.
function groupState(svc, progress) {
  const state = svc.convergence && svc.convergence.state;
  if (progress.paused) {
    return ["Paused", "warn"];
  }
  if (state === "CONVERGENCE_STATE_HALTED") {
    return ["Halted", "bad"];
  }
  if (state === "CONVERGENCE_STATE_BLOCKED" || progress.failed > 0) {
    return ["Blocked", "bad"];
  }
  if (desiredOf(svc) === "") {
    return ["No version set", "muted"];
  }
  return progress.active ? ["Updating", "info"] : ["Up to date", "ok"];
}

// The word and tone of the package as a whole: the worst state of its groups.
export function overallState(statuses, progress) {
  if (statuses.length === 0) {
    return ["Not rolled out", "muted"];
  }
  const states = statuses.map((svc, i) => groupState(svc, progress[i]));
  for (const word of ["Halted", "Blocked", "Paused", "Updating", "No version set"]) {
    const found = states.find(([label]) => label === word);
    if (found) {
      return found;
    }
  }
  return ["Up to date", "ok"];
}

function pill(tone, text) {
  return h("span", { class: `pk-pill ${tone}` }, text);
}

// The bar and words of one group: a native `progress` element, and the counts in words beside it
// (a failure or a hold is never a colour alone).
export function progressBar(group, progress) {
  return h("div", { class: "rollout-bar mini" },
    h("progress", {
      name: `progress-${group}`, value: progress.updated, max: progress["total"] || 1,
      "aria-label": `${group}: ${progress.percent}% updated`,
    }),
    h("span", { name: `progress-text-${group}`, class: "counts" }, countWords(progress)));
}

// The versions the hosts of a status entry run, as `1.3.2 (4 hosts)`, highest first.
function currentVersions(svc) {
  const counts = new Map();
  for (const host of list(svc, "hosts")) {
    const version = host.view && typeof host.view.installedVersion === "string" ? host.view.installedVersion : "none";
    counts.set(version, (counts.get(version) || 0) + 1);
  }
  return Array.from(counts)
    .sort(([a], [b]) => (a === "none" ? 1 : b === "none" ? -1 : byVersionDescending(a, b)))
    .map(([version, n]) => `${version} (${n} ${n === 1 ? "host" : "hosts"})`)
    .join(", ") || "no host";
}

// The minimum version of the package in `group`, from the package's entry of Services.List.
function minimumOf(entry, group) {
  const min = list(entry, "minVersions").find((one) => one["role"] === group);
  return min === undefined ? "none" : min.version;
}

function groupName(svc) {
  return typeof svc.hostGroup === "string" && svc.hostGroup !== "" ? svc.hostGroup : "-";
}

// ---- pieces ----

function fact(label, value) {
  return h("div", { class: "fact" }, h("span", { class: "k" }, label), h("span", { class: "v" }, value));
}

// A button that opens or closes the form `name` of the page.
function opener(state, redraw, name, label, extra) {
  return h("button", {
    type: "button", name: `open-${name}`, class: `${state.panel === name ? "on " : ""}${extra || ""}`.trim(),
    on: { click: () => { state.panel = state.panel === name ? "" : name; redraw(); } },
  }, label);
}

// The change handler of a bound select, followed by a redraw.
function redrawOnChange(control, redraw) {
  const chooser = control.children[control.children.length - 1];
  const bound = chooser.on.change;
  chooser.on.change = (event) => {
    bound(event);
    redraw();
  };
  return control;
}

// A hold form's target: a choice of the host groups or of the hosts when the page knows them,
// else the name typed.
function holdTarget(form, statuses) {
  if (form.scope === "fleet") {
    return null;
  }
  const hosts = [];
  for (const svc of statuses) {
    for (const host of list(svc, "hosts")) {
      if (typeof host.nodeId === "string" && !hosts.includes(host.nodeId)) {
        hosts.push(host.nodeId);
      }
    }
  }
  const known = form.scope === "role" ? statuses.map(groupName).filter((g) => g !== "-") : hosts;
  const label = form.scope === "role" ? "Host group" : "Host";
  if (known.length === 0) {
    return field(label, "hold-value", form, "value");
  }
  if (!known.includes(form.value)) {
    form.value = known[0];
  }
  return select(label, "hold-value", form, "value", known.map((one) => [one, one]));
}

// The holds on the package, and the release of a hold.
function holdsCard(ctx, pkg, holds, form) {
  const rows = holds.map((hold) => h("tr", {},
    h("td", {}, scopeLabel(hold.scope)),
    h("td", {}, hold.reason || "no reason given"),
    h("td", {}, hold.setBy || "unknown"),
    h("td", {}, fmtUnix(hold.setUnix)),
    h("td", {}, action(ctx, "Release", "unhold", async () => {
      const reason = trimmed(form.releaseReason);
      if (reason === "") {
        return "Give a reason for the release first.";
      }
      if (reasonProblem(reason) !== null) {
        return reasonProblem(reason);
      }
      await ctx.api.services.unhold({ scope: hold.scope, service: pkg, reason });
      ctx.refresh();
      return `Released the hold of ${pkg} (${scopeLabel(hold.scope)}).`;
    }))));
  return h("section", { class: "card holds" },
    h("h3", {}, `Holds on ${pkg} (${holds.length})`),
    holds.length === 0
      ? h("p", { class: "note" }, "No hold stands. A held package is ordered nothing where it is held.")
      : [
        table(["Scope", "Reason", "Set by", "Set at", ""], rows),
        field("Reason for a release", "unhold-reason", form, "releaseReason", { maxlength: 256 }),
      ]);
}

// The form of a new hold, opened by its button.
function holdPanel(ctx, pkg, form, statuses, redraw) {
  return h("div", { class: "panel", name: "panel-hold" },
    h("h3", { class: "panel-title" }, `Hold ${pkg}`),
    h("p", { class: "note" },
      "A held package is ordered nothing where it is held: no update, install, rollback, uninstall or force without include held."),
    redrawOnChange(select("Scope", "hold-scope", form, "scope",
      [["host", "a host"], ["role", "a host group"], ["fleet", "the whole fleet"]]), redraw),
    holdTarget(form, statuses),
    field("Reason (required)", "hold-reason", form, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" },
      action(ctx, "Hold", "hold", async () => {
        const reason = trimmed(form.reason);
        if (reason === "") {
          return "A hold needs a reason.";
        }
        if (reasonProblem(reason) !== null) {
          return reasonProblem(reason);
        }
        const scope = holdScope(form);
        if (typeof scope === "string") {
          return scope;
        }
        await ctx.api.services.hold({ scope, service: pkg, reason });
        ctx.refresh();
        return `Held ${pkg} (${scopeLabel(scope)}).`;
      })));
}

// The form of the desired version of one host group.
function setPanel(ctx, statuses, state, redraw) {
  const groups = statuses.map(groupName);
  if (!groups.includes(state.setGroup)) {
    state.setGroup = groups[0];
  }
  const svc = statuses[groups.indexOf(state.setGroup)];
  return h("div", { class: "panel", name: "panel-set" },
    h("h3", { class: "panel-title" }, `Set the desired version of ${svc.service}`),
    groups.length > 1
      ? redrawOnChange(select("Host group", "set-group", state, "setGroup", groups.map((g) => [g, g])), redraw)
      : null,
    field("Version", "desired-version", state.set, "version"),
    field("Release index digest (64 hex characters)", "desired-index-digest", state.set, "indexDigest"),
    h("div", { class: "buttons" }, setDesired(ctx, svc, state.set)));
}

// The reason an act is sent with: typed, not empty, and one the API takes.
function withReasonOf(form) {
  return (run) => async () => {
    if (trimmed(form.reason) === "") {
      return "Give a reason first.";
    }
    return reasonProblem(trimmed(form.reason)) || run(trimmed(form.reason));
  };
}

// The form of the plan: Advance, Pause or Resume.
function planPanel(ctx, pkg, plan, state) {
  return h("div", { class: "panel", name: "panel-plan" },
    h("h3", { class: "panel-title" }, `${plan.paused === true ? "Resume" : "Pause"} the plan of ${pkg}`),
    field("Reason", "reason", state.act, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" }, planButtons(ctx, pkg, plan, withReasonOf(state.act))));
}

// The plan the plan buttons are for: the one that waits for an Advance, else a paused one, else
// the first (a plan is the package's, not a host group's: Pause and Resume name the package).
function planOf(statuses) {
  const plans = statuses.map((svc) => svc.plan).filter((plan) => Boolean(plan) && plan.finished !== true);
  return plans.find((plan) => plan.waiting === "PLAN_WAIT_MANUAL_ADVANCE")
    || plans.find((plan) => plan.paused === true) || plans[0] || null;
}

// The danger zone: the acts that mark a version bad, halt the package or force a version. It is
// closed until the operator opens it.
function dangerZone(ctx, pkg, statuses, versions, state, redraw) {
  const toggle = h("button", {
    type: "button", name: "open-danger", class: `collapser danger-toggle${state.danger ? " is-open" : ""}`,
    on: { click: () => { state.danger = !state.danger; redraw(); } },
  }, "Danger zone");
  if (!state.danger) {
    return h("section", { class: "card danger-zone" }, toggle);
  }
  const form = state.act;
  const bad = Array.from(new Set(statuses.flatMap((svc) => list(svc.marks || {}, "bad"))));
  const halted = statuses.some((svc) => Boolean((svc.marks || {}).halt)
    || (svc.convergence && svc.convergence.state === "CONVERGENCE_STATE_HALTED"));
  const withReason = withReasonOf(form);
  if (!versions.includes(form.version)) {
    form.version = versions[0] || "";
  }
  return h("section", { class: "card danger-zone is-open" }, toggle,
    h("p", { class: "note" },
      "These acts stop updates, mark a version bad, or switch hosts at once. Each is audited, and the ones that cut running work ask first."),
    field("Reason", "reason", form, "reason", { maxlength: 256 }),
    versions.length === 0 ? null : select("Version to mark bad", "bad-version", form, "version", versions.map((v) => [v, v])),
    h("div", { class: "buttons" },
      action(ctx, "Mark bad", "mark-bad", withReason(async (reason) => {
        const version = trimmed(form.version);
        if (version === "") {
          return "Name the version to mark bad.";
        }
        await ctx.api.releases.markBad({ service: pkg, version, reason });
        ctx.refresh();
        return `Marked ${pkg} ${version} bad.`;
      }), { class: "danger" }),
      bad.map((version) => action(ctx, `Unmark ${version}`, "unmark-bad", withReason(async (reason) => {
        await ctx.api.releases.unmarkBad({ service: pkg, version, reason });
        ctx.refresh();
        return `Unmarked ${pkg} ${version}.`;
      }))),
      halted
        ? action(ctx, "Clear the halt", "clear-halt", withReason(async (reason) => {
          await ctx.api.releases.clearHalt({ service: pkg, reason });
          ctx.refresh();
          return `Cleared the halt of ${pkg}.`;
        }))
        : action(ctx, "Halt", "halt", withReason(async (reason) => {
          if (!ctx.confirm(`Halt ${pkg}? No host of it is ordered until the halt is cleared.`)) {
            return "Not halted.";
          }
          await ctx.api.releases.halt({ service: pkg, reason });
          ctx.refresh();
          return `Halted ${pkg}.`;
        }), { class: "danger" })),
    statuses.length === 0 ? null : forceSection(ctx, [{ service: pkg }], state, redraw));
}

// The group's row of the rollout card: its name and state in words, what is desired, minimum and
// run, the small bar, and the plan in one line; the failed hosts by name.
function groupRow(ctx, svc, progress, entry) {
  const group = groupName(svc);
  const [label, tone] = groupState(svc, progress);
  const desired = desiredOf(svc);
  const plan = svc.plan;
  const marks = svc.marks || {};
  const bad = list(marks, "bad");
  const attention = list(svc, "hosts").filter((host) => hostBucket(host, desired) === "failed");
  const notes = [];
  if (plan && planSummary(plan) !== "none") {
    notes.push(`Plan ${planSummary(plan)}, stage ${stageProgress(plan)}`);
  }
  if (progress.waiting !== "-") {
    notes.push(progress.waiting);
  }
  if (typeof marks.lastGood === "string") {
    notes.push(`Last good ${marks.lastGood}`);
  }
  if (bad.length > 0) {
    notes.push(`Bad ${bad.join(", ")}`);
  }
  return h("div", { class: "group-row", name: `group-${group}` },
    h("div", { class: "group-head" },
      h("strong", { class: "group-name" }, group),
      pill(tone, label),
      h("span", { class: "group-facts" },
        `Desired ${desired === "" ? "none set" : desired} / minimum ${minimumOf(entry, group)} / running ${currentVersions(svc)}`)),
    progressBar(group, progress),
    notes.length === 0 ? null : h("p", { class: "note group-notes" }, notes.join(" / ")),
    attention.length === 0 ? null : h("p", { class: "group-failed" }, "Failed on ",
      attention.map((host) => h("button", {
        type: "button", class: "linklike", name: "open-failed-host",
        on: { click: () => ctx.navigate("host", { nodeId: host.nodeId }) },
      }, host.nodeId))));
}

// The manifest of the selected version in a collapsible code view: the reply as the API gave it,
// or read as tables with the entry each host is picked.
function manifestCard(ctx, pkg, state, redraw) {
  const shown = state.version;
  if (shown === "") {
    return h("section", { class: "card manifest" }, h("h3", {}, "Manifest"),
      h("p", { class: "note" }, "No version is known of this package yet."));
  }
  const toggle = h("button", {
    type: "button", name: "manifest-open", class: `collapser${state.manifestOpen ? " is-open" : ""}`,
    on: { click: () => { state.manifestOpen = !state.manifestOpen; redraw(); } },
  }, `Manifest of ${pkg} ${shown}`);
  if (!state.manifestOpen) {
    return h("section", { class: "card manifest" }, toggle);
  }
  const switcher = h("button", {
    type: "button", name: "manifest-toggle",
    on: { click: () => { state.raw = !state.raw; redraw(); } },
  }, state.raw ? "Readable" : "Raw");
  const head = h("div", { class: "manifest-head" }, toggle, switcher);
  if (state.manifest.error !== undefined) {
    return h("section", { class: "card manifest" }, head,
      h("p", { class: "error", role: "alert" }, describeError(state.manifest.error)));
  }
  const release = state.manifest.release;
  if (state.raw) {
    return h("section", { class: "card manifest" }, head,
      h("pre", { name: "manifest-raw", class: "code" }, JSON.stringify(release, null, 2)));
  }
  const targets = release.targets || {};
  const keys = platformOrder(targets);
  return h("section", { class: "card manifest" }, head,
    table(["", ""], [
      ["Release index digest", digestCell(release.releaseIndexDigest)],
      ["Platforms", keys.length === 0 ? "none" : keys.join(", ")],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    keys.length === 0 ? h("p", {}, "This release names no targets.") : keys.map((key) => platformSection(key, targets[key])),
    state.manifest.hostsError !== undefined
      ? h("p", { class: "error", role: "alert" }, `The hosts could not be read: ${describeError(state.manifest.hostsError)}`)
      : picksSection(ctx, pkg, targets, state.manifest.hosts));
}

// Reads the manifest of `state.version` and, when it answers, the hosts for the picks.
async function loadManifest(ctx, pkg, state, groups) {
  if (state.version === "") {
    state.manifest = {};
    return;
  }
  const request = { service: pkg, version: state.version };
  if (state["group"] !== "") {
    request.hostGroup = state["group"];
  } else if (groups.length === 1 && groups[0] !== "-") {
    request.hostGroup = groups[0];
  }
  try {
    state.manifest = { release: await ctx.api.releases.getRelease(request) };
  } catch (error) {
    state.manifest = { error };
    return;
  }
  try {
    state.manifest.hosts = await allPages((pageToken) => ctx.api.hosts.list(
      pageToken === "" ? {} : { pageToken }), "hosts");
  } catch (error) {
    state.manifest.hostsError = error;
  }
}

// ---- trusted publishers ----

// The reasons sent with a write to a package's trusted publishers.
const PUT_REASON = "set in the console";
const REMOVE_REASON = "removed in the console";

// The rules of the API, checked here so the form answers before it sends.
const REPOSITORY = /^[A-Za-z0-9_.-]{1,100}\/[A-Za-z0-9_.-]{1,100}$/;
const WORKFLOW = /^\.github\/workflows\/[A-Za-z0-9_.-]+\.ya?ml$/;
const BRANCH = /^[!-~]{1,150}$/;
const BRANCH_FORBIDDEN = /[~^:?*[\\@]/;
const ENVIRONMENT = /^[ -~]{1,100}$/;

// A branch as the entry holds it (`refs/heads/main`), shown as the operator types it (`main`).
function branchOf(ref) {
  if (ref === "") {
    return "any";
  }
  return ref.startsWith("refs/heads/") ? ref.slice("refs/heads/".length) : ref;
}

// What the allow-list file pinned on an imported entry, in words; "" for an entry made here.
function pinned(entry) {
  const parts = [];
  if (entry.repositoryId) {
    parts.push(`repository id ${entry.repositoryId}`);
  }
  const called = list(entry, "calledWorkflows");
  if (called.length > 0) {
    parts.push(`calls ${called.join(", ")}`);
  }
  if (entry.limitedToHostGroups === true) {
    const groups = list(entry, "hostGroups");
    parts.push(groups.length === 0 ? "deploys to no host group" : `deploys to host group ${groups.join(", ")}`);
  }
  return parts.join("; ");
}

// What the section holds for `pkg`: its entries, or why they could not be read, and the form.
async function loadPublishers(ctx, pkg) {
  const state = {
    entries: [], failure: null, adding: false,
    form: { repository: "", workflow: "", ref: "main", environment: "" },
  };
  try {
    state.entries = list(await ctx.api.services.listTrustedPublishers({ service: pkg }), "publishers");
  } catch (error) {
    state.failure = error;
  }
  return state;
}

// The section for `pkg`; `redraw` draws the page again after the entries change.
function publishersSection(ctx, pkg, state, redraw) {
  const key = (entry) => ({
    service: pkg, repository: entry.repository || "", workflow: entry.workflow || "", ref: entry.ref || "",
    environment: entry.environment || "",
  });

  const add = action(ctx, "Add trusted publisher", "add-publisher", async () => {
    const { form } = state;
    const repository = trimmed(form.repository);
    const workflow = trimmed(form.workflow);
    const ref = trimmed(form.ref) === "" ? "main" : trimmed(form.ref);
    const environment = trimmed(form.environment);
    if (!REPOSITORY.test(repository)) {
      return "Name the repository as owner/name, for example komira-ai/tierstore.";
    }
    if (!WORKFLOW.test(workflow)) {
      return "Name the workflow file under .github/workflows, for example .github/workflows/release.yml.";
    }
    if (!BRANCH.test(ref) || BRANCH_FORBIDDEN.test(ref) || ref.includes("..")) {
      return "Name the branch, for example main (or a full ref such as refs/tags/v1).";
    }
    if (environment !== "" && !ENVIRONMENT.test(environment)) {
      return "An environment is printable text of at most 100 characters.";
    }
    const request = { service: pkg, repository, workflow, ref, reason: PUT_REASON };
    if (environment !== "") {
      request.environment = environment;
    }
    try {
      state.entries = list(await ctx.api.services.putTrustedPublisher(request), "publishers");
    } catch (error) {
      return `${pkg}: ${describeError(error)}`;
    }
    state.form = { repository: "", workflow: "", ref: "main", environment: "" };
    state.adding = false;
    redraw();
    return `${repository} ${workflow} may now set ${pkg}.`;
  });

  const remove = (entry) => action(ctx, "Remove", "remove-publisher", async () => {
    const shown = `${entry.repository || "any repository"} ${entry.workflow || "any workflow"}`;
    if (!ctx.confirm(`Remove ${shown}? Its runs can no longer set ${pkg}; the next call is refused at once.`)) {
      return "Not removed.";
    }
    try {
      state.entries = list(await ctx.api.services.removeTrustedPublisher({ ...key(entry), reason: REMOVE_REASON }), "publishers");
    } catch (error) {
      return `${pkg}: ${describeError(error)}`;
    }
    redraw();
    return `Removed ${shown}.`;
  });

  const rows = () => state.entries.map((entry) => h("tr", {},
    h("td", {}, entry.repository || "any"),
    h("td", {}, entry.workflow || "any workflow of the repository"),
    h("td", {}, branchOf(entry.ref || "")),
    h("td", {}, entry.environment || "-"),
    h("td", {}, pinned(entry) || "-"),
    h("td", {}, remove(entry))));

  const toggle = h("button", {
    type: "button", name: "open-publisher-form", class: state.adding ? "on" : "",
    on: { click: () => { state.adding = !state.adding; redraw(); } },
  }, "Add");
  return h("section", { class: "card publishers" },
    h("div", { class: "rollout-top" }, h("h3", {}, "Trusted publishers"), toggle),
    h("p", { class: "note" },
      `The GitHub Actions runs that may set the desired version of ${pkg}. A run is trusted when its repository, workflow file and branch match one entry. With no entry, no CI call sets this package.`),
    state.failure !== null ? h("p", { class: "error", role: "alert" }, `Trusted publishers: ${describeError(state.failure)}`) : null,
    state.failure === null && state.entries.length === 0
      ? h("p", { class: "note", name: "no-publishers" }, `No run is trusted to set ${pkg}.`) : null,
    state.entries.length > 0
      ? table(["Repository", "Workflow file", "Branch", "Environment", "Pinned by the allow-list file", ""], rows())
      : null,
    state.adding
      ? h("div", { class: "panel", name: "panel-publisher" },
        h("h3", { class: "panel-title" }, "Add trusted publisher"),
        field("Repository", "publisher-repository", state.form, "repository", { placeholder: "owner/name" }),
        field("Workflow file", "publisher-workflow", state.form, "workflow", { placeholder: ".github/workflows/release.yml" }),
        field("Branch", "publisher-ref", state.form, "ref"),
        field("Environment (optional)", "publisher-environment", state.form, "environment"),
        h("div", { class: "buttons" }, add))
      : null);
}

export async function render(ctx) {
  const pkg = param(ctx, "package");
  if (pkg === "") {
    return h("section", { class: "page" }, h("h2", {}, "Package"),
      h("p", {}, "Choose a package on the Packages page."),
      action(ctx, "Packages", "open-packages", async () => {
        ctx.navigate("packages", {});
        return undefined;
      }));
  }
  let statuses;
  try {
    statuses = list(await ctx.api.releases.status({ service: pkg }), "services").filter((svc) => svc.service === pkg);
  } catch (error) {
    return errorView(`Package ${pkg}`, error);
  }
  let listing = { entries: [], holds: [] };
  let listingError = null;
  try {
    const reply = await ctx.api.services.list({});
    listing = { entries: list(reply, "services"), holds: list(reply, "holds") };
  } catch (error) {
    listingError = error;
  }
  let repositoryUri = "";
  try {
    const repositories = list(await ctx.api.repositories.list({}), "repositories");
    const entry = listing.entries.find((one) => one.service === pkg) || {};
    const found = repositories.find((one) => (one.repository || {}).name === entry.repository);
    repositoryUri = found && found.repository.uri ? found.repository.uri : "";
  } catch {
    repositoryUri = "";
  }
  const entry = listing.entries.find((one) => one.service === pkg) || {};
  const versions = knownVersions(statuses);
  const asked = param(ctx, "version");
  const firstDesired = statuses.map((svc) => svc.desired && svc.desired.version).find((v) => typeof v === "string" && v !== "");
  const state = {
    version: asked !== "" ? asked : (firstDesired || versions[0] || ""),
    group: param(ctx, "group"),
    raw: true,
    manifestOpen: false,
    manifest: {},
    panel: "",
    danger: false,
    setGroup: "",
    set: { version: "", indexDigest: "", index: "" },
    act: { reason: "", version: versions[0] || "" },
    hold: { scope: "host", value: "", reason: "", releaseReason: "" },
    force: { service: pkg, version: "", reason: "", targetKind: "fleet", role: "", hosts: "", includeHeld: false },
    phase: "idle",
    attempt: 0,
    prepared: null,
    typed: "",
    pollError: null,
  };
  const groups = statuses.map(groupName);
  await loadManifest(ctx, pkg, state, groups);
  const publishers = await loadPublishers(ctx, pkg);

  const page = () => {
    const redraw = () => ctx.show(page());
    const progress = statuses.map(rolloutProgress);
    const all = totalProgress(progress);
    const [overall, tone] = overallState(statuses, progress);
    const holds = listing.holds.filter((hold) => hold.service === pkg);
    const plan = planOf(statuses);
    const pickVersion = (version) => h("button", {
      type: "button", name: "pick-version", class: version === state.version ? "vchip current" : "vchip",
      on: {
        click: async () => {
          state.version = version;
          await loadManifest(ctx, pkg, state, groups);
          ctx.show(page());
        },
      },
    }, version);
    const desired = Array.from(new Set(statuses.map(desiredOf).filter((v) => v !== "")));
    const lastGood = statuses.map((svc) => (svc.marks || {}).lastGood).find((v) => typeof v === "string");
    const panels = {
      set: statuses.length === 0 ? null : () => setPanel(ctx, statuses, state, redraw),
      plan: plan === null ? null : () => planPanel(ctx, pkg, plan, state),
      hold: () => holdPanel(ctx, pkg, state.hold, statuses, redraw),
    };
    const open = panels[state.panel] ? panels[state.panel]() : null;
    const percent = all["total"] === 0 ? 0 : Math.floor((100 * all.updated) / all["total"]);
    return h("section", { class: "page package" },
      h("section", { class: "card pkg-head" },
        h("div", { class: "pkg-title" }, h("h2", {}, pkg), pill(tone, overall)),
        h("p", { name: "repository", class: "repo" },
          `Repository: ${entry.repository || "none"}${repositoryUri === "" ? "" : ` (${repositoryUri})`}`),
        h("div", { class: "facts" },
          fact("Latest version", versions[0] || "none known"),
          fact("Desired", desired.length === 0 ? "none set" : desired.join(", ")),
          fact("Last good", lastGood || "none")),
        h("p", { name: "versions", class: "versions" }, "Versions: ",
          versions.length === 0 ? "none known" : versions.map((v) => [pickVersion(v), " "]))),
      listingError !== null ? h("p", { class: "error", role: "alert" }, `Packages: ${describeError(listingError)}`) : null,
      state.pollError !== null
        ? h("p", { class: "error", role: "alert" }, `Progress was not refreshed: ${describeError(state.pollError)}`) : null,
      h("section", { class: "card rollout-card", name: "rollout" },
        h("div", { class: "rollout-top" },
          h("h3", {}, "Rollout"),
          h("div", { class: "actions" },
            statuses.length === 0 ? null : opener(state, redraw, "set", "Set version", "primary"),
            plan === null ? null : opener(state, redraw, "plan", plan.paused === true ? "Resume" : "Pause"),
            opener(state, redraw, "hold", "Hold"),
            statuses.length === 0 ? null : action(ctx, "Check convergence", "converge", async () => {
              const reply = await ctx.api.releases.converge({ service: pkg });
              const c = reply.convergence || {};
              return `${pkg}: ${c.state === "CONVERGENCE_STATE_CONVERGED" ? "converged" : "not converged yet"}, desired ${c.desired || "none"}`
                + (c.quiet === true ? ", quiet start" : "") + ".";
            }))),
        open,
        statuses.length === 0
          ? h("p", { class: "note" }, `No host group runs ${pkg}.`)
          : [
            h("div", { class: "rollout-big" },
              h("progress", {
                name: "progress-total", value: all.updated, max: all["total"] || 1,
                "aria-label": `${percent}% of the hosts updated`,
              }),
              h("p", { class: "big-text", name: "progress-total-text" },
                `${all.updated} of ${all["total"]} hosts updated${all.failed > 0 ? `, ${all.failed} failed` : ""}`)),
            h("div", { class: "pills" },
              all.updating > 0 ? pill("info", `${all.updating} updating`) : null,
              all.pending > 0 ? pill("muted", `${all.pending} waiting`) : null,
              all.held > 0 ? pill("warn", `${all.held} held`) : null,
              all.down > 0 ? pill("warn", `${all.down} down`) : null,
              all.failed > 0 ? pill("bad", `${all.failed} failed`) : null),
            h("div", { class: "groups" }, statuses.map((svc, i) => groupRow(ctx, svc, progress[i], entry))),
          ]),
      manifestCard(ctx, pkg, state, redraw),
      holdsCard(ctx, pkg, holds, state.hold),
      publishersSection(ctx, pkg, publishers, redraw),
      dangerZone(ctx, pkg, statuses, versions, state, redraw));
  };

  const tree = page();
  if (statuses.map(rolloutProgress).some((p) => p.active)) {
    const stop = ctx.every(async () => {
      try {
        statuses = list(await ctx.api.releases.status({ service: pkg }), "services").filter((svc) => svc.service === pkg);
        state.pollError = null;
      } catch (error) {
        state.pollError = error;
      }
      if (!statuses.map(rolloutProgress).some((p) => p.active)) {
        stop();
      }
      ctx.show(page());
    }, REFRESH_SECONDS);
  }
  return tree;
}
