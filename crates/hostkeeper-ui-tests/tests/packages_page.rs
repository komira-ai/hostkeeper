//! The Packages page, run in the JavaScript engine against the fake gateway of `support`: the
//! table of registered packages, the form that adds one (its repository chosen from the registry's
//! repositories), Edit and Remove on each row, the Repositories section with its form (a name, a
//! URL and an access; a check that shows what the repository serves and its key, and an add that
//! carries what was shown), and the reading of a keyring's fingerprints (`openpgp.js`).

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

/// Driver helpers: the last tree the page showed, and the choice of a file in the key picker.
const HELPERS: &str = r#"
const last = () => seen.shown[seen.shown.length - 1];
const file = (bytes, size) => ({ name: "key.gpg", size: size === undefined ? bytes.length : size, arrayBuffer: async () => Uint8Array.from(bytes).buffer });
const chooseKey = (t, f) => named(t, "repository-key")[0].on.change({ target: { files: [f] } });
const count = (t, name) => nodes(t).filter((n) => n.attrs && n.attrs["name"] === name).length;
"#;

/// A keyring of one version 4 public key made from `seed`, as the registry's tests build one.
fn keyring(seed: u8) -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend([seed; 8]);
    let mut packet = vec![0xc0 | 6, u8::try_from(body.len()).expect("short")];
    packet.extend(body);
    packet
}

/// The fingerprint of the one key of `keyring(seed)`: the SHA-1 of the key packet's body framed as
/// RFC 4880 section 12.2 says (0x99, a two-octet length, the body), worked out outside the page
/// (`hashlib.sha1` of those bytes), which agrees with the registry's reading of the same bytes
/// (`hostkeeper_release::registry::keys`).
fn fingerprint_of(seed: u8) -> &'static str {
    match seed {
        1 => "D282F759B44189DAE07134C869E048AEDB2EEC39",
        3 => "F91DE8BA0D3943DEA5DECD861D1FB6746EB94D8C",
        5 => "8ED51EF306BCB11C67A6BD58A7B953D0D373860A",
        7 => "B4D7DC84B752E40DFCEB149C411414B2D5AC549B",
        other => panic!("no fingerprint recorded for the key of seed {other}"),
    }
}

/// A byte list as JavaScript source.
fn js_bytes(bytes: &[u8]) -> String {
    format!("{bytes:?}")
}

fn services_reply() -> Value {
    json!({"services": [
        {"service": "hostkeeper", "repository": "komira", "hostGroups": ["all", "web"],
         "minVersions": [{"role": "web", "version": "0.1.200"}]},
        {"service": "js-agent", "repository": "acme"},
        {"service": "tierstore", "repository": "komira", "hostGroups": ["web"]},
    ]})
}

fn repositories_reply() -> Value {
    json!({"repositories": [
        {"repository": {"name": "acme", "kind": "apt", "uri": "https://acme.example.com/apt",
                        "suites": ["testing"], "components": ["main"],
                        "fingerprints": ["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA11112222"],
                        "access": "hosts", "credentialFile": "/etc/hostkeeper/acme.cred"},
         "keyringSha256": "ab", "generation": "2", "services": ["js-agent"],
         "platforms": ["linux-x86_64", "linux-arm64"]},
        {"repository": {"name": "komira", "kind": "apt", "uri": "https://packages.example.com/apt",
                        "suites": ["testing", "stable"], "components": ["main", "extra"],
                        "fingerprints": ["BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB33334444"],
                        "baseUri": "https://packages.example.com"},
         "keyringSha256": "cd", "generation": "1", "services": ["hostkeeper", "tierstore"],
         "platforms": ["linux-x86_64", "linux-arm64", "macos-arm64"]},
    ], "generation": "2"})
}

fn replies(ui: &mut Ui) {
    ui.reply(&m("Services/List"), services_reply());
    ui.reply(&m("Repositories/List"), repositories_reply());
}

fn bodies_of(ui: &Ui, method: &str) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(called, _)| called == &m(method))
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn the_page_lists_the_packages_with_their_repository_minimums_and_host_groups() {
    // Proves: each registered package is a row with its repository, the host groups that run it,
    // and Edit and Remove; a package no group runs shows "-".
    // Catches: a table that leaves out the repository or the groups, or shows a default as a value.
    let mut ui = Ui::start();
    replies(&mut ui);
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        "return rows(tree).slice(3);",
    ));
    assert_eq!(
        out["out"],
        json!([
            ["Package", "Repository", "Host groups", ""],
            ["hostkeeper", "komira", "all, web", "Edit Remove"],
            ["js-agent", "acme", "-", "Edit Remove"],
            ["tierstore", "komira", "web", "Edit Remove"],
        ])
    );
}

#[test]
fn an_empty_registry_says_so_and_add_package_waits_for_a_repository() {
    // Proves: with no package and no repository the page says so in words, "Add repository" is
    // enabled, "Add package" is disabled with the hint "Add a repository first" and does nothing
    // when clicked, and no form is open.
    // Catches: a first package that can be started with no repository to come from.
    let mut ui = Ui::start();
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Repositories/List"), json!({}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        r#"
const flags = [named(tree, "add-repository")[0].attrs.disabled === true, named(tree, "add-package")[0].attrs.disabled === true];
await click(tree, "add-package");
return { flags, shown: seen.shown.length };
"#,
    ));
    assert_eq!(out["out"], json!({"flags": [false, true], "shown": 0}));
    assert_shows(
        &out,
        &[
            "No package is registered yet.",
            "No repository is registered yet.",
            "Add a repository first",
        ],
    );
    assert!(!shown(&out).contains("Save"));
}

#[test]
fn no_form_is_visible_by_default_and_each_add_button_opens_its_own_with_save_and_cancel() {
    // Proves: the page opens with no field and no Save button; "Add repository" opens the
    // repository form alone (Check repository and Cancel), "Add package" the package form alone (replacing the other), Cancel
    // closes each, and a form is not shown again by the redraw after it.
    // Catches: a form shown by default, or an Add button that opens the wrong form.
    let mut ui = Ui::start();
    replies(&mut ui);
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
const form = (t) => nodes(t).filter((n) => n.tag === "fieldset").length;
const names = (t) => nodes(t).filter((n) => n.tag === "button").map((n) => n.attrs["name"]);
const forms = (t) => names(t).filter((n) => n.startsWith("put-") || n.startsWith("check-repository") || n.startsWith("cancel-"));
const start = [form(tree), forms(tree)];
await click(tree, "add-repository");
const repo = [form(last()), forms(last())];
await click(last(), "add-package");
const pkg = [form(last()), forms(last())];
await click(last(), "cancel-package");
const closed = form(last());
await click(last(), "add-repository");
await click(last(), "cancel-repository");
return {{ start, repo, pkg, closed, again: form(last()) }};
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({
            "start": [0, []],
            "repo": [1, ["check-repository", "cancel-repository"]],
            "pkg": [1, ["put-package", "cancel-package"]],
            "closed": 0,
            "again": 0
        })
    );
}

#[test]
fn the_repositories_are_listed_above_the_packages() {
    // Proves: the Repositories heading, its table and its Add button come before the Packages
    // heading, its table and its Add button.
    // Catches: packages listed first, with the repository they come from below them.
    let mut ui = Ui::start();
    replies(&mut ui);
    let out = ui.run(&page_driver("packages", "{}", "[]", ""));
    let text = shown(&out);
    let at = |piece: &str| {
        text.find(piece)
            .unwrap_or_else(|| panic!("{piece:?} not in {text}"))
    };
    assert!(at("Repositories") < at("Add repository"));
    assert!(at("Add repository") < at("Repository URL Platforms Used by"));
    assert!(at("Repository URL Platforms Used by") < at("Add package"));
    assert!(at("Add package") < at("Package Repository Host groups"));
}

#[test]
fn each_list_has_one_description_and_marks_add_primary_and_remove_danger() {
    // Proves: each list's header holds its title, one description and its Add button in a
    // `list-head`; every Add is `primary` and every Remove, of a package and of a repository, is
    // `danger`; each row's Edit (a repository's: Check again) and Remove sit in one `row-actions`
    // cell; Edit is not danger.
    // Catches: an Add button outside its header, a Remove that looks like Edit, or a second
    // description repeated under the page title.
    let mut ui = Ui::start();
    replies(&mut ui);
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        r#"
const classes = (name) => named(tree, name).map((n) => n.attrs.class || "");
const heads = nodes(tree).filter((n) => n.attrs && n.attrs.class === "list-head");
return {
  add: [...classes("add-repository"), ...classes("add-package")],
  remove: [...classes("remove-package"), ...classes("remove-repository")],
  edit: [...classes("edit-package"), ...classes("check-again-repository")],
  heads: heads.length,
  notes: heads.map((n) => nodes(n).filter((m) => m.attrs && m.attrs.class === "note").length),
  cells: nodes(tree).filter((n) => n.attrs && n.attrs.class === "row-actions").length,
};
"#,
    ));
    let got = &out["out"];
    assert_eq!(got["add"], json!(["primary", "primary"]));
    assert_eq!(
        got["remove"],
        json!(["danger", "danger", "danger", "danger", "danger"])
    );
    assert_eq!(got["edit"], json!(["", "", "", "", ""]));
    assert_eq!(got["heads"], json!(2));
    assert_eq!(got["notes"], json!([1, 1]));
    assert_eq!(got["cells"], json!(5));
}

#[test]
fn a_refused_list_shows_an_alert_and_the_rest_of_the_page() {
    // Proves: a refused Services.List and a refused Repositories.List are each shown with their
    // code and reason, and the forms still render.
    // Catches: a Packages page that fails whole because one list did.
    let mut ui = Ui::start();
    ui.refuse(&m("Services/List"), 403, "PERMISSION_DENIED", "forbidden");
    ui.refuse(&m("Repositories/List"), 500, "INTERNAL", "store_failed");
    let out = ui.run(&page_driver("packages", "{}", "[]", ""));
    assert_shows(
        &out,
        &[
            "Packages: PERMISSION_DENIED forbidden",
            "Repositories: INTERNAL store_failed",
            "Add package",
            "Add repository",
        ],
    );
}

#[test]
fn adding_a_package_chooses_its_repository_from_the_registry_and_puts_it() {
    // Proves: Add package opens the form, the repository dropdown offers the registered repositories (komira preselected),
    // Add package sends Services.Put with the name trimmed, that repository and a fixed reason,
    // reports it and refreshes once; the form has no field for a repository's name.
    // Catches: a repository typed instead of chosen, or a Put that carries another body.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(
        &m("Services/Put"),
        json!({"entry": {"seq": "9", "action": "service_put"}}),
    );
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-package");
const form = last();
const offered = choices(form, "package-repository");
const before = chosen(form, "package-repository");
type(form, "package-name", " js-agent ");
choose(form, "package-repository", "acme");
await click(form, "put-package");
return {{ offered, before }};
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({"offered": ["Choose a repository", "acme", "komira"], "before": "komira"})
    );
    assert_eq!(
        bodies_of(&ui, "Services/Put"),
        [json!({"service": "js-agent", "repository": "acme", "reason": "set in the console"})]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(["Added package js-agent from acme."])
    );
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn a_package_without_a_good_name_or_a_repository_is_not_put_and_a_refusal_is_shown() {
    // Proves: a name that breaks the rule, or no repository chosen, reports a sentence and sends
    // nothing; a refused Put shows its code and reason and does not refresh.
    // Catches: a Put the coordinator would refuse, or a refusal swallowed.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.refuse(&m("Services/Put"), 404, "NOT_FOUND", "repository_unknown");
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-package");
const form = last();
await click(form, "put-package");
type(form, "package-name", "Bad Name");
await click(form, "put-package");
type(form, "package-name", "js-agent");
choose(form, "package-repository", "");
await click(form, "put-package");
choose(form, "package-repository", "acme");
await click(form, "put-package");
return seen.reports;
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!([
            "Name the package: 1 to 64 lower case letters, digits or -.",
            "Name the package: 1 to 64 lower case letters, digits or -.",
            "Choose the repository the package comes from.",
            "NOT_FOUND repository_unknown"
        ])
    );
    assert_eq!(bodies_of(&ui, "Services/Put").len(), 1);
    assert_eq!(out["seen"]["refreshes"], json!(0));
}

#[test]
fn removing_a_package_a_group_runs_is_refused_naming_the_groups_and_sends_nothing() {
    // Proves: Remove on a package some host group runs reports the groups by name and calls
    // nothing; Remove on one no group runs asks first, a no sends nothing, a yes sends
    // Services.Remove and refreshes.
    // Catches: a package removed from under the group that deploys it, or a remove without a
    // question.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Services/Remove"), json!({"entry": {"seq": "5"}}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[false, true]",
        r#"
await click(tree, "remove-package", 0);
await click(tree, "remove-package", 1);
await click(tree, "remove-package", 1);
return { reports: seen.reports, questions: seen.questions };
"#,
    ));
    assert_eq!(
        out["out"]["reports"],
        json!([
            "hostkeeper is run by host group all, web: take it out of those groups on the Host groups page first.",
            "Not removed.",
            "Removed package js-agent."
        ])
    );
    assert_eq!(
        out["out"]["questions"],
        json!([
            "Remove package js-agent? No host group lists it, so no host is changed.",
            "Remove package js-agent? No host group lists it, so no host is changed."
        ])
    );
    assert_eq!(
        bodies_of(&ui, "Services/Remove"),
        [json!({"service": "js-agent", "reason": "removed in the console"})]
    );
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn the_apis_in_use_refusal_of_a_remove_is_shown_in_words() {
    // Proves: a Services.Remove the coordinator refuses `in_use` (a group listed the package since
    // the page loaded) says so in a sentence naming the package, and does not refresh.
    // Catches: a bare "FAILED_PRECONDITION in_use" where the page knows what it means.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.refuse(&m("Services/Remove"), 412, "FAILED_PRECONDITION", "in_use");
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[true]",
        r#"await click(tree, "remove-package", 1); return null;"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!(["js-agent: a host group still lists it, so it is not removed."])
    );
    assert_eq!(out["seen"]["refreshes"], json!(0));
}

#[test]
fn editing_a_package_sets_its_repository_and_a_groups_minimum_version() {
    // Proves: Edit loads the package; Save sends Services.SetRepository when the repository
    // changed, and Services.SetMinVersion for the chosen host group with the version typed (empty
    // removes it); with nothing changed it sends nothing; a package no group runs offers no
    // minimum.
    // Catches: a repository sent when it did not change, a minimum set for no group, or a minimum
    // offered for a group that does not run the package.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Services/SetRepository"), json!({"entry": {"seq": "3"}}));
    ui.reply(&m("Services/SetRepository"), json!({"entry": {"seq": "3"}}));
    ui.reply(&m("Services/SetMinVersion"), json!({"entry": {"seq": "4"}}));
    ui.reply(&m("Services/SetMinVersion"), json!({"entry": {"seq": "5"}}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "edit-package", 0);
const loaded = last();
const groups = choices(loaded, "package-min-group");
const repository = chosen(loaded, "package-repository");
const note = text(loaded).includes("Minimum versions set: web 0.1.200.");
await click(loaded, "put-package");
choose(loaded, "package-repository", "acme");
choose(loaded, "package-min-group", "web");
type(loaded, "package-min-version", " 0.1.210 ");
await click(loaded, "put-package");
type(loaded, "package-min-version", "");
await click(loaded, "put-package");
await click(tree, "edit-package", 1);
const lone = last();
const none = nodes(lone).filter((n) => n.attrs["name"] === "package-min-version").length;
return {{ groups, repository, note, none, reports: seen.reports }};
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({
            "groups": ["No change", "all", "web"],
            "repository": "komira",
            "note": true,
            "none": 0,
            "reports": [
                "Nothing to change for hostkeeper.",
                "Changed hostkeeper: repository acme, minimum version of web 0.1.210.",
                "Changed hostkeeper: repository acme, minimum version of web removed."
            ]
        })
    );
    assert_eq!(
        bodies_of(&ui, "Services/SetRepository"),
        [
            json!({"service": "hostkeeper", "repository": "acme", "reason": "set in the console"}),
            json!({"service": "hostkeeper", "repository": "acme", "reason": "set in the console"})
        ]
    );
    assert_eq!(
        bodies_of(&ui, "Services/SetMinVersion"),
        [
            json!({"service": "hostkeeper", "minVersion": {"role": "web", "version": "0.1.210"}}),
            json!({"service": "hostkeeper", "minVersion": {"role": "web", "version": ""}})
        ]
    );
}

#[test]
fn the_repositories_section_lists_each_with_its_platforms_and_packages() {
    // Proves: each repository is a row with its name, its URL (the base URL it was added with, else
    // its apt root), the platforms it serves in words, and how many packages come from it, with
    // Check again and Remove; a repository packages name that the registry lacks is shown as an
    // alert.
    // Catches: a table that hides the platforms or how many packages use a repository, or an
    // unregistered one.
    let mut ui = Ui::start();
    ui.reply(&m("Services/List"), json!({}));
    let mut reply = repositories_reply();
    reply["unregistered"] = json!(["ghost"]);
    ui.reply(&m("Repositories/List"), reply);
    let out = ui.run(&page_driver("packages", "{}", "[]", "return rows(tree);"));
    let rows = out["out"].as_array().expect("rows");
    assert_eq!(
        rows[..3],
        [
            json!(["Repository", "URL", "Platforms", "Used by", ""]),
            json!([
                "acme",
                "https://acme.example.com/apt",
                "Linux x86-64, Linux arm64",
                "used by 1 package",
                "Check again Remove"
            ]),
            json!([
                "komira",
                "https://packages.example.com",
                "Linux x86-64, Linux arm64, macOS arm64",
                "used by 2 packages",
                "Check again Remove"
            ]),
        ]
    );
    assert_shows(
        &out,
        &["Packages come from repositories that are not registered: ghost."],
    );
}

#[test]
fn the_built_in_repository_is_tagged_and_has_no_remove() {
    // Proves: a repository the coordinator has built in shows a "built in" tag beside its name and
    // a Check again but no Remove; one put keeps its Remove.
    // Catches: a Remove offered for a repository the coordinator refuses to remove.
    let mut ui = Ui::start();
    ui.reply(&m("Services/List"), json!({}));
    let mut reply = repositories_reply();
    reply["repositories"][1]["builtin"] = json!(true);
    ui.reply(&m("Repositories/List"), reply);
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        r#"return [rows(tree).slice(1, 3), named(tree, "remove-repository").length];"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            [
                [
                    "acme",
                    "https://acme.example.com/apt",
                    "Linux x86-64, Linux arm64",
                    "used by 1 package",
                    "Check again Remove"
                ],
                [
                    "komira built in",
                    "https://packages.example.com",
                    "Linux x86-64, Linux arm64, macOS arm64",
                    "used by 2 packages",
                    "Check again"
                ]
            ],
            1
        ])
    );
}

/// What a check of `https://origin.example.com` answers: Linux and macOS, two channels, one
/// macOS package kept in another directory, the key `user id` and its two fingerprints.
fn discovery(pinned: bool) -> Value {
    json!({"discovery": {
        "name": "origin", "description": "an origin's packages",
        "channels": ["testing", "stable"],
        "platforms": ["linux-x86_64", "linux-arm64", "macos-arm64"],
        "macosPackages": ["hello"],
        "fingerprints": [
            "CB09CF205649494A92DFC22C70D1F2A25906CB22",
            "F767235D83B0410CFAF4779E8738AD1BE061EEDD"
        ],
        "userIds": ["origin packages <packages@example.com>"],
        "descriptorSha256": "ab".repeat(32),
        "keyringSha256": "cd".repeat(32),
        "pinned": pinned
    }})
}

#[test]
fn checking_a_repository_shows_what_it_serves_and_adding_confirms_exactly_that() {
    // Proves: Check repository sends Repositories.Discover with the trimmed URL and access, shows
    // the platforms in words, the channels, the macOS packages, the key's user id and its
    // fingerprints in groups of four with the line that says to compare them, and fills the name
    // the repository suggests; Add repository then sends Repositories.Add with that name, the URL
    // and access that were checked, the descriptor digest and the fingerprints shown, and a fixed
    // reason, and the page draws again.
    // Catches: a fingerprint typed or altered on the way, an Add that does not carry what was
    // confirmed, or a field the form should not ask for.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Discover"), discovery(false));
    ui.reply(&m("Repositories/Add"), json!({}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-repository");
const before = nodes(last()).filter((n) => n.tag === "input" || n.tag === "select").map((n) => n.attrs["name"]);
type(last(), "repository-uri", " https://origin.example.com ");
await click(last(), "check-repository");
const found = text(last());
const name = named(last(), "repository-name")[0].attrs.value;
await click(last(), "add-repository-confirm");
return {{ before, found, name }};
"#
        ),
    ));
    assert_eq!(
        out["out"]["before"],
        json!(["repository-name", "repository-uri", "repository-access"])
    );
    assert_eq!(out["out"]["name"], "origin");
    let found = out["out"]["found"].as_str().expect("text");
    for piece in [
        "an origin's packages",
        "Platforms: Linux x86-64, Linux arm64, macOS arm64.",
        "Channels: testing, stable.",
        "hello",
        "Key: origin packages <packages@example.com>",
        "CB09 CF20 5649 494A 92DF C22C 70D1 F2A2 5906 CB22",
        "F767 235D 83B0 410C FAF4 779E 8738 AD1B E061 EEDD",
        "Compare this fingerprint with the one the repository's owner publishes.",
    ] {
        assert!(found.contains(piece), "{piece:?} not in {found}");
    }
    assert!(!found.contains("pinned"));
    assert_eq!(
        bodies_of(&ui, "Repositories/Discover"),
        [json!({"baseUri": "https://origin.example.com", "access": "public"})]
    );
    assert_eq!(
        bodies_of(&ui, "Repositories/Add"),
        [json!({
            "name": "origin", "baseUri": "https://origin.example.com", "access": "public",
            "descriptorSha256": "ab".repeat(32),
            "fingerprints": [
                "CB09CF205649494A92DFC22C70D1F2A25906CB22",
                "F767235D83B0410CFAF4779E8738AD1BE061EEDD"
            ],
            "reason": "set in the console"
        })]
    );
    assert_eq!(out["seen"]["reports"], json!(["Added repository origin."]));
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn a_pinned_key_says_so_instead_of_asking_to_compare() {
    // Proves: a key the coordinator says is pinned shows "This is komira's pinned key." and not the
    // line that asks the operator to compare it.
    // Catches: a pinned key presented as one to verify by hand, or the reverse.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Discover"), discovery(true));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-repository");
type(last(), "repository-uri", "https://packages.komira.ai");
await click(last(), "check-repository");
return text(last());
"#
        ),
    ));
    let found = out["out"].as_str().expect("text");
    assert!(found.contains("This is komira's pinned key."), "{found}");
    assert!(!found.contains("Compare this fingerprint"), "{found}");
}

#[test]
fn a_repository_form_that_breaks_a_rule_sends_nothing_and_says_why() {
    // Proves: a URL that is not https or ends in `/`, and a private repository without its
    // credential file, each report a sentence and send no Discover; Add is not offered before a
    // check; after one, a changed URL is refused "check it again" and a bad name is refused, and
    // neither sends an Add.
    // Catches: a call the coordinator would refuse, or an Add of an address other than the one
    // whose key was shown.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Discover"), discovery(false));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
const say = async (name) => {{ await click(last(), name); return seen.reports[seen.reports.length - 1]; }};
const out = [];
await click(tree, "add-repository");
out.push(count(last(), "add-repository-confirm"));
out.push(await say("check-repository"));
type(last(), "repository-uri", "http://origin.example.com");
out.push(await say("check-repository"));
type(last(), "repository-uri", "https://origin.example.com/");
out.push(await say("check-repository"));
type(last(), "repository-uri", "https://origin.example.com");
choose(last(), "repository-access", "hosts");
out.push(await say("check-repository"));
choose(last(), "repository-access", "public");
await click(last(), "check-repository");
type(last(), "repository-name", "Bad Name");
out.push(await say("add-repository-confirm"));
type(last(), "repository-name", "origin");
type(last(), "repository-uri", "https://other.example.com");
out.push(await say("add-repository-confirm"));
return out;
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!([
            0,
            "The URL is an https address with a host and no trailing /.",
            "The URL is an https address with a host and no trailing /.",
            "The URL is an https address with a host and no trailing /.",
            "A private repository needs the credential file's path on the coordinator.",
            "Name the repository: 1 to 32 lower case letters, digits or -.",
            "The address changed since it was checked: check it again."
        ])
    );
    assert_eq!(bodies_of(&ui, "Repositories/Discover").len(), 1);
    assert_eq!(bodies_of(&ui, "Repositories/Add"), Vec::<Value>::new());
}

#[test]
fn a_private_repository_sends_its_credential_file_and_a_public_one_sends_none() {
    // Proves: access `hosts` shows the credential file field and sends its trimmed path in the
    // check and in the add; changing it back to public drops the field and the path.
    // Catches: a credential path sent with a public repository, which the coordinator refuses.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Discover"), discovery(false));
    ui.reply(&m("Repositories/Add"), json!({}));
    ui.reply(&m("Repositories/Discover"), discovery(false));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-repository");
type(last(), "repository-uri", "https://priv.example.com");
choose(last(), "repository-access", "hosts");
const shown = last();
type(shown, "repository-credential", " /etc/hostkeeper/priv.cred ");
await click(shown, "check-repository");
await click(last(), "add-repository-confirm");
choose(last(), "repository-access", "public");
const credentials = nodes(last()).filter((n) => n.attrs["name"] === "repository-credential").length;
await click(last(), "check-repository");
return credentials;
"#
        ),
    ));
    assert_eq!(out["out"], 0);
    let discovers = bodies_of(&ui, "Repositories/Discover");
    assert_eq!(
        discovers[0],
        json!({"baseUri": "https://priv.example.com", "access": "hosts", "credentialFile": "/etc/hostkeeper/priv.cred"})
    );
    assert_eq!(
        discovers[1],
        json!({"baseUri": "https://priv.example.com", "access": "public"})
    );
    let adds = bodies_of(&ui, "Repositories/Add");
    assert_eq!(adds.len(), 1);
    assert_eq!(adds[0]["access"], "hosts");
    assert_eq!(adds[0]["credentialFile"], "/etc/hostkeeper/priv.cred");
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn the_refusals_of_a_check_and_an_add_are_shown_in_words() {
    // Proves: `discovery_failed`, `descriptor_invalid` and `credential_unusable` of a check, and
    // `descriptor_changed` and `builtin` of an add, each report their own sentence, and a refused
    // check leaves no result to add.
    // Catches: a token shown raw, or an Add offered after a check that failed.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.refuse(
        &m("Repositories/Discover"),
        503,
        "UNAVAILABLE",
        "discovery_failed",
    );
    ui.refuse(
        &m("Repositories/Discover"),
        400,
        "INVALID_ARGUMENT",
        "descriptor_invalid",
    );
    ui.refuse(
        &m("Repositories/Discover"),
        412,
        "FAILED_PRECONDITION",
        "credential_unusable",
    );
    ui.reply(&m("Repositories/Discover"), discovery(false));
    ui.refuse(
        &m("Repositories/Add"),
        412,
        "FAILED_PRECONDITION",
        "descriptor_changed",
    );
    ui.refuse(
        &m("Repositories/Add"),
        412,
        "FAILED_PRECONDITION",
        "builtin",
    );
    ui.refuse(
        &m("Repositories/Add"),
        404,
        "NOT_FOUND",
        "unknown_repository",
    );
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
const say = async (name) => {{ await click(last(), name); return seen.reports[seen.reports.length - 1]; }};
const out = [];
await click(tree, "add-repository");
type(last(), "repository-uri", "https://origin.example.com");
out.push(await say("check-repository"));
out.push(await say("check-repository"));
out.push(await say("check-repository"));
out.push(count(last(), "add-repository-confirm"));
out.push(await say("check-repository"));
out.push(await say("add-repository-confirm"));
out.push(await say("add-repository-confirm"));
out.push(await say("add-repository-confirm"));
return out;
"#
        ),
    ));
    let said = out["out"].as_array().expect("sentences");
    assert!(
        said[0]
            .as_str()
            .expect("text")
            .starts_with("The coordinator could not read a repository at that address")
    );
    assert!(
        said[1]
            .as_str()
            .expect("text")
            .starts_with("That address does not serve a valid repository descriptor")
    );
    assert!(
        said[2]
            .as_str()
            .expect("text")
            .starts_with("The coordinator cannot use that credential file")
    );
    assert_eq!(said[3], 0);
    assert!(
        said[5]
            .as_str()
            .expect("text")
            .starts_with("What the repository serves changed since it was checked")
    );
    assert!(
        said[6]
            .as_str()
            .expect("text")
            .starts_with("komira is the organisation's built-in repository")
    );
    assert!(
        said[7]
            .as_str()
            .expect("text")
            .contains("unknown_repository")
    );
    assert_eq!(out["seen"]["refreshes"], json!(0));
}

#[test]
fn check_again_opens_the_form_for_a_repository_with_its_name_locked() {
    // Proves: Check again on a row opens the form with that repository's URL (its base URL, or an
    // apt root without its `/apt`) and access, the name locked (no name field), and its Add sends
    // that repository's name; the doc link of the form leads to the structure section of the
    // repository documentation.
    // Catches: a re-check that can rename the repository, or a form that does not say how a
    // repository is structured.
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Discover"), discovery(false));
    ui.reply(&m("Repositories/Add"), json!({}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "check-again-repository", 0);
const acme = last();
const first = [named(acme, "repository-uri")[0].attrs.value, chosen(acme, "repository-access"), named(acme, "repository-credential")[0].attrs.value];
await click(tree, "check-again-repository", 1);
const komira = last();
const second = [named(komira, "repository-uri")[0].attrs.value, count(komira, "repository-name"), text(komira).includes("Check repository komira again")];
const link = named(komira, "repository-structure-help")[0];
await click(komira, "check-repository");
await click(last(), "add-repository-confirm");
return {{ first, second, href: link.attrs.href, text: link.children.map((c) => c.text).join("") }};
"#
        ),
    ));
    assert_eq!(
        out["out"]["first"],
        json!([
            "https://acme.example.com",
            "hosts",
            "/etc/hostkeeper/acme.cred"
        ])
    );
    assert_eq!(
        out["out"]["second"],
        json!(["https://packages.example.com", 0, true])
    );
    assert_eq!(
        out["out"]["href"],
        "https://github.com/komira-ai/hostkeeper/blob/main/docs/deploy/package-repositories.md#structure-of-a-repository"
    );
    assert_eq!(out["out"]["text"], "How do I structure a repository?");
    let adds = bodies_of(&ui, "Repositories/Add");
    assert_eq!(adds.len(), 1);
    assert_eq!(adds[0]["name"], "komira");
    assert_eq!(adds[0]["baseUri"], "https://packages.example.com");
}

#[test]
fn removing_a_repository_a_package_comes_from_is_refused_naming_the_packages() {
    // Proves: Remove on a repository some package comes from reports the packages and calls
    // nothing; on an unused one it asks, a no sends nothing, a yes sends Repositories.Remove; an
    // `in_use` refusal of the API is shown in words.
    // Catches: a repository removed from under its packages.
    let mut ui = Ui::start();
    ui.reply(&m("Services/List"), json!({}));
    let mut reply = repositories_reply();
    reply["repositories"][0]["services"] = json!([]);
    ui.reply(&m("Repositories/List"), reply);
    ui.reply(&m("Repositories/Remove"), json!({"generation": "3"}));
    ui.refuse(
        &m("Repositories/Remove"),
        412,
        "FAILED_PRECONDITION",
        "in_use",
    );
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[false, true, true]",
        r#"
await click(tree, "remove-repository", 1);
await click(tree, "remove-repository", 0);
await click(tree, "remove-repository", 0);
await click(tree, "remove-repository", 0);
return seen.reports;
"#,
    ));
    assert_eq!(
        out["out"],
        json!([
            "komira is the repository of hostkeeper, tierstore: change those packages' repository first.",
            "Not removed.",
            "Removed repository acme.",
            "acme: a package still comes from it, so it is not removed."
        ])
    );
    assert_eq!(
        bodies_of(&ui, "Repositories/Remove"),
        [
            json!({"name": "acme", "reason": "removed in the console"}),
            json!({"name": "acme", "reason": "removed in the console"})
        ]
    );
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn the_fingerprints_the_page_reads_are_the_sha1_of_each_primary_key_packet() {
    // Proves: the page's SHA-1 gives the standard digests, and its fingerprints of a keyring of
    // two keys with a subkey between them, one of them in the old packet format and one with a
    // two-octet length, are the SHA-1 digests worked out outside the page, primary keys only and in order.
    // Catches: a hash that differs from SHA-1, a subkey listed as a key, or a packet format the
    // reader does not follow.
    let mut keys = keyring(1);
    let mut subkey = vec![4, 0, 0, 0, 1, 22];
    subkey.extend([2u8; 8]);
    keys.extend([0xc0 | 14, u8::try_from(subkey.len()).expect("short")]);
    keys.extend(&subkey);
    let mut old = vec![4, 0, 0, 0, 1, 22];
    old.extend([3u8; 8]);
    keys.extend([0x80 | (6 << 2), u8::try_from(old.len()).expect("short")]);
    keys.extend(&old);
    let mut long = vec![4, 0, 0, 0, 1, 22];
    long.resize(200, 4);
    // A two-octet length: 192 and 8 make 200 bytes.
    keys.extend([0xc0 | 6, 192, 8]);
    keys.extend(&long);
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ keyringFingerprints, sha1Hex }} from "./openpgp.js";
export async function main() {{
  const text = (s) => Uint8Array.from(s, (c) => c.charCodeAt(0));
  return {{
    abc: sha1Hex(text("abc")),
    empty: sha1Hex(new Uint8Array(0)),
    long: sha1Hex(text("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
    keys: keyringFingerprints(Uint8Array.from({})),
  }};
}}"#,
        js_bytes(&keys)
    ));
    assert_eq!(
        out["abc"], "A9993E364706816ABA3E25717850C26C9CD0D89D",
        "{out}"
    );
    assert_eq!(out["empty"], "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
    assert_eq!(out["long"], "84983E441C3BD26EBAAE4AA1F95129E5E54670F1");
    // The second primary key is the first's seed 3 packet, in the old format; the third is 200
    // bytes of 4 after its header.
    assert_eq!(
        out["keys"],
        json!({"fingerprints": [
            fingerprint_of(1),
            fingerprint_of(3),
            "E6F2D2F4DB9D1C033AB4391000525104581F2223"
        ]})
    );
}

#[test]
fn a_keyring_the_page_cannot_name_the_keys_of_is_answered_with_why() {
    // Proves: an empty file, armored text, a packet cut short, a version 6 key, a version 3 key, a
    // file with no public key and a keyring of nine keys each come back as a sentence and no
    // fingerprints.
    // Catches: a keyring read past its end, or fingerprints made of a key this page cannot hash.
    let mut nine = Vec::new();
    for seed in 0..9 {
        nine.extend(keyring(seed));
    }
    let drivers = [
        ("[]", "The signing key file is empty."),
        (
            "[45, 45, 45]",
            "The signing key is armored text; export it in binary form (gpg --export, without --armor).",
        ),
        (
            "[198, 20, 4]",
            "The signing key file is not a binary OpenPGP keyring.",
        ),
        (
            "[198, 255, 0, 0]",
            "The signing key file is not a binary OpenPGP keyring.",
        ),
        (
            "[198, 230, 4]",
            "The signing key file is not a binary OpenPGP keyring.",
        ),
        (
            "[1, 2]",
            "The signing key file is not a binary OpenPGP keyring.",
        ),
        (
            "[198, 3, 6, 0, 0]",
            "The keyring has a version 6 key, whose fingerprint this page does not make; use hkctl repositories put.",
        ),
        (
            "[198, 3, 3, 0, 0]",
            "The keyring has a key of a version this page does not read.",
        ),
        ("[205, 1, 7]", "The signing key file holds no public key."),
    ];
    let mut ui = Ui::start();
    for (bytes, problem) in drivers {
        let out = ui.run(&format!(
            r#"
import {{ keyringFingerprints }} from "./openpgp.js";
export async function main() {{ return keyringFingerprints(Uint8Array.from({bytes})); }}"#
        ));
        assert_eq!(out, json!({"problem": problem}), "{bytes}");
    }
    let out = ui.run(&format!(
        r#"
import {{ keyringFingerprints }} from "./openpgp.js";
export async function main() {{ return keyringFingerprints(Uint8Array.from({})); }}"#,
        js_bytes(&nine)
    ));
    assert_eq!(
        out,
        json!({"problem": "A repository lists at most 8 keys; this keyring has 9."})
    );
}
