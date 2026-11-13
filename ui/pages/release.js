// Release: the release a version of a service was set with, as Releases.GetRelease answers it
// (each platform's entries in the order set, and the digest of the release index set with them),
// and the entry each host of the service is picked, by the coordinator's rule
// (docs/design/packages.md section 3.3), from what Hosts.List says of the host.
// Calls: Releases.GetRelease, then, when it answers, Hosts.List, every page of it; nothing else.
//
// An installer's name is shown as it is, whatever it is: the page knows no installer and gives
// none a column or a word of its own, so a kind no daemon lists yet reads as every other.
//
// The page list has no entry for this page any more: its route opens the package page
// (`pages/package.js`), which builds on the parts this module exports.

import { h, action, allPages, describeError, errorView, field, fmtUnix, list, table, trimmed } from "../view.js";

// The platforms a release names, in the order the page shows them; a key the page does not know
// is shown after them, in sorted order.
export const PLATFORMS = ["linux-x86_64", "linux-arm64", "macos-arm64"];

// The key of a release's targets for a heartbeat's platform (`PLATFORM_LINUX_X86_64`, as
// Hosts.List answers it); null for an absent, unspecified or unknown one.
const PLATFORM_KEYS = {
  PLATFORM_LINUX_X86_64: "linux-x86_64",
  PLATFORM_LINUX_ARM64: "linux-arm64",
  PLATFORM_DARWIN_ARM64: "macos-arm64",
};

export function platformKey(platform) {
  return Object.prototype.hasOwnProperty.call(PLATFORM_KEYS, platform) ? PLATFORM_KEYS[platform] : null;
}

// The installer every daemon runs, and the one a daemon that did not announce `target-v1` is
// counted as listing alone.
const ARCHIVE = "hostkeeper-archive";
// The feature a daemon announces to be picked from the installers it lists.
const TARGET_FEATURE = "target-v1";

// The most hosts the table of picks shows; the counts above it count every host.
export const MAX_HOST_ROWS = 200;

// The platform keys of `targets` (GetRelease's map), known ones first in PLATFORMS' order, then
// the others sorted.
export function platformOrder(targets) {
  const keys = Object.keys(targets || {});
  const known = PLATFORMS.filter((key) => keys.includes(key));
  const other = keys.filter((key) => !PLATFORMS.includes(key)).sort();
  return known.concat(other);
}

// What the coordinator's pick gives `host` (one host of Hosts.List) of the release `targets` for
// `service`: `{installer, position}`, the picked entry's installer and its place in the list
// from 1, or `{reason}`, why none is picked, as `GateReason` and the coordinator spell it:
// - `no_target`: the release has no list for the host's platform;
// - `unheard`: its daemon announced `target-v1` and the coordinator has read no installer of it;
// - `installer_dropped`: the host runs the service with an installer the list has no entry of,
//   or that its daemon no longer lists;
// - `no_build_for_host`: the host does not run the service, and no entry is of an installer its
//   daemon lists.
// A host runs the service when its last heartbeat names it with any health but absent; it runs it
// with the installer that heartbeat names, `hostkeeper-archive` when it names none. A daemon that
// did not announce `target-v1` lists `hostkeeper-archive` alone and runs everything from archives.
// `{unknown}` says the page cannot tell: Hosts.List names no platform the page knows for the host
// (it names none for a host whose only word of its services is that it is a member of them,
// though the coordinator, which knows the host's platform, may pick it an entry).
export function pickFor(host, service, targets) {
  const beat = (host && host.heartbeat) || {};
  const key = platformKey(beat.platform);
  if (key === null) {
    return { unknown: "platform not reported" };
  }
  const entries = targets && Object.prototype.hasOwnProperty.call(targets, key)
    ? list(targets[key], "entries")
    : null;
  if (entries === null) {
    return { reason: "no_target" };
  }
  let listed = [ARCHIVE];
  let running = ARCHIVE;
  const word = list(beat, "services").find((one) => one["name"] === service);
  if (list(beat, "features").includes(TARGET_FEATURE)) {
    listed = list(host, "installers");
    if (listed.length === 0) {
      return { reason: "unheard" };
    }
    if (word && typeof word.installer === "string" && word.installer !== "") {
      running = word.installer;
    }
  }
  const runs = word !== undefined && word["health"] !== "HEALTH_ABSENT";
  const at = runs
    ? entries.findIndex((entry) => entry.installer === running && listed.includes(running))
    : entries.findIndex((entry) => listed.includes(entry.installer));
  if (at < 0) {
    return { reason: runs ? "installer_dropped" : "no_build_for_host" };
  }
  return { installer: entries[at].installer, position: at + 1 };
}

// The words for the reasons no entry is picked, as the coordinator spells them.
const PICK_REASONS = {
  no_target: "No build for this platform",
  unheard: "Installer not reported",
  installer_dropped: "Installer missing",
  no_build_for_host: "No build for this host",
};

// What the page says of a pick.
export function pickLabel(pick) {
  if (typeof pick.unknown === "string") {
    return `Unknown (${pick.unknown})`;
  }
  if (typeof pick.reason === "string") {
    return Object.prototype.hasOwnProperty.call(PICK_REASONS, pick.reason) ? PICK_REASONS[pick.reason] : pick.reason;
  }
  return `${pick.installer} (entry ${pick.position})`;
}

// The installers the host's daemon lists: those it reported when it announced `target-v1`, the
// archive alone when it did not; empty when it announced the feature and reported none.
export function installersListed(host) {
  const beat = (host && host.heartbeat) || {};
  return list(beat, "features").includes(TARGET_FEATURE) ? list(host, "installers") : [ARCHIVE];
}

// What the host runs `service` with: the installer its heartbeat names (the archive when it
// names none, or its daemon did not announce `target-v1`); "Not installed" when the heartbeat
// does not have the service running.
export function runsWith(host, service) {
  const beat = (host && host.heartbeat) || {};
  const word = list(beat, "services").find((one) => one["name"] === service);
  if (word === undefined || word["health"] === "HEALTH_ABSENT") {
    return "Not installed";
  }
  const named = list(beat, "features").includes(TARGET_FEATURE) && typeof word.installer === "string"
    && word.installer !== "";
  return named ? word.installer : ARCHIVE;
}

// The hosts of Hosts.List that the coordinator says something of `service` for.
function hostsOf(hosts, service) {
  return hosts.filter((host) => list(host, "services").some((view) => view.service === service));
}

// An entry's locations, in preference order, each as `store:locator`.
export function locations(entry) {
  const all = list(entry, "locations");
  return all.length === 0 ? "-" : all.map((at) => `${at["store"] || "-"}:${at["locator"] || "-"}`).join(", ");
}

export function digestCell(digest) {
  return typeof digest === "string" && digest !== "" ? h("code", {}, digest) : "-";
}

// One platform's list: a row per entry, in the order set.
export function platformSection(key, targetList) {
  const entries = list(targetList, "entries");
  const rows = entries.map((entry, i) => {
    const manifest = entry.manifest || {};
    const attestation = entry.attestation || {};
    return h("tr", {},
      h("td", {}, String(i + 1)),
      h("td", {}, entry.installer || "-"),
      h("td", {}, digestCell(entry.digest)),
      h("td", {}, entry.bytes === undefined ? "-" : String(entry.bytes)),
      h("td", {}, locations(entry)),
      h("td", {}, digestCell(attestation.digest)),
      h("td", {}, typeof manifest.package === "string" ? manifest.package : "-"),
      h("td", {}, manifest.minHostkeeper || "-"));
  });
  return h("section", { class: "platform" },
    h("h3", {}, `${key} (${entries.length} ${entries.length === 1 ? "entry" : "entries"})`),
    entries.length === 0
      ? h("p", {}, "No entry.")
      : table(["#", "Installer", "Digest", "Bytes", "Locations", "Attestation", "Package", "Oldest daemon"], rows));
}

// Each host of the service with its pick: the counts of every pick first, then a row per host,
// at most MAX_HOST_ROWS of them.
export function picksSection(ctx, service, targets, hosts) {
  const mine = hostsOf(hosts, service);
  const picked = mine.map((host) => [host, pickFor(host, service, targets)]);
  const counts = new Map();
  for (const [, pick] of picked) {
    const said = pickLabel(pick);
    counts.set(said, (counts.get(said) || 0) + 1);
  }
  const shown = picked.slice(0, MAX_HOST_ROWS);
  const rows = shown.map(([host, pick]) => {
    const beat = host.heartbeat || {};
    return h("tr", {},
      h("td", {},
        h("button", {
          type: "button", class: "linklike", name: "open-host",
          on: { click: () => ctx.navigate("host", { nodeId: host.nodeId }) },
        }, host.nodeId || "-")),
      h("td", {}, platformKey(beat.platform) || "-"),
      h("td", {}, installersListed(host).join(", ") || "none reported"),
      h("td", {}, runsWith(host, service)),
      h("td", {}, fmtUnix(host.lastHeartbeatUnix)),
      h("td", {}, pickLabel(pick)));
  });
  return h("section", { class: "picks" },
    h("h3", {}, `Hosts of ${service} (${mine.length})`),
    h("p", { class: "note" },
      "Each host's pick, by the coordinator's rule, from the host's last heartbeat: the entry of the installer "
      + "it runs the service with, else the first entry whose installer its daemon lists."),
    mine.length === 0
      ? h("p", {}, `No host reports ${service}.`)
      : [
        h("ul", { class: "plain" },
          Array.from(counts).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
            .map(([said, count]) => h("li", {}, `${said}: ${count}`))),
        table(["Host", "Platform", "Installers listed", "Runs with", "Last heartbeat", "Picked"], rows),
        mine.length > shown.length
          ? h("p", { class: "note" },
            `Showing the first ${shown.length} of ${mine.length} hosts; the counts above count every one.`)
          : null,
      ]);
}

function chooser(ctx, service, version) {
  const form = { service, version };
  return h("fieldset", {},
    h("legend", {}, "Release"),
    field("Service", "service", form, "service"),
    field("Version", "version", form, "version"),
    action(ctx, "Show", "show", async () => {
      if (trimmed(form.service) === "" || trimmed(form.version) === "") {
        return "Name a service and a version first.";
      }
      ctx.navigate("release", { service: trimmed(form.service), version: trimmed(form.version) });
      return undefined;
    }));
}

export async function render(ctx) {
  const param = (name) => (ctx.params && typeof ctx.params[name] === "string" ? ctx.params[name] : "");
  const service = param("service");
  const version = param("version");
  const top = [h("h2", {}, "Release"), chooser(ctx, service, version)];
  if (service === "" || version === "") {
    return h("section", { class: "page" }, top,
      h("p", {}, "Name a service and a version to see the targets it was set with."));
  }
  const title = `${service} ${version}`;
  let release;
  try {
    release = await ctx.api.releases.getRelease({ service, version });
  } catch (error) {
    return h("section", { class: "page" }, top, errorView(title, error).children);
  }
  const targets = release.targets || {};
  const keys = platformOrder(targets);
  let hostsPart;
  try {
    const hosts = await allPages((pageToken) => ctx.api.hosts.list(
      pageToken === "" ? {} : { pageToken }), "hosts");
    hostsPart = picksSection(ctx, service, targets, hosts);
  } catch (error) {
    hostsPart = h("section", { class: "picks" }, h("h3", {}, `Hosts of ${service}`),
      h("p", { class: "error", role: "alert" }, `The hosts could not be read: ${describeError(error)}`));
  }
  return h("section", { class: "page" }, top,
    h("h3", {}, title),
    table(["", ""], [
      ["Release index digest", digestCell(release.releaseIndexDigest)],
      ["Platforms", keys.length === 0 ? "none" : keys.join(", ")],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    keys.length === 0
      ? h("p", {}, "This release names no targets.")
      : keys.map((key) => platformSection(key, targets[key])),
    hostsPart);
}
