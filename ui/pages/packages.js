// Packages: the registry of what the coordinator can deploy, and the package repositories it comes
// from. A table of the registered packages (its repository, its minimum version in each host group
// that has one, and the host groups that run it), a form that adds one (its name and the
// repository it comes from, chosen from the registered repositories), Edit and Remove on each row;
// and, below, the repositories (their URL, suites, components, access, the packages that come from
// each and its signing keys) with a form that adds or changes one, and Remove on each row.
//
// A package is registered when a host group lists it or the operator added it here; the Host
// groups page offers these and no other. The minimum version of a package belongs to a host
// group (the package's minimum in that group), so it is set from a package's Edit, for one of the
// groups that run it, and a new package has none until a group runs it.
//
// A repository needs a name, a URL, the suites and components it serves and the key it is signed
// with. The key is a binary OpenPGP keyring chosen as a file; its fingerprints are read from it
// here (`../openpgp.js`), never typed. The package manager is `apt`, the only one there is, and is not
// asked for. A private repository (access `hosts`) names the file on the coordinator's machine
// that holds the credential it is read with.
//
// Calls: Services.List (the packages), Services.Put on Add package, Services.SetRepository and
// Services.SetMinVersion on a package's Save, Services.Remove; Repositories.List, Repositories.Get (the
// keyring of a repository changed without a new key file), Repositories.Put on Save and
// Repositories.Remove. The reason of each write is a fixed sentence: the audit log names the operator.
// A package a host group lists, or a repository a package comes from, is not removed: the page says
// which and the API refuses it all the same.

import { toBase64 } from "../bytes.js";
import { keyringFingerprints } from "../openpgp.js";
import {
  h, action, describeError, field, filePicker, list, select, table, trimmed, words,
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

// A keyring is at most 64 KiB.
const MAX_KEYRING = 65536;

// A package's minimum versions of Services.List, by the host group they are set for.
function minimums(entry) {
  return list(entry, "minVersions")
    .filter((min) => typeof min["role"] === "string")
    .map((min) => `${min["role"]} ${min.version}`);
}

// The keys of a repository, each by the last 16 hex characters of its fingerprint.
function keysOf(repository) {
  return list(repository, "fingerprints").map((print) => print.slice(-16)).join(", ") || "-";
}

// The packages page's sentences for a refusal: the API's token read in words where the page can
// say more than the token does.
function refusalWords(error, inUse) {
  return error && error.reason === "in_use" ? inUse : describeError(error);
}

// The bytes of a chosen file within `limit`, else a sentence saying it is too long. The size is
// read before the contents, so a huge file is not loaded.
async function readFile(file, limit) {
  if (typeof file.size === "number" && file.size > limit) {
    return `The signing key file is at most ${limit} bytes; this one is ${file.size}.`;
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  return bytes.length > limit ? `The signing key file is at most ${limit} bytes; this one is ${bytes.length}.` : bytes;
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
  const names = repositories.map((one) => (one.repository || {}).name).filter((name) => typeof name === "string");
  const state = {
    pkg: { editing: false, name: "", repository: names.includes(DEFAULT_REPOSITORY) ? DEFAULT_REPOSITORY : "", minGroup: "", minVersion: "" },
    repo: { editing: false, name: "", uri: "", suites: "", components: "main", access: "public", credentialFile: "", key: null },
  };
  const show = () => ctx.show(page());
  const packageOf = (name) => packages.find((entry) => entry.service === name);

  // Loads the package `name` into the form.
  const editPackage = (name) => {
    const entry = packageOf(name) || {};
    state.pkg = { editing: true, name, repository: entry.repository || DEFAULT_REPOSITORY, minGroup: "", minVersion: "" };
    show();
  };

  const packageForm = () => {
    const { pkg } = state;
    const entry = packageOf(pkg.name) || {};
    const groups = list(entry, "hostGroups");
    return h("fieldset", {},
      h("legend", {}, pkg.editing ? `Change package ${pkg.name}` : "Add package"),
      pkg.editing ? null : hinted(field("Name", "package-name", pkg, "name"),
        "The package's name as its repository publishes it: lower case letters, digits and -."),
      hinted(
        select("Repository", "package-repository", pkg, "repository",
          [["", "Choose a repository"], ...names.map((name) => [name, name])]),
        names.length === 0
          ? "No repository is registered: add one under Repositories below first."
          : "Where hosts install the package from; the repositories below."),
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
        action(ctx, pkg.editing ? "Save package" : "Add package", "put-package", async () => {
          const name = trimmed(pkg.name);
          if (!PACKAGE_NAME.test(name)) {
            return "Name the package: 1 to 64 lower case letters, digits or -.";
          }
          if (pkg.repository === "") {
            return "Choose the repository the package comes from.";
          }
          if (!pkg.editing) {
            await ctx.api.services.put({ service: name, repository: pkg.repository, reason: PUT_REASON });
            state.pkg.name = "";
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
        pkg.editing
          ? action(ctx, "Cancel", "cancel-package", () => {
            state.pkg = { editing: false, name: "", repository: names.includes(DEFAULT_REPOSITORY) ? DEFAULT_REPOSITORY : "", minGroup: "", minVersion: "" };
            show();
          })
          : null));
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
  });

  const packageRows = () => packages.map((entry) => h("tr", {},
    h("td", {}, h("button", {
      type: "button", class: "linklike", name: "open-package",
      on: { click: () => ctx.navigate("package", { package: entry.service }) },
    }, entry.service)),
    h("td", {}, entry.repository || DEFAULT_REPOSITORY),
    h("td", {}, minimums(entry).join(", ") || "-"),
    h("td", {}, list(entry, "hostGroups").join(", ") || "-"),
    h("td", {}, action(ctx, "Edit", "edit-package", () => editPackage(entry.service)),
      removePackage(entry.service))));

  // Loads the repository `one` of Repositories.List into the form.
  const editRepository = (one) => {
    const body = one.repository || {};
    state.repo = {
      editing: true,
      name: body.name,
      uri: body.uri || "",
      suites: list(body, "suites").join(", "),
      components: list(body, "components").join(", "),
      access: body.access === "hosts" ? "hosts" : "public",
      credentialFile: body.credentialFile || "",
      key: null,
    };
    show();
  };

  // The repository the form describes, with its keyring and fingerprints: from the file chosen, or,
  // for a repository changed without one, from what the registry holds. A sentence when it cannot.
  const repositoryFromForm = async () => {
    const { repo } = state;
    const name = trimmed(repo.name);
    const uri = trimmed(repo.uri);
    const suites = words(repo.suites);
    const components = words(repo.components);
    if (!REPOSITORY_NAME.test(name)) {
      return { problem: "Name the repository: 1 to 32 lower case letters, digits or -." };
    }
    if (!HTTPS_ADDRESS.test(uri)) {
      return { problem: "The URL is an https address with a host." };
    }
    if (suites.length === 0 || components.length === 0) {
      return { problem: "Name at least one suite and one component." };
    }
    if (repo.access === "hosts" && trimmed(repo.credentialFile) === "") {
      return { problem: "A private repository needs the credential file's path on the coordinator." };
    }
    let keyring;
    let fingerprints;
    if (repo.key !== null) {
      const bytes = await readFile(repo.key, MAX_KEYRING);
      if (typeof bytes === "string") {
        return { problem: bytes };
      }
      const read = keyringFingerprints(bytes);
      if (read.problem) {
        return { problem: read.problem };
      }
      keyring = toBase64(bytes);
      fingerprints = read.fingerprints;
    } else if (repo.editing) {
      const held = ((await ctx.api.repositories.get({ name })).repository || {}).repository || {};
      keyring = held.keyring;
      fingerprints = list(held, "fingerprints");
    } else {
      return { problem: "Choose the signing key file." };
    }
    const repository = { name, kind: "apt", uri, suites, components, fingerprints, keyring, access: repo.access };
    if (repo.access === "hosts") {
      repository.credentialFile = trimmed(repo.credentialFile);
    }
    return { repository };
  };

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

  const repositoryForm = () => {
    const { repo } = state;
    return h("fieldset", {},
      h("legend", {}, repo.editing ? `Change repository ${repo.name}` : "Add repository"),
      repo.editing ? null : hinted(field("Name", "repository-name", repo, "name"),
        "What packages call it: lower case letters, digits and -."),
      hinted(field("URL", "repository-uri", repo, "uri"), "The repository's root: an https address with a host."),
      hinted(field("Suites", "repository-suites", repo, "suites"),
        "The channels it publishes, separated by commas or spaces (testing, stable)."),
      hinted(field("Components", "repository-components", repo, "components"),
        "The components of every suite (main)."),
      hinted(filePicker("Signing key", "repository-key", repo, "key"),
        repo.editing
          ? "The binary keyring its suites are signed with (gpg --export); leave it empty to keep the one held."
          : "The binary keyring its suites are signed with (gpg --export, not --armor); its fingerprints are read from it."),
      hinted(redrawn(select("Who reads it", "repository-access", repo, "access",
        [["public", "Hosts read it from the URL"], ["hosts", "Private: hosts read it through the coordinator"]])),
        "A private repository is read by the coordinator alone, with a credential."),
      repo.access === "hosts"
        ? hinted(field("Credential file", "repository-credential", repo, "credentialFile"),
          "The absolute path, on the coordinator's machine, of the file that holds the Authorization header's value.")
        : null,
      h("div", { class: "buttons" },
        action(ctx, repo.editing ? "Save repository" : "Add repository", "put-repository", async () => {
          const built = await repositoryFromForm();
          if (built.problem) {
            return built.problem;
          }
          await ctx.api.repositories.put({ repository: built.repository, reason: PUT_REASON });
          ctx.refresh();
          return `Saved repository ${built.repository.name}.`;
        }),
        repo.editing
          ? action(ctx, "Cancel", "cancel-repository", () => {
            state.repo = { editing: false, name: "", uri: "", suites: "", components: "main", access: "public", credentialFile: "", key: null };
            show();
          })
          : null));
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
  });

  const repositoryRows = () => repositories.map((one) => {
    const body = one.repository || {};
    return h("tr", {},
      h("td", {}, body.name),
      h("td", {}, body.uri || "-"),
      h("td", {}, list(body, "suites").join(", ") || "-"),
      h("td", {}, list(body, "components").join(", ") || "-"),
      h("td", {}, body.access === "hosts" ? "private" : "public"),
      h("td", {}, list(one, "services").join(", ") || "-"),
      h("td", {}, keysOf(body)),
      h("td", {}, action(ctx, "Edit", "edit-repository", () => editRepository(one)), removeRepository(one)));
  });

  const page = () => h("section", { class: "page packages" },
    h("h2", {}, "Packages"),
    h("p", { class: "note" }, "The packages the coordinator can deploy. A host group is given packages from this list on the Host groups page."),
    packagesFailure !== null ? h("p", { class: "error", role: "alert" }, `Packages: ${describeError(packagesFailure)}`) : null,
    packagesFailure === null && packages.length === 0 ? h("p", { class: "note" }, "No package is registered yet.") : null,
    packages.length > 0
      ? table(["Package", "Repository", "Minimum version", "Host groups", ""], packageRows())
      : null,
    packageForm(),
    h("h3", {}, "Repositories"),
    h("p", { class: "note" }, "Where packages are installed from. Each package comes from one of these."),
    repositoriesFailure !== null ? h("p", { class: "error", role: "alert" }, `Repositories: ${describeError(repositoriesFailure)}`) : null,
    repositoriesFailure === null && repositories.length === 0 ? h("p", { class: "note" }, "No repository is registered yet.") : null,
    unregistered.length > 0
      ? h("p", { class: "error", role: "alert" }, `Packages come from repositories that are not registered: ${unregistered.join(", ")}. Hosts of those packages are sent no repository until they are added.`)
      : null,
    repositories.length > 0
      ? table(["Repository", "URL", "Suites", "Components", "Access", "Packages", "Keys", ""], repositoryRows())
      : null,
    repositoryForm());
  return page();
}
