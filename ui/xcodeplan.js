// What the Xcode page says about the Macs, from facts it was given and nothing else: which Xcode
// releases a host group keeps, which a Mac lacks, and what that makes due on it. The rule is the
// coordinator's (`maintenance_xcode.rs`, `plan`; docs/design/xcode-one-action.md section 2.5, step 1):
//
// - Kept: the newest `xcode_keep` releases of the store, newest first, ordered by their version
//   numbers (`27.10` is newer than `27.9`, `27` is `27.0`; two spellings of one number are two
//   versions, ordered by their text), then by build, then by SHA-256. None when `xcode_keep` is 0.
// - The default: `latest` (or no value) is the newest kept release; a version is the kept release of
//   exactly that version, and none when no kept release has it.
// - Installed: a kept release is installed only in its own directory (the report's Xcode is not
//   `foreign`) with the release's version and build.
// - Due: the kept releases that are not installed, newest first; when none is missing, the default
//   when `xcode-select` does not name it as installed.
//
// It calls nothing and changes nothing.

import { list } from "./view.js";

// The numbers of a version, `27.3.1` as [27, 3, 1]; a part that is not digits reads as 0.
function numbers(version) {
  return String(version).split(".").map((part) => (/^[0-9]+$/.test(part) ? Number(part) : 0));
}

// -1, 0 or 1: `a` before, the same as, or after `b` as the coordinator orders versions.
export function compareVersions(a, b) {
  const x = numbers(a);
  const y = numbers(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const one = x[i] || 0;
    const other = y[i] || 0;
    if (one !== other) {
      return one < other ? -1 : 1;
    }
  }
  return a < b ? -1 : a > b ? 1 : 0;
}

function textOf(value) {
  return typeof value === "string" ? value : "";
}

// The archives, newest first.
export function newestFirst(archives) {
  const order = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
  return archives.slice().sort((a, b) => compareVersions(b.version, a.version)
    || order(textOf(b["build"]), textOf(a["build"])) || order(textOf(a.sha256), textOf(b.sha256)));
}

// What a host group's policy keeps: `{ keep, xcodeDefault }` from a `HostGroupState` of
// HostGroups.List; no group (the hosts in none) keeps nothing.
export function policyOf(state) {
  const maintenance = ((((state || {}).group || {}).policy || {}).maintenance) || {};
  return {
    keep: Number(maintenance.xcodeKeep || 0),
    xcodeDefault: typeof maintenance.xcodeDefault === "string" && maintenance.xcodeDefault !== ""
      ? maintenance.xcodeDefault : "latest",
  };
}

// Whether `installed` (an Xcode of a report) is `archive` in its own directory.
export function isInstalled(installed, archive) {
  return installed.foreign !== true && installed.version === archive.version
    && textOf(installed["build"]) === textOf(archive["build"]);
}

// The plan of a Mac that reports `xcodes`, in a group with `policy`, by the store's `archives`:
// `{ kept, defaultArchive, missing, unselected }`; `missing` the kept releases it lacks, newest
// first; `unselected` the default when nothing is missing and `xcode-select` does not name it.
export function planOf(policy, archives, xcodes) {
  const kept = policy.keep === 0 ? [] : newestFirst(archives).slice(0, policy.keep);
  const defaultArchive = (policy.xcodeDefault === "latest"
    ? kept[0] : kept.find((archive) => archive.version === policy.xcodeDefault)) || null;
  const installed = (archive) => xcodes.some((xcode) => isInstalled(xcode, archive));
  const missing = kept.filter((archive) => !installed(archive));
  const selected = defaultArchive !== null
    && xcodes.some((xcode) => xcode.selected === true && isInstalled(xcode, defaultArchive));
  return {
    kept,
    defaultArchive,
    missing,
    unselected: defaultArchive !== null && !selected && missing.length === 0 ? defaultArchive : null,
  };
}

// The `xcodes` a host's report lists.
export function xcodesOf(host) {
  return list(host.maintenance || {}, "xcodes");
}

// What the Mac's row says is due, from its plan and the group's `keep`.
export function dueWords(plan, keep) {
  if (keep === 0) {
    return "left out: the group keeps no Xcode";
  }
  if (plan.kept.length === 0) {
    return "nothing due: the store holds no Xcode";
  }
  if (plan.missing.length > 0) {
    return `due: ${plan.missing.map((archive) => archive.version).join(", then ")}`;
  }
  if (plan.unselected !== null) {
    return `${plan.unselected.version} installed, not selected: due to be selected`;
  }
  return "installed";
}

// A size in bytes as decimal gigabytes with two places: `11.92 GB`.
export function gigabytes(bytes) {
  const value = Number(bytes);
  return `${((Number.isFinite(value) ? value : 0) / 1e9).toFixed(2)} GB`;
}

// `count` and its noun, plural for any count but one.
function counted(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

// How long ago `unix` was at `now` (both unix seconds): `40 seconds ago`, `2 minutes ago`, `3 hours
// ago`, `2 days ago`; an instant not before `now` is `just now`, and none (0) is `never`.
export function agoWords(now, unix) {
  const at = Number(unix);
  if (!Number.isFinite(at) || at <= 0) {
    return "never";
  }
  const secs = Math.floor(now - at);
  if (secs < 1) {
    return "just now";
  }
  if (secs < 60) {
    return `${counted(secs, "second")} ago`;
  }
  if (secs < 3600) {
    return `${counted(Math.floor(secs / 60), "minute")} ago`;
  }
  if (secs < 86400) {
    return `${counted(Math.floor(secs / 3600), "hour")} ago`;
  }
  return `${counted(Math.floor(secs / 86400), "day")} ago`;
}

// How far an upload (an `XcodeUpload`) is, in words: `7.31 of 11.92 GB, last write 2 minutes ago`,
// at `now`.
export function uploadWords(now, upload) {
  return `${gigabytes(upload.receivedBytes).replace(" GB", "")} of ${gigabytes(upload.sizeBytes)}, `
    + `last write ${agoWords(now, upload.lastWriteUnix)}`;
}
