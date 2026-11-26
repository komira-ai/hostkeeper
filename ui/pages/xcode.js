// Xcode: what the coordinator's Xcode store holds, the release that waits to be added, the uploads in
// progress, and for each Mac the Xcodes it has, what is due on it and whether device management
// blocks its updates (docs/design/xcode-one-action.md, section 4). It starts nothing on a Mac: the
// Update and Schedule buttons stay on the Maintenance page, so one page starts runs.
//
// Sections, in order:
// - Waiting: when the daily list names a release the store does not hold (`Xcode.List`'s `waiting`),
//   "Xcode 27.3 (27C21) is out and not in the store; it needs macOS 27.1" and the Add Xcode button,
//   which opens the panel of `xcodeadd.js` (the command to run on the operator's Mac, to copy).
//   With none waiting the page says the store holds the newest release, and the button opens the
//   panel of the bare command. Without a store (`configured` false) the page says so and has no
//   button.
// - Uploads: each partial upload, who started it and how far it is ("7.31 of 11.92 GB, last write 2
//   minutes ago", by the clock the page is given).
// - The store: each archive with its version, build, size, who uploaded it, when, and how many Macs
//   have it installed. Remove has a Reason field, as `hk xcode remove` does, and after one question
//   that names the Macs on which the archive is still due (a kept release they lack: the rule of
//   `xcodeplan.js`, which is the coordinator's), calls `Xcode.Remove` with the reason.
// - The Macs: one row per Mac (a host whose last heartbeat says Mac, or whose report has a Mac's
//   facts), in group order and Unassigned last: its group, the group's `xcode_keep` and
//   `xcode_default`, each Xcode installed (version, build, selected, foreign), what is due, its
//   device-management state (`nothing that blocks updates`, or the keys its report lists as
//   `mdmConflicts`), and the run of the Maintenance page that holds it, a link to that page.
//   A run holds a Mac when it is scheduled, running or halted, and names the Mac or its group. It
//   takes Xcode for the Mac only when the Mac's turn in it (`Maintenance.Get`) has the xcode kind;
//   else the row says "which does not take Xcode: schedule after it ends".
//
// Calls: Xcode.List, Hosts.List (every page) and HostGroups.List (every page) when opened and again
// every REFRESH_SECONDS through `ctx.every`, and Maintenance.List (the last 10 runs) and Maintenance.Get
// of each of them that still holds its hosts (each host's kinds) with them; while
// the panel is open, Xcode.List alone every PANEL_SECONDS. A read again that fails keeps the last
// answer and says so; a run list that fails is said and the rest of the page stays. Xcode.Remove,
// one call for each click.

import {
  action, allPages, describeError, errorView, field, fmtUnix, h, list, reasonProblem, table, trimmed,
} from "../view.js";
import { addPanel } from "../xcodeadd.js";
import {
  dueWords, gigabytes, isInstalled, planOf, policyOf, uploadWords, xcodesOf,
} from "../xcodeplan.js";

// How often the page reads everything again, in seconds.
export const REFRESH_SECONDS = 60;

// How often the page reads Xcode.List while the Add Xcode panel is open, in seconds.
export const PANEL_SECONDS = 10;

// How many runs the page reads.
const RUNS_LIMIT = 10;

const UNASSIGNED = "Unassigned";

const STATE = "MAINTENANCE_STATE_";
const MAC = "PLATFORM_DARWIN_ARM64";

// The states in which a run still holds its hosts.
const OPEN = [`${STATE}SCHEDULED`, `${STATE}RUNNING`, `${STATE}HALTED`];

function counted(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function nameOf(host) {
  return typeof host.nodeId === "string" ? host.nodeId : "";
}

function groupOf(host) {
  return typeof host.hostGroup === "string" ? host.hostGroup : "";
}

// Whether `host` is a Mac: its last heartbeat says so, or its report has a Mac's facts.
function isMac(host) {
  return ((host.heartbeat || {}).platform === MAC) || (host.maintenance || {}).mac !== undefined;
}

function idOf(archive) {
  return `Xcode ${archive.version} (${archive["build"]})`;
}

// What one read of the page's calls answers: `{ xcode, hosts, groups }`, `groups` the policy of each
// host group by name; it throws what a failed call throws.
async function readFleet(ctx) {
  const xcode = await ctx.api.xcode.list({});
  const hosts = await allPages((pageToken) => ctx.api.hosts.list(pageToken === "" ? {} : { pageToken }), "hosts");
  const states = await allPages((pageToken) => ctx.api.hostGroups.list(pageToken === "" ? {} : { pageToken }),
    "hostGroups");
  const groups = new Map(states.map((state) => [(state["group"] || {}).name, policyOf(state)]));
  return { xcode, hosts, groups };
}

// The runs: `{ runs, failure }`, each of `runs` `{ run, hosts }` with `hosts` the answer of
// Maintenance.Get for a run that still holds its hosts (the hosts of the run and each one's kinds,
// which the coordinator fixed when it scheduled the run), and none for any other. A call that fails
// fails the read.
async function readRuns(ctx) {
  try {
    const reply = await ctx.api.maintenance.list({ limit: RUNS_LIMIT });
    const runs = [];
    for (const run of list(reply, "runs")) {
      const hosts = OPEN.includes(run.state) ? list(await ctx.api.maintenance.get({ runId: run["id"] }), "hosts") : [];
      runs.push({ run, hosts });
    }
    return { runs, failure: null };
  } catch (error) {
    return { runs: [], failure: error };
  }
}

export async function render(ctx) {
  let fleet;
  try {
    fleet = await readFleet(ctx);
  } catch (error) {
    return errorView("Xcode", error);
  }
  let runs = await readRuns(ctx);
  let refreshFailure = null;
  // The Remove field of each archive, by SHA-256.
  const removals = new Map();
  // The open Add Xcode panel: `{ target, baseline, refused, stop }`, or null.
  let panel = null;

  const archives = () => list(fleet.xcode, "archives");
  const policyFor = (group) => fleet.groups.get(group) || { keep: 0, xcodeDefault: "latest" };

  // The Macs, in group order (Unassigned last) and then by name, each with its group's policy and plan.
  const macs = () => {
    const order = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
    return fleet.hosts.filter(isMac)
      .map((host) => {
        const group = groupOf(host);
        const policy = policyFor(group);
        return { host, group, policy, plan: planOf(policy, archives(), xcodesOf(host)) };
      })
      .sort((a, b) => (a["group"] === b["group"] ? order(nameOf(a["host"]), nameOf(b["host"]))
        : a["group"] === "" ? 1 : b["group"] === "" ? -1 : order(a["group"], b["group"])));
  };

  // The Macs on which `archive` is still due: a kept release the Mac lacks.
  const dueOn = (archive) => macs().filter(({ host, plan }) => host.maintenance !== undefined
    && plan.missing.some((missing) => missing.sha256 === archive.sha256));

  // The open run that holds `host`, as `{ run, hosts }`: it names the Mac or its group.
  const runOf = (host) => runs.runs.find(({ run }) => OPEN.includes(run.state)
    && (list(run, "hostIds").includes(nameOf(host)) || (run.hostGroup === groupOf(host) && run.hostGroup !== "")));

  const repaint = () => ctx.show(page());

  const waitingSection = () => {
    const waiting = fleet.xcode.waiting;
    if (fleet.xcode.configured !== true) {
      return h("p", { class: "quiet", name: "xcode-waiting" },
        "This coordinator has no Xcode store: it was started without --xcode-dir.");
    }
    const needs = waiting === undefined || trimmed(waiting.requiresMacos) === "" ? "" : `; it needs macOS ${waiting.requiresMacos}`;
    return h("div", { name: "xcode-waiting" },
      h("p", waiting === undefined ? { class: "quiet" } : {}, waiting === undefined
        ? "No release waits: the store holds the newest release."
        : h("strong", {}, `Xcode ${waiting.version} (${waiting["build"]}) is out and not in the store${needs}`)),
      panel !== null ? null : h("div", { class: "buttons" },
        h("button", { type: "button", name: "add-xcode", on: { click: () => openPanel() } }, "Add Xcode")));
  };

  const uploadsSection = () => {
    const uploads = list(fleet.xcode, "uploads");
    if (uploads.length === 0) {
      return null;
    }
    return h("section", { name: "xcode-uploads" },
      h("h3", {}, "Uploads"),
      h("ul", {}, uploads.map((upload) => h("li", { name: "upload" },
        `Xcode ${upload.version} (${upload["build"]}), started by ${upload.startedBy || "-"}: `
        + uploadWords(ctx.now(), upload)))));
  };

  const removeRow = (archive) => {
    if (!removals.has(archive.sha256)) {
      removals.set(archive.sha256, {});
    }
    const state = removals.get(archive.sha256);
    const due = dueOn(archive);
    const names = due.map(({ host }) => nameOf(host));
    const installed = macs().filter(({ host }) => xcodesOf(host).some((xcode) => isInstalled(xcode, archive))).length;
    return h("tr", { name: "archive" },
      h("td", {}, archive.version),
      h("td", {}, archive["build"]),
      h("td", {}, gigabytes(archive.sizeBytes)),
      h("td", {}, archive.uploadedBy || "-"),
      h("td", {}, fmtUnix(archive.uploadedAtUnix)),
      h("td", {}, counted(installed, "Mac")),
      h("td", {},
        h("p", { class: "note", name: "still-due" },
          names.length === 0 ? "Due on no Mac." : `Still due on ${names.join(", ")}.`),
        field("Reason", "remove-reason", state, "reason", { maxlength: 256 }),
        action(ctx, "Remove", "remove-archive", async () => {
          const reason = trimmed(state.reason);
          if (reason === "") {
            return "Give a reason for the removal.";
          }
          const problem = reasonProblem(reason);
          if (problem !== null) {
            return problem;
          }
          const still = names.length === 0 ? "No Mac has it due." : `It is still due on ${names.join(", ")}.`;
          if (!ctx.confirm(`Remove ${idOf(archive)} from the store? ${still}`)) {
            return undefined;
          }
          await ctx.api.xcode.remove({ sha256: archive.sha256, reason });
          removals.delete(archive.sha256);
          fleet = { ...fleet, xcode: await ctx.api.xcode.list({}) };
          repaint();
          return `Removed ${idOf(archive)}.`;
        })));
  };

  const storeSection = () => {
    const held = archives();
    const keep = Number(fleet.xcode.keep || 0);
    return h("section", { name: "xcode-store" },
      h("h3", {}, "The store"),
      fleet.xcode.configured !== true ? null : h("p", { class: "note" },
        `The store keeps the newest ${counted(keep, "release")}, and any a Mac reports installed. `
        + `An upload may be at most ${gigabytes(fleet.xcode.maxBytes)}.`),
      held.length === 0
        ? h("p", { class: "quiet", name: "store-empty" }, "The store holds no Xcode.")
        : table(["Version", "Build", "Size", "Uploaded by", "Stored", "Installed on", ""], held.map(removeRow)));
  };

  const macRow = ({ host, group, policy, plan }) => {
    const report = host.maintenance;
    const held = runOf(host);
    const run = held === undefined ? undefined : held["run"];
    // The run's turn for this Mac, whose kinds the coordinator fixed when it scheduled the run: a Mac
    // with no turn (nothing due of the run's kinds then) or a turn without Xcode is not given the
    // release by the run.
    const turn = held === undefined ? undefined : held.hosts.find((entry) => entry.hostId === nameOf(host));
    const takes = turn !== undefined && list(turn, "kinds").includes("MAINTENANCE_KIND_XCODE");
    const conflicts = list((report || {}).mac, "mdmConflicts");
    return h("tr", { name: "mac" },
      h("td", {}, h("button", {
        type: "button", class: "linklike", name: "open-host",
        on: { click: () => ctx.navigate("host", { nodeId: nameOf(host) }) },
      }, nameOf(host))),
      h("td", {}, group === "" ? UNASSIGNED : group),
      h("td", {}, group === "" ? "-" : `keep ${policy.keep}, default ${policy.xcodeDefault}`),
      h("td", { name: "installed" }, xcodesOf(host).map((xcode) => h("div", {},
        `${xcode.version} (${xcode["build"]})${xcode.selected === true ? ", selected" : ""}${xcode.foreign === true ? ", foreign" : ""}`)),
      xcodesOf(host).length === 0 ? "none" : null),
      h("td", { name: "due" }, report === undefined ? "no report" : dueWords(plan, policy.keep)),
      h("td", { name: "device-management" }, report === undefined ? "no report"
        : conflicts.length === 0 ? "nothing that blocks updates" : conflicts.join(", ")),
      h("td", { name: "run" }, run === undefined ? null : [
        h("button", {
          type: "button", class: "linklike", name: "open-run",
          on: { click: () => ctx.navigate("maintenance", {}) },
        }, `run ${run["id"]}`),
        takes ? null : h("span", { class: "note" }, ", which does not take Xcode: schedule after it ends")]));
  };

  const macsSection = () => {
    const rows = macs();
    return h("section", { name: "xcode-macs" },
      h("h3", {}, "The Macs"),
      rows.length === 0
        ? h("p", { class: "quiet", name: "macs-empty" }, "No Mac is enrolled.")
        : table(["Mac", "Group", "Keeps", "Xcodes installed", "Due", "Device management", "Run"], rows.map(macRow)));
  };

  // The words after "Stored." for the archive that was not in the store when the panel opened.
  const storedWords = (archive) => {
    const by = new Map();
    for (const { group } of dueOn(archive)) {
      by.set(group, (by.get(group) || 0) + 1);
    }
    const groups = [...by.entries()].map(([group, n]) => `${counted(n, "Mac")} of ${group === "" ? UNASSIGNED : group}`);
    const due = groups.length === 0 ? "Due on no Mac." : `Due on ${groups.join(", ")}.`;
    return panel["target"] === null ? `${idOf(archive)}. ${due}` : due;
  };

  const panelView = () => {
    const stored = archives().find((archive) => !panel.baseline.has(archive.sha256));
    const uploads = list(fleet.xcode, "uploads");
    const progress = panel["target"] === null ? uploads[0]
      : uploads.find((upload) => upload.version === panel["target"].version
        && upload["build"] === panel["target"]["build"]);
    return addPanel({
      ctx,
      target: panel["target"],
      origin: typeof ctx.origin === "string" ? ctx.origin : "",
      progress: stored !== undefined || progress === undefined ? null : progress,
      stored: stored === undefined ? null : storedWords(stored),
      refused: panel.refused,
      now: ctx.now(),
      copy: async (key, line) => {
        try {
          await ctx.copy(line);
          panel.refused = null;
          ctx.report("Copied.");
        } catch (error) {
          panel.refused = key;
        }
        repaint();
      },
      close: () => {
        panel.stop();
        panel = null;
        repaint();
      },
    });
  };

  const openPanel = () => {
    const waiting = fleet.xcode.waiting;
    panel = {
      target: waiting === undefined ? null : { version: waiting.version, build: waiting["build"] },
      baseline: new Set(archives().map((archive) => archive.sha256)),
      refused: null,
      stop: () => {},
    };
    const mine = panel;
    // Only Xcode.List: the Macs are as the last full read left them.
    panel.stop = ctx.every(async () => {
      try {
        fleet = { ...fleet, xcode: await ctx.api.xcode.list({}) };
      } catch (error) {
        return;
      }
      if (panel === mine) {
        repaint();
      }
    }, PANEL_SECONDS);
    repaint();
  };

  const page = () => h("section", { class: "page" },
    h("h2", {}, "Xcode"),
    h("p", { class: "note" },
      "The Xcode releases the coordinator holds for the Macs, and what each Mac has. Updates are started on the Maintenance page."),
    refreshFailure === null ? null : h("p", { class: "warning", role: "alert" },
      `The page could not be read again (${describeError(refreshFailure)}): this is the last answer.`),
    runs.failure === null ? null : h("p", { class: "warning", role: "alert", name: "runs-failed" },
      `The runs could not be read (${describeError(runs.failure)}): no Mac is linked to its run.`),
    waitingSection(),
    panel === null ? null : panelView(),
    uploadsSection(),
    storeSection(),
    macsSection());

  ctx.every(async () => {
    try {
      fleet = await readFleet(ctx);
      refreshFailure = null;
    } catch (error) {
      refreshFailure = error;
    }
    runs = await readRuns(ctx);
    repaint();
  }, REFRESH_SECONDS);
  return page();
}
