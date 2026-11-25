// The Add Xcode panel of the Xcode page (docs/design/xcode-one-action.md, section 4.1). The browser
// cannot download from Apple for the operator, so the panel shows the command to run on the Mac that
// is signed in to the developer account, with a Copy button, and the line that makes `hk`'s settings
// file the first time. It makes no call and keeps no state: the page gives it what it shows.
//
// - The command is `hk xcode add <version>` for the release that waits, or `hk xcode add` (the
//   newest archive in ~/Downloads) when none does.
// - The settings line is `hk settings init --endpoint <origin> --ca-file <ca.pem> --token-file
//   <token-file>`, the origin being the page's own (`ctx.origin`); without one it names this
//   console's address in words, never an address of its own.
// - Copy calls `copy(key, line)`. When the page says the clipboard refused the line `key`, the line
//   is selected when the page is shown, and the panel says so.
// - While the page's upload is in progress the panel says how far it is; once the page says the
//   archive is stored, it says so with the Macs it is due on and a link to the Maintenance page.

import { action, deployDocLink, h } from "./view.js";
import { uploadWords } from "./xcodeplan.js";

// Selects the text of `element`, as a person's drag would, so that a copy by hand takes it.
export function selectText(element) {
  const doc = element.ownerDocument;
  const selection = doc.defaultView.getSelection();
  const range = doc.createRange();
  range.selectNodeContents(element);
  selection.removeAllRanges();
  selection.addRange(range);
}

// The command that adds `target` (`{ version, build }`, or null when no release waits).
export function addCommand(target) {
  return target === null ? "hk xcode add" : `hk xcode add ${target.version}`;
}

// The line that makes `hk`'s settings file, for the page's own `origin` ("" when it has none).
export function settingsLine(origin) {
  const endpoint = origin === "" ? "<this console's address>" : origin;
  return `hk settings init --endpoint ${endpoint} --ca-file <ca.pem> --token-file <token-file>`;
}

// A line to copy: the line itself, selected when `refused`, its Copy button and, when the clipboard
// refused it, what to do.
function copyLine(env, key, line) {
  const refused = env.refused === key;
  return [
    h("div", { class: "copy-line" },
      h("pre", {
        name: `${key}-line`,
        on: refused ? { mounted: (e) => selectText(e.currentTarget) } : {},
      }, line),
      action(env.ctx, "Copy", `copy-${key}`, async () => {
        await env.copy(key, line);
      })),
    refused ? h("p", { class: "warning", name: `${key}-refused`, role: "alert" },
      "This browser did not allow copying: the line above is selected. Copy it by hand.") : null,
  ];
}

// The panel. `env`: `ctx`; `target`, the waiting release (`{ version, build }`) or null; `origin`;
// `progress`, the upload of the release in progress (an `XcodeUpload`) or null; `stored`, the words
// after "Stored." once the archive is stored, or null; `refused`, the key of the line the clipboard
// refused or null; `copy(key, line)`; `now` (unix seconds); `close()`.
export function addPanel(env) {
  const { ctx, target } = env;
  return h("section", { class: "card", name: "add-panel" },
    h("h3", {}, target === null ? "Add Xcode" : `Add Xcode ${target.version} (${target.build})`),
    h("p", {}, "On the Mac signed in to the Apple developer account, in Terminal:"),
    copyLine(env, "add", addCommand(target)),
    target === null
      ? h("p", { class: "note", name: "add-note" },
        "No release waits: the store holds the newest release. `hk xcode add` takes the newest archive in ~/Downloads. "
        + "This page shows the upload as it arrives.")
      : h("p", { class: "note", name: "add-note" },
        `It downloads ${target.version} with xcodes (or takes ~/Downloads/Xcode_${target.version}.xip), checks Apple's `
        + "signature and the build, and uploads it here. This page shows the upload as it arrives."),
    h("p", {}, "First time on this Mac: install hk (",
      deployDocLink("verify-a-release", "how to verify a release", "verify-a-release.md", "verify-a-release"),
      "), then"),
    copyLine(env, "settings", settingsLine(env.origin)),
    env.progress === null ? null : h("p", { name: "upload-progress" },
      `Uploading: ${uploadWords(env.now, env.progress)}`),
    env.stored === null ? null : h("p", { name: "upload-stored" },
      h("strong", {}, `Stored. ${env.stored}`), " ",
      h("button", {
        type: "button", class: "linklike", name: "open-maintenance",
        on: { click: () => ctx.navigate("maintenance", {}) },
      }, "Open the Maintenance page")),
    h("div", { class: "buttons" },
      h("button", { type: "button", name: "close-add", on: { click: () => env.close() } }, "Close")));
}
