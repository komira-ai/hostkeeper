// Package: one package of the registry, reached by clicking it on the Packages page. Its
// repository and the versions known of it; per host group, the desired version against the
// versions the hosts run, with a progress bar while a rollout is under way (hosts updated of the
// hosts of the group, the failures and the holds); the manifest of a version as Releases.GetRelease
// answers it, readable or raw, with the entry each host of the package is picked; the holds that
// stand on the package; its trusted publishers, the GitHub Actions runs that may set its desired
// version; and the acts on it: set the desired version of a group, pause, resume or
// advance its rollout, hold or release a host, a role or the fleet, mark a version bad, halt, and
// force. These are what the Rollout, Release and Holds pages did; their routes lead here.
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
  h, action, allPages, describeError, enumLabel, errorView, field, fmtUnix, list, reasonProblem, scopeLabel, select,
  table, trimmed,
} from "../view.js";
import { holdScope } from "./holds.js";
import { digestCell, picksSection, platformOrder, platformSection } from "./release.js";
import {
  actFieldset, forceSection, planSummary, planWaiting, setDesired, stageProgress,
} from "./rollout.js";

// How often a page with a rollout under way asks for the status again.
export const REFRESH_SECONDS = 5;

function param(ctx, name) {
  return ctx.params && typeof ctx.params[name] === "string" ? ctx.params[name] : "";
}

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

// How far the rollout of one status entry (one host group of the package) has come, worked out
// from the hosts' states: `total` hosts of the group, `updated` (run the desired version),
// `updating` (hold an update slot, or are ordered the update), `failed` (the update failed on
// them), `held` (an operator holds the package on them), `down` (not live), `waiting` (the plan's
// reason for ordering no new host, "-" when none), `paused`, `finished` (every host runs the
// desired version, or the plan says every host rolled), `active` (a desired version is set and
// the rollout is not finished: the page polls while one is), and `percent` of `total`.
export function rolloutProgress(svc) {
  const desired = svc.desired && typeof svc.desired.version === "string" ? svc.desired.version : "";
  const plan = svc.plan || null;
  const progress = {
    total: 0, updated: 0, updating: 0, failed: 0, held: 0, down: 0,
    waiting: "-", paused: false, finished: false, active: false, percent: 0,
  };
  for (const host of list(svc, "hosts")) {
    const view = host.view || {};
    progress["total"] += 1;
    const updated = desired !== "" && view.installedVersion === desired;
    if (updated) {
      progress.updated += 1;
    } else if (view.holdsSlot === true || view.gateReason === "GATE_REASON_UPDATING") {
      progress.updating += 1;
    }
    if (view.gateReason === "GATE_REASON_FAILED_HERE") {
      progress.failed += 1;
    }
    if (list(view, "holds").length > 0 || view.gateReason === "GATE_REASON_HELD") {
      progress.held += 1;
    }
    if (host["live"] === false) {
      progress.down += 1;
    }
  }
  progress.paused = Boolean(plan) && plan.paused === true;
  if (plan && plan.waiting !== undefined) {
    progress.waiting = planWaiting(plan);
  }
  progress.finished = desired !== "" && (Boolean(plan) && plan.finished === true
    || (progress["total"] > 0 && progress.updated === progress["total"]));
  progress.active = desired !== "" && !progress.finished && (progress["total"] > 0 || plan !== null);
  progress.percent = progress["total"] === 0 ? 0 : Math.floor((100 * progress.updated) / progress["total"]) | 0;
  return progress;
}

// The progress bar of a rollout: a native `progress` element, with the counts in words beside it
// (a failure or a hold is never a colour alone).
export function progressBar(group, progress) {
  const counts = [`${progress.updated} of ${progress["total"]} updated`];
  if (progress.updating > 0) {
    counts.push(`${progress.updating} updating`);
  }
  if (progress.failed > 0) {
    counts.push(`${progress.failed} failed`);
  }
  if (progress.held > 0) {
    counts.push(`${progress.held} held`);
  }
  if (progress.down > 0) {
    counts.push(`${progress.down} down`);
  }
  return h("div", { class: "rollout-bar" },
    h("progress", {
      name: `progress-${group}`, value: progress.updated, max: progress["total"] || 1,
      "aria-label": `${group}: ${progress.percent}% updated`,
    }),
    h("span", { name: `progress-text-${group}` }, ` ${counts.join(", ")}`));
}

// The versions the hosts of a status entry run, as `1.3.2 (4 hosts)`, highest first.
function currentVersions(svc) {
  const counts = new Map();
  for (const host of list(svc, "hosts")) {
    const version = host.view && typeof host.view.installedVersion === "string" ? host.view.installedVersion : "-";
    counts.set(version, (counts.get(version) || 0) + 1);
  }
  return Array.from(counts)
    .sort(([a], [b]) => (a === "-" ? 1 : b === "-" ? -1 : byVersionDescending(a, b)))
    .map(([version, n]) => `${version} (${n} ${n === 1 ? "host" : "hosts"})`)
    .join(", ") || "-";
}

// The minimum version of the package in `group`, from the package's entry of Services.List.
function minimumOf(entry, group) {
  const min = list(entry, "minVersions").find((one) => one["role"] === group);
  return min === undefined ? "-" : min.version;
}

function groupName(svc) {
  return typeof svc.hostGroup === "string" && svc.hostGroup !== "" ? svc.hostGroup : "-";
}

// The Hold form's state, and the Release of a hold.
function holdsSection(ctx, pkg, holds, state) {
  const form = state.hold;
  const rows = holds.map((hold) => h("tr", {},
    h("td", {}, scopeLabel(hold.scope)),
    h("td", {}, hold.reason || "-"),
    h("td", {}, hold.setBy || "-"),
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
  return h("section", { class: "holds" },
    h("h3", {}, `Holds on ${pkg} (${holds.length})`),
    h("p", { class: "note" },
      "A held package is ordered nothing where it is held: no update, install, rollback, uninstall or force without include held."),
    holds.length === 0 ? h("p", {}, "No hold stands.") : table(["Scope", "Reason", "Set by", "Set at", ""], rows),
    holds.length === 0 ? null : field("Reason for a release", "unhold-reason", form, "releaseReason", { maxlength: 256 }),
    h("fieldset", {},
      h("legend", {}, `Hold ${pkg}`),
      select("Scope", "hold-scope", form, "scope", [["host", "a host"], ["role", "a host group"], ["fleet", "the whole fleet"]]),
      field("Host or host group", "hold-value", form, "value"),
      field("Reason (required)", "hold-reason", form, "reason", { maxlength: 256 }),
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

// The card of one host group: what it is set to, the bar while its rollout runs, and its acts.
function groupCard(ctx, svc, progress, state) {
  const group = groupName(svc);
  const forms = state.forms[group] || (state.forms[group] = {
    desired: { version: "", indexDigest: "", index: "" },
    act: { reason: "", version: "" },
  });
  const plan = svc.plan;
  return h("section", { class: "service rollout", name: `group-${group}` },
    h("h3", {}, `${group}: ${progress.active ? "rollout under way" : "no rollout under way"}`),
    progressBar(group, progress),
    table(["", ""], [
      ["Desired", svc.desired && svc.desired.version ? svc.desired.version : "none set"],
      ["State", enumLabel(svc.convergence && svc.convergence.state, "CONVERGENCE_STATE_")],
      ["Plan", planSummary(plan)],
      ["Stage", stageProgress(plan)],
      ["Waiting", planWaiting(plan)],
      ["Last good", svc.marks && typeof svc.marks.lastGood === "string" ? svc.marks.lastGood : "-"],
      ["Bad versions", ((svc.marks || {}).bad || []).join(", ") || "-"],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    h("fieldset", {},
      h("legend", {}, `Set the desired version of ${svc.service} in ${group}`),
      field("Version", "desired-version", forms.desired, "version"),
      field("Release index digest (64 hex characters)", "desired-index-digest", forms.desired, "indexDigest"),
      h("div", { class: "buttons" }, setDesired(ctx, svc, forms.desired))),
    actFieldset(ctx, svc, forms.act));
}

// The manifest of the selected version, readable or raw, and below it the entry each host is picked.
function manifestSection(ctx, pkg, state, redraw) {
  const shown = state.version;
  if (shown === "") {
    return h("section", { class: "manifest" }, h("h3", {}, "Manifest"), h("p", {}, "No version is known of this package yet."));
  }
  const toggle = h("button", {
    type: "button", name: "manifest-toggle",
    on: { click: () => { state.raw = !state.raw; redraw(); } },
  }, state.raw ? "Readable" : "Raw");
  const head = [h("h3", {}, `Manifest of ${pkg} ${shown}`), toggle];
  if (state.manifest.error !== undefined) {
    return h("section", { class: "manifest" }, head,
      h("p", { class: "error", role: "alert" }, describeError(state.manifest.error)));
  }
  const release = state.manifest.release;
  if (state.raw) {
    return h("section", { class: "manifest" }, head, h("pre", { name: "manifest-raw" }, JSON.stringify(release, null, 2)));
  }
  const targets = release.targets || {};
  const keys = platformOrder(targets);
  return h("section", { class: "manifest" }, head,
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
    entries: [], failure: null,
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

  return h("section", { class: "publishers" },
    h("h3", {}, "Trusted publishers"),
    h("p", { class: "note" },
      `The GitHub Actions runs that may set the desired version of ${pkg}. A run is trusted when its repository, workflow file and branch match one entry. With no entry, no CI call sets this package.`),
    state.failure !== null ? h("p", { class: "error", role: "alert" }, `Trusted publishers: ${describeError(state.failure)}`) : null,
    state.failure === null && state.entries.length === 0
      ? h("p", { class: "note", name: "no-publishers" }, `No run is trusted to set ${pkg}.`) : null,
    state.entries.length > 0
      ? table(["Repository", "Workflow file", "Branch", "Environment", "Pinned by the allow-list file", ""], rows())
      : null,
    h("fieldset", {},
      h("legend", {}, "Add trusted publisher"),
      field("Repository", "publisher-repository", state.form, "repository", { placeholder: "owner/name" }),
      field("Workflow file", "publisher-workflow", state.form, "workflow", { placeholder: ".github/workflows/release.yml" }),
      field("Branch", "publisher-ref", state.form, "ref"),
      field("Environment (optional)", "publisher-environment", state.form, "environment"),
      h("div", { class: "buttons" }, add)));
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
    raw: false,
    manifest: {},
    forms: {},
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
    const progress = statuses.map(rolloutProgress);
    const holds = listing.holds.filter((hold) => hold.service === pkg);
    const pickVersion = (version) => h("button", {
      type: "button", name: "pick-version", class: version === state.version ? "linklike current" : "linklike",
      on: {
        click: async () => {
          state.version = version;
          await loadManifest(ctx, pkg, state, groups);
          ctx.show(page());
        },
      },
    }, version);
    return h("section", { class: "page package" },
      h("div", { class: "crumbs" },
        h("button", { type: "button", class: "linklike", name: "open-packages",
          on: { click: () => ctx.navigate("packages", {}) } }, "Packages"),
        ` / ${pkg}`),
      h("h2", {}, pkg),
      h("p", { name: "repository" }, `Repository: ${entry.repository || "-"}${repositoryUri === "" ? "" : ` (${repositoryUri})`}`),
      listingError !== null ? h("p", { class: "error", role: "alert" }, `Packages: ${describeError(listingError)}`) : null,
      state.pollError !== null
        ? h("p", { class: "error", role: "alert" }, `Progress was not refreshed: ${describeError(state.pollError)}`) : null,
      h("p", { name: "versions" }, "Versions: ", versions.length === 0 ? "none known" : versions.map((v) => [pickVersion(v), " "])),
      h("h3", {}, "Host groups"),
      statuses.length === 0
        ? h("p", {}, `No host group runs ${pkg}.`)
        : table(["Group", "Desired", "Minimum", "Current versions", "State", "Progress"],
          statuses.map((svc, i) => h("tr", {},
            h("td", {}, groupName(svc)),
            h("td", {}, svc.desired && svc.desired.version ? svc.desired.version : "none set"),
            h("td", {}, minimumOf(entry, groupName(svc))),
            h("td", {}, currentVersions(svc)),
            h("td", {}, enumLabel(svc.convergence && svc.convergence.state, "CONVERGENCE_STATE_")),
            h("td", {}, progress[i].total === 0 ? "-" : progressBar(`table-${groupName(svc)}`, progress[i]))))),
      statuses.map((svc, i) => groupCard(ctx, svc, progress[i], state)),
      manifestSection(ctx, pkg, state, () => ctx.show(page())),
      holdsSection(ctx, pkg, holds, state),
      publishersSection(ctx, pkg, publishers, () => ctx.show(page())),
      statuses.length === 0 ? null : forceSection(ctx, [{ service: pkg }], state, () => ctx.show(page())));
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
