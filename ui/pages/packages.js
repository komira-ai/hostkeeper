// Packages: the repositories packages are installed from and the registry of what the coordinator
// can deploy, as two plain lists, the repositories first because a package comes from one. Each
// list is a table (a repository: its name, its URL, the platforms it serves and how many packages use it; a package: its
// name, its repository and the host groups that run it) with an "Add repository" or "Add package"
// button in its header and Edit and Remove on each package row, Check again and Remove on each repository row. No form is shown until a button asks for
// one: Add opens an empty form under its table, a row's Edit opens that row's, each with Save and
// Cancel (one form at a time; a save draws the page again with every form closed). "Add package"
// is disabled, with a hint, while no repository is registered. A header is the list's title with
// one line saying what it is, and its button right-aligned; Remove is the danger button of the row,
// asking before it removes. Styles of the page are in `packages.css`.
//
// A package is registered when a host group lists it or the operator added it here; the Host
// groups page offers these and no other. The minimum version of a package belongs to a host
// group (the package's minimum in that group), so it is set from a package's Edit, for one of the
// groups that run it, and a new package has none until a group runs it.
//
// A repository is one entry that serves every platform it publishes for, and adding one asks for a
// name, a URL and, for a private one, the credential file on the coordinator's machine. "Check
// repository" has the coordinator read the repository's descriptor (docs/deploy/package-repositories.md,
// "Structure of a repository") and shows what it found: the platforms, the channels, the packages
// published for macOS and the signing key's fingerprints, worked out by the coordinator from the key
// itself. The operator compares the fingerprint with the one the repository's owner publishes and
// adds it; the coordinator reads the repository again and refuses if anything differs from what was
// shown. `komira`, the organisation's repository, is built in: it is listed with a "built in" tag and
// has no Remove. A row's "Check again" opens the same form for a repository already added (its name
// locked), which is how an entry added before descriptors learns the platforms its repository
// serves. The suites, components and key file are not asked for: they come from the descriptor, and
// the API's `Repositories.Put` still takes them by hand.
//
// Calls: Services.List (the packages), Services.Put on Add package, Services.SetRepository and
// Services.SetMinVersion on a package's Save, Services.Remove; Repositories.List, Repositories.Discover
// on Check repository, Repositories.Add on Add repository and Repositories.Remove. The reason of each write is a fixed sentence: the audit log names the operator.
// A package a host group lists, or a repository a package comes from, is not removed: the page says
// which and the API refuses it all the same.

import {
  h, action, describeError, deployDocLink, field, list, select, table, trimmed,
} from "../view.js";

// The reasons sent with a write.
const PUT_REASON = "set in the console";
const REMOVE_REASON = "removed in the console";

// The default repository of a package that names none, preselected on the form.
const DEFAULT_REPOSITORY = "komira";

// The package's name rule of the API, checked here so the form answers before it sends.
const PACKAGE_NAME = /^[a-z0-9-]{1,64}$/;
// A repository's name rule.
const REPOSITORY_NAME = /^[a-z0-9-]{1,32}$/;

// A repository's URL: https and a host (the coordinator checks the rest of the address).
const HTTPS_ADDRESS = /^https:\/{2}[^/]/;

// The names of the platforms of a repository, as the operator reads them.
const PLATFORMS = {
  "linux-x86_64": "Linux x86-64",
  "linux-arm64": "Linux arm64",
  "macos-arm64": "macOS arm64",
};

// A package's minimum versions of Services.List, by the host group they are set for.
function minimums(entry) {
  return list(entry, "minVersions")
    .filter((min) => typeof min["role"] === "string")
    .map((min) => `${min["role"]} ${min.version}`);
}

// A platform in words.
function platformName(platform) {
  return PLATFORMS[platform] || platform;
}

// A fingerprint in groups of four characters, as it is read aloud and compared.
function grouped(print) {
  return (print.match(/.{1,4}/g) || []).join(" ");
}

// The packages page's sentences for a refusal: the API's token read in words where the page can
// say more than the token does.
function refusalWords(error, inUse) {
  return error && error.reason === "in_use" ? inUse : describeError(error);
}

// The sentences for a refusal of Repositories.Discover and Repositories.Add.
const REPOSITORY_REFUSALS = {
  discovery_failed: "The coordinator could not read a repository at that address: check the URL, and the credential file if it is private.",
  descriptor_invalid: "That address does not serve a valid repository descriptor (hostkeeper-repository.json); see How do I structure a repository?",
  descriptor_changed: "What the repository serves changed since it was checked: check it again and compare the fingerprint.",
  builtin: "komira is the organisation's built-in repository and its key is pinned: this key is not the pinned one.",
  conflict: "A channel of this repository is already a channel of another one.",
  capacity: "The coordinator holds as many repositories as it can.",
  credential_unusable: "The coordinator cannot use that credential file (it must be a file of the coordinator's user, mode 0600, one line).",
};

// A refusal of a repository call in words.
function repositoryWords(error) {
  return error && REPOSITORY_REFUSALS[error.reason] ? REPOSITORY_REFUSALS[error.reason] : describeError(error);
}

// A package form that is closed, with the repository `names` offers preselected.
function closedPackage(names) {
  return { visible: false, editing: false, service: "", repository: names.includes(DEFAULT_REPOSITORY) ? DEFAULT_REPOSITORY : "", minGroup: "", minVersion: "" };
}

// A repository form that is closed and empty.
function closedRepository() {
  return { visible: false, editing: false, called: "", uri: "", access: "public", credentialFile: "", found: null, checked: null };
}

// "1 package" or "3 packages".
function packagesCount(n) {
  return `${n} ${n === 1 ? "package" : "packages"}`;
}

// A control with a one-line hint under it.
function hinted(control, hint) {
  return h("div", { class: "hinted" }, control, h("p", { class: "note" }, hint));
}

// The Packages page.
export async function render(ctx) {
  let packages = [];
  let packagesFailure = null;
  try {
    packages = list(await ctx.api.services.list({}), "services");
  } catch (error) {
    packagesFailure = error;
  }
  let repositories = [];
  let unregistered = [];
  let repositoriesFailure = null;
  try {
    const reply = await ctx.api.repositories.list({});
    repositories = list(reply, "repositories");
    unregistered = list(reply, "unregistered");
  } catch (error) {
    repositoriesFailure = error;
  }
  const names = repositories.map((one) => (one.repository || {})["name"]).filter((name) => typeof name === "string");
  const state = { pkg: closedPackage(names), repo: closedRepository() };
  const show = () => ctx.show(page());
  const packageOf = (name) => packages.find((entry) => entry.service === name);

  // Loads the package `name` into the form.
  const editPackage = (name) => {
    const entry = packageOf(name) || {};
    state.repo = closedRepository();
    state.pkg = { visible: true, editing: true, service: name, repository: entry.repository || DEFAULT_REPOSITORY, minGroup: "", minVersion: "" };
    show();
  };

  // Opens the empty form that adds a package.
  const addPackage = () => {
    if (names.length === 0) {
      return;
    }
    state.repo = closedRepository();
    state.pkg = { ...closedPackage(names), visible: true };
    show();
  };

  const packageForm = () => {
    const { pkg } = state;
    if (!pkg.visible) {
      return null;
    }
    const entry = packageOf(pkg.service) || {};
    const groups = list(entry, "hostGroups");
    return h("fieldset", {},
      h("legend", {}, pkg.editing ? `Change package ${pkg.service}` : "Add package"),
      pkg.editing ? null : hinted(field("Name", "package-name", pkg, "service"),
        "The package's name as its repository publishes it: lower case letters, digits and -."),
      hinted(
        select("Repository", "package-repository", pkg, "repository",
          [["", "Choose a repository"], ...names.map((name) => [name, name])]),
        names.length === 0
          ? "No repository is registered: add one under Repositories first."
          : "Where hosts install the package from; the repositories above."),
      pkg.editing && minimums(entry).length > 0
        ? h("p", { class: "note" }, `Minimum versions set: ${minimums(entry).join(", ")}.`)
        : null,
      pkg.editing
        ? hinted(
          select("Minimum in host group", "package-min-group", pkg, "minGroup",
            [["", "No change"], ...groups.map((name) => [name, name])]),
          groups.length === 0
            ? "A minimum version belongs to a host group; no host group runs this package yet."
            : "The host group the minimum version below is set for.")
        : null,
      pkg.editing && groups.length > 0
        ? hinted(field("Minimum version", "package-min-version", pkg, "minVersion"),
          "No host of that group is ordered a version below it; empty removes the group's minimum.")
        : null,
      h("div", { class: "buttons" },
        action(ctx, "Save package", "put-package", async () => {
          const name = trimmed(pkg.service);
          if (!PACKAGE_NAME.test(name)) {
            return "Name the package: 1 to 64 lower case letters, digits or -.";
          }
          if (pkg.repository === "") {
            return "Choose the repository the package comes from.";
          }
          if (!pkg.editing) {
            await ctx.api.services.put({ service: name, repository: pkg.repository, reason: PUT_REASON });
            ctx.refresh();
            return `Added package ${name} from ${pkg.repository}.`;
          }
          const said = [];
          if (pkg.repository !== (entry.repository || "")) {
            await ctx.api.services.setRepository({ service: name, repository: pkg.repository, reason: PUT_REASON });
            said.push(`repository ${pkg.repository}`);
          }
          if (pkg.minGroup !== "") {
            await ctx.api.services.setMinVersion(
              { service: name, minVersion: { role: pkg.minGroup, version: trimmed(pkg.minVersion) } });
            said.push(`minimum version of ${pkg.minGroup} ${trimmed(pkg.minVersion) || "removed"}`);
          }
          ctx.refresh();
          return said.length === 0 ? `Nothing to change for ${name}.` : `Changed ${name}: ${said.join(", ")}.`;
        }),
        action(ctx, "Cancel", "cancel-package", () => {
          state.pkg = closedPackage(names);
          show();
        })));
  };

  const removePackage = (name) => action(ctx, "Remove", "remove-package", async () => {
    const groups = list(packageOf(name), "hostGroups");
    if (groups.length > 0) {
      return `${name} is run by host group ${groups.join(", ")}: take it out of ${groups.length === 1 ? "that group" : "those groups"} on the Host groups page first.`;
    }
    if (!ctx.confirm(`Remove package ${name}? No host group lists it, so no host is changed.`)) {
      return "Not removed.";
    }
    try {
      await ctx.api.services.remove({ service: name, reason: REMOVE_REASON });
    } catch (error) {
      return `${name}: ${refusalWords(error, "a host group still lists it, so it is not removed.")}`;
    }
    ctx.refresh();
    return `Removed package ${name}.`;
  }, { class: "danger" });

  const packageRows = () => packages.map((entry) => h("tr", {},
    h("td", {}, h("button", {
      type: "button", class: "linklike", name: "open-package",
      on: { click: () => ctx.navigate("package", { package: entry.service }) },
    }, entry.service)),
    h("td", {}, entry.repository || DEFAULT_REPOSITORY),
    h("td", {}, list(entry, "hostGroups").join(", ") || "-"),
    h("td", { class: "row-actions" }, action(ctx, "Edit", "edit-package", () => editPackage(entry.service)),
      removePackage(entry.service))));

  // The address a repository is read at: the base URL it was added with, else (an entry added
  // before descriptors) its apt root without the `/apt` its layout usually ends in.
  const baseOf = (body) => body.baseUri || (body.uri || "").replace(/\/apt$/, "");

  // `control` (a `select` of view.js: a label holding the chooser) that draws the page again after
  // it changes, for a form whose fields depend on its choice.
  const redrawn = (control) => {
    const chooser = control.children[1];
    const changed = chooser.on.change;
    chooser.on.change = (e) => {
      changed(e);
      show();
    };
    return control;
  };

  // Opens the form that checks the repository `one` of Repositories.List again, its name locked.
  const checkAgain = (one) => {
    const body = one.repository || {};
    state.pkg = closedPackage(names);
    state.repo = {
      ...closedRepository(),
      visible: true,
      editing: true,
      called: body["name"],
      uri: baseOf(body),
      access: body.access === "hosts" ? "hosts" : "public",
      credentialFile: body.credentialFile || "",
    };
    show();
  };

  // Opens the empty form that adds a repository.
  const addRepository = () => {
    state.pkg = closedPackage(names);
    state.repo = { ...closedRepository(), visible: true };
    show();
  };

  // What the form describes, or a sentence saying why it cannot be sent yet: the address and access
  // as the calls take them. The name is judged where it is needed.
  const requestOfForm = () => {
    const { repo } = state;
    const baseUri = trimmed(repo.uri);
    if (!HTTPS_ADDRESS.test(baseUri) || baseUri.endsWith("/")) {
      return { problem: "The URL is an https address with a host and no trailing /." };
    }
    if (repo.access === "hosts" && trimmed(repo.credentialFile) === "") {
      return { problem: "A private repository needs the credential file's path on the coordinator." };
    }
    const request = { baseUri, access: repo.access };
    if (repo.access === "hosts") {
      request.credentialFile = trimmed(repo.credentialFile);
    }
    return { request };
  };

  // The card that shows what a check found: the platforms, the channels, the macOS packages and the
  // key, whose fingerprints are in groups of four to be compared with the owner's.
  const foundCard = (found) => {
    const platforms = list(found, "platforms").map(platformName).join(", ") || "-";
    const packages = list(found, "macosPackages");
    return h("div", { class: "found", role: "group", "aria-label": "What the repository serves" },
      found.description ? h("p", {}, found.description) : null,
      h("p", { name: "found-platforms" }, `Platforms: ${platforms}.`),
      h("p", { name: "found-channels" }, `Channels: ${list(found, "channels").join(", ") || "-"}.`),
      packages.length > 0
        ? h("p", { name: "found-packages" }, `macOS installers kept in a directory of another name: ${packages.join(", ")}.`)
        : null,
      list(found, "userIds").map((id) => h("p", { name: "found-key-user" }, `Key: ${id}`)),
      list(found, "fingerprints").map((print) => h("p", { class: "fingerprint", name: "found-fingerprint" }, h("code", {}, grouped(print)))),
      found.pinned
        ? h("p", { class: "note", name: "found-pinned" }, "This is komira's pinned key.")
        : h("p", { class: "note", name: "found-compare" }, "Compare this fingerprint with the one the repository's owner publishes."));
  };

  const repositoryForm = () => {
    const { repo } = state;
    if (!repo.visible) {
      return null;
    }
    return h("fieldset", {},
      h("legend", {}, repo.editing ? `Check repository ${repo.called} again` : "Add repository"),
      repo.editing ? null : hinted(field("Name", "repository-name", repo, "called"),
        "What packages call it: lower case letters, digits and -. Left empty, the repository's own name is used."),
      hinted(field("URL", "repository-uri", repo, "uri"),
        "The repository's address: an https address with a host. Everything else is read from the repository."),
      hinted(redrawn(select("Access", "repository-access", repo, "access",
        [["public", "Public: hosts read it from the URL"], ["hosts", "Private: hosts read it through the coordinator"]])),
        "A private repository is read by the coordinator alone, with a credential."),
      repo.access === "hosts"
        ? hinted(field("Credential file", "repository-credential", repo, "credentialFile"),
          "The absolute path, on the coordinator's machine, of the file that holds the Authorization header's value.")
        : null,
      h("p", { class: "note" }, deployDocLink("repository-structure-help", "How do I structure a repository?",
        "package-repositories.md", "structure-of-a-repository")),
      repo.found !== null ? foundCard(repo.found) : null,
      h("div", { class: "buttons" },
        action(ctx, repo.found === null ? "Check repository" : "Check again", "check-repository", async () => {
          const built = requestOfForm();
          if (built.problem) {
            return built.problem;
          }
          let reply;
          try {
            reply = await ctx.api.repositories.discover(built.request);
          } catch (error) {
            repo.found = null;
            repo.checked = null;
            return repositoryWords(error);
          }
          repo.found = reply.discovery || {};
          repo.checked = built.request;
          const { name: suggested = "" } = repo.found;
          if (trimmed(repo.called) === "" && REPOSITORY_NAME.test(suggested)) {
            repo.called = suggested;
          }
          show();
          return null;
        }),
        repo.found !== null
          ? action(ctx, "Add repository", "add-repository-confirm", async () => {
            const name = trimmed(repo.called);
            if (!REPOSITORY_NAME.test(name)) {
              return "Name the repository: 1 to 32 lower case letters, digits or -.";
            }
            const built = requestOfForm();
            if (built.problem) {
              return built.problem;
            }
            if (JSON.stringify(built.request) !== JSON.stringify(repo.checked)) {
              return "The address changed since it was checked: check it again.";
            }
            try {
              await ctx.api.repositories.add({
                name,
                ...repo.checked,
                descriptorSha256: repo.found.descriptorSha256,
                fingerprints: list(repo.found, "fingerprints"),
                reason: PUT_REASON,
              });
            } catch (error) {
              return repositoryWords(error);
            }
            ctx.refresh();
            return `Added repository ${name}.`;
          }, { class: "primary" })
          : null,
        action(ctx, "Cancel", "cancel-repository", () => {
          state.repo = closedRepository();
          show();
        })));
  };

  const removeRepository = (one) => action(ctx, "Remove", "remove-repository", async () => {
    const { name } = one.repository || {};
    const users = list(one, "services");
    if (users.length > 0) {
      return `${name} is the repository of ${users.join(", ")}: change those packages' repository first.`;
    }
    if (!ctx.confirm(`Remove repository ${name}? No package comes from it.`)) {
      return "Not removed.";
    }
    try {
      await ctx.api.repositories.remove({ name, reason: REMOVE_REASON });
    } catch (error) {
      return `${name}: ${refusalWords(error, "a package still comes from it, so it is not removed.")}`;
    }
    ctx.refresh();
    return `Removed repository ${name}.`;
  }, { class: "danger" });

  const repositoryRows = () => repositories.map((one) => {
    const body = one.repository || {};
    return h("tr", {},
      h("td", {}, body["name"], one.builtin === true ? [" ", h("span", { class: "tag" }, "built in")] : null),
      h("td", {}, body.baseUri || body.uri || "-"),
      h("td", {}, list(one, "platforms").map(platformName).join(", ") || "-"),
      h("td", {}, `used by ${packagesCount(list(one, "services").length)}`),
      h("td", { class: "row-actions" },
        action(ctx, "Check again", "check-again-repository", () => checkAgain(one)),
        one.builtin === true ? null : removeRepository(one)));
  });

  // A list's header: its title and the button that opens its add form.
  // The title and its one-line description on the left, the button right-aligned and centred on them.
  const header = (title, description, button) => h("div", { class: "list-head" },
    h("div", { class: "list-title" }, h("h3", {}, title), h("p", { class: "note" }, description)),
    button);

  const page = () => h("section", { class: "page packages" },
    h("h2", {}, "Packages"),
    header("Repositories", "Where packages are installed from. Each package comes from one of these.",
      action(ctx, "Add repository", "add-repository", addRepository, { class: "primary" })),
    repositoriesFailure !== null ? h("p", { class: "error", role: "alert" }, `Repositories: ${describeError(repositoriesFailure)}`) : null,
    repositoriesFailure === null && repositories.length === 0 ? h("p", { class: "note" }, "No repository is registered yet.") : null,
    unregistered.length > 0
      ? h("p", { class: "error", role: "alert" }, `Packages come from repositories that are not registered: ${unregistered.join(", ")}. Hosts of those packages are sent no repository until they are added.`)
      : null,
    repositories.length > 0
      ? table(["Repository", "URL", "Platforms", "Used by", ""], repositoryRows())
      : null,
    repositoryForm(),
    header("Packages", "What the coordinator can deploy; a host group is given packages from this list.",
      action(ctx, "Add package", "add-package", addPackage, names.length === 0 ? { class: "primary", disabled: true } : { class: "primary" })),
    names.length === 0 ? h("p", { class: "note" }, "Add a repository first") : null,
    packagesFailure !== null ? h("p", { class: "error", role: "alert" }, `Packages: ${describeError(packagesFailure)}`) : null,
    packagesFailure === null && packages.length === 0 ? h("p", { class: "note" }, "No package is registered yet.") : null,
    packages.length > 0
      ? table(["Package", "Repository", "Host groups", ""], packageRows())
      : null,
    packageForm());
  return page();
}
