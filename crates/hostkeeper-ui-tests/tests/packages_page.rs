//! The Packages page, run in the JavaScript engine against the fake gateway of `support`: the
//! table of registered packages, the form that adds one (its repository chosen from the registry's
//! repositories), Edit and Remove on each row, the Repositories section with its form, and the
//! reading of a keyring's fingerprints that the form relies on.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

/// Driver helpers: the last tree the page showed, and the choice of a file in the key picker.
const HELPERS: &str = r#"
const last = () => seen.shown[seen.shown.length - 1];
const file = (bytes, size) => ({ name: "key.gpg", size: size === undefined ? bytes.length : size, arrayBuffer: async () => Uint8Array.from(bytes).buffer });
const chooseKey = (t, f) => named(t, "repository-key")[0].on.change({ target: { files: [f] } });
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

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
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
         "keyringSha256": "ab", "generation": "2", "services": ["js-agent"]},
        {"repository": {"name": "komira", "kind": "apt", "uri": "https://packages.example.com/apt",
                        "suites": ["testing", "stable"], "components": ["main", "extra"],
                        "fingerprints": ["BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB33334444"]},
         "keyringSha256": "cd", "generation": "1", "services": ["hostkeeper", "tierstore"]},
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
    // repository form alone, "Add package" the package form alone (replacing the other), Cancel
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
const start = [form(tree), names(tree).filter((n) => n.startsWith("put-") || n.startsWith("cancel-"))];
await click(tree, "add-repository");
const repo = [form(last()), names(last()).filter((n) => n.startsWith("put-") || n.startsWith("cancel-"))];
await click(last(), "add-package");
const pkg = [form(last()), names(last()).filter((n) => n.startsWith("put-") || n.startsWith("cancel-"))];
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
            "repo": [1, ["put-repository", "cancel-repository"]],
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
    assert!(at("Add repository") < at("Repository URL Used by"));
    assert!(at("Repository URL Used by") < at("Add package"));
    assert!(at("Add package") < at("Package Repository Host groups"));
}

#[test]
fn each_list_has_one_description_and_marks_add_primary_and_remove_danger() {
    // Proves: each list's header holds its title, one description and its Add button in a
    // `list-head`; every Add is `primary` and every Remove, of a package and of a repository, is
    // `danger`; each row's Edit and Remove sit in one `row-actions` cell; Edit is not danger.
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
  edit: [...classes("edit-package"), ...classes("edit-repository")],
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
fn the_repositories_section_lists_each_with_its_packages_keys_and_access() {
    // Proves: each repository is a row with its name, its URL and how many packages come from it,
    // with Edit and Remove; a repository packages name that the registry lacks is shown as an
    // alert.
    // Catches: a table that hides how many packages use a repository, or an unregistered one.
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
            json!(["Repository", "URL", "Used by", ""]),
            json!([
                "acme",
                "https://acme.example.com/apt",
                "used by 1 package",
                "Edit Remove"
            ]),
            json!([
                "komira",
                "https://packages.example.com/apt",
                "used by 2 packages",
                "Edit Remove"
            ]),
        ]
    );
    assert_shows(
        &out,
        &["Packages come from repositories that are not registered: ghost."],
    );
}

#[test]
fn adding_a_repository_reads_the_fingerprints_from_the_key_file_and_puts_it() {
    // Proves: Add repository sends Repositories.Put with the name, URL, suites and components as
    // typed (commas and spaces divide), kind apt, the keyring as base64 and the fingerprints the
    // registry reads from that keyring (never typed), access public and a fixed reason.
    // Catches: a fingerprint typed or wrongly derived, a keyring sent in another encoding, or a
    // field the form should not ask for.
    let key = keyring(7);
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Put"), json!({}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-repository");
const form = last();
type(form, "repository-name", "spare");
type(form, "repository-uri", " https://spare.example.com/apt ");
type(form, "repository-suites", "testing, stable");
type(form, "repository-components", "main contrib");
chooseKey(form, file({}));
await click(form, "put-repository");
return null;
"#,
            js_bytes(&key)
        ),
    ));
    assert_eq!(
        bodies_of(&ui, "Repositories/Put"),
        [json!({
            "repository": {
                "name": "spare", "kind": "apt", "uri": "https://spare.example.com/apt",
                "suites": ["testing", "stable"], "components": ["main", "contrib"],
                "fingerprints": [fingerprint_of(7)], "keyring": base64(&key), "access": "public"
            },
            "reason": "set in the console"
        })]
    );
    assert_eq!(out["seen"]["reports"], json!(["Saved repository spare."]));
    assert_eq!(out["seen"]["refreshes"], json!(1));
}

#[test]
fn a_repository_form_that_breaks_a_rule_sends_nothing_and_says_why() {
    // Proves: a bad name, a URL that is not https, no suite, a private repository without its
    // credential file, no key file, a key that is armored text, one that is not a keyring, one
    // that is too big and one that cannot be read each report a sentence and send no Put.
    // Catches: a Put the coordinator would refuse, or a key file sent that was never read.
    let mut ui = Ui::start();
    replies(&mut ui);
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
const say = async () => {{ await click(last(), "put-repository"); return seen.reports[seen.reports.length - 1]; }};
const out = [];
await click(tree, "add-repository");
out.push(await say());
type(last(), "repository-name", "spare");
out.push(await say());
type(last(), "repository-uri", "http://spare.example.com");
out.push(await say());
type(last(), "repository-uri", "https://spare.example.com");
out.push(await say());
type(last(), "repository-suites", "testing");
out.push(await say());
choose(last(), "repository-access", "hosts");
const priv = last();
out.push(await say());
choose(last(), "repository-access", "public");
out.push(await say());
chooseKey(last(), file([45, 45, 45, 45, 45]));
out.push(await say());
chooseKey(last(), file([1, 2, 3]));
out.push(await say());
chooseKey(last(), file([], 70000));
out.push(await say());
chooseKey(last(), file([], 0));
out.push(await say());
chooseKey(last(), {{ name: "k", arrayBuffer: async () => new ArrayBuffer(70000) }});
out.push(await say());
return out;
"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!([
            "Name the repository: 1 to 32 lower case letters, digits or -.",
            "The URL is an https address with a host.",
            "The URL is an https address with a host.",
            "Name at least one suite and one component.",
            "Choose the signing key file.",
            "A private repository needs the credential file's path on the coordinator.",
            "Choose the signing key file.",
            "The signing key is armored text; export it in binary form (gpg --export, without --armor).",
            "The signing key file is not a binary OpenPGP keyring.",
            "The signing key file is at most 65536 bytes; this one is 70000.",
            "The signing key file is empty.",
            "The signing key file is at most 65536 bytes; this one is 70000."
        ])
    );
    assert_eq!(bodies_of(&ui, "Repositories/Put"), Vec::<Value>::new());
}

#[test]
fn a_private_repository_sends_its_credential_file_and_a_public_one_sends_none() {
    // Proves: access `hosts` shows the credential file field and sends its path; changing it back
    // to public drops the path from the request.
    // Catches: a credential path sent with a public repository, which the coordinator refuses.
    let key = keyring(9);
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(&m("Repositories/Put"), json!({}));
    ui.reply(&m("Repositories/Put"), json!({}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "add-repository");
type(last(), "repository-name", "priv");
type(last(), "repository-uri", "https://priv.example.com");
type(last(), "repository-suites", "web");
chooseKey(last(), file({}));
choose(last(), "repository-access", "hosts");
const shown = last();
type(shown, "repository-credential", " /etc/hostkeeper/priv.cred ");
await click(shown, "put-repository");
choose(shown, "repository-access", "public");
const credentials = nodes(last()).filter((n) => n.attrs["name"] === "repository-credential").length;
await click(last(), "put-repository");
return credentials;
"#,
            js_bytes(&key)
        ),
    ));
    assert_eq!(out["out"], 0);
    let puts = bodies_of(&ui, "Repositories/Put");
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[0]["repository"]["access"], "hosts");
    assert_eq!(
        puts[0]["repository"]["credentialFile"],
        "/etc/hostkeeper/priv.cred"
    );
    assert_eq!(puts[1]["repository"]["access"], "public");
    assert!(puts[1]["repository"].get("credentialFile").is_none());
    assert_eq!(out["seen"]["refreshes"], json!(2));
}

#[test]
fn changing_a_repository_keeps_the_key_it_holds_unless_a_new_one_is_chosen() {
    // Proves: Edit loads the repository's URL, suites, components and access; Save with no key
    // file reads the repository (Repositories.Get) and puts it again with the keyring and
    // fingerprints held; Save with a key file puts the new key and reads nothing.
    // Catches: a change that drops or silently rotates the signing key.
    let held = keyring(3);
    let fresh = keyring(5);
    let mut ui = Ui::start();
    replies(&mut ui);
    ui.reply(
        &m("Repositories/Get"),
        json!({"repository": {"repository": {
            "name": "komira", "kind": "apt", "uri": "https://packages.example.com/apt",
            "suites": ["testing", "stable"], "components": ["main", "extra"],
            "fingerprints": [fingerprint_of(3)], "keyring": base64(&held)}}}),
    );
    ui.reply(&m("Repositories/Put"), json!({}));
    ui.reply(&m("Repositories/Put"), json!({}));
    let out = ui.run(&page_driver(
        "packages",
        "{}",
        "[]",
        &format!(
            r#"{HELPERS}
await click(tree, "edit-repository", 1);
const loaded = last();
const form = [named(loaded, "repository-uri")[0].attrs.value, named(loaded, "repository-suites")[0].attrs.value, named(loaded, "repository-components")[0].attrs.value, chosen(loaded, "repository-access")];
type(loaded, "repository-uri", "https://mirror.example.com/apt");
await click(loaded, "put-repository");
chooseKey(loaded, file({}));
await click(loaded, "put-repository");
return [...form, text(loaded).includes("Signing keys held: BBBBBBBB33334444.")];
"#,
            js_bytes(&fresh)
        ),
    ));
    assert_eq!(
        out["out"],
        json!([
            "https://packages.example.com/apt",
            "testing, stable",
            "main, extra",
            "public",
            true
        ])
    );
    assert_eq!(
        bodies_of(&ui, "Repositories/Get"),
        [json!({"name": "komira"})]
    );
    let puts = bodies_of(&ui, "Repositories/Put");
    assert_eq!(puts.len(), 2);
    assert_eq!(
        puts[0]["repository"]["uri"],
        "https://mirror.example.com/apt"
    );
    assert_eq!(puts[0]["repository"]["keyring"], base64(&held));
    assert_eq!(
        puts[0]["repository"]["fingerprints"],
        json!([fingerprint_of(3)])
    );
    assert_eq!(puts[1]["repository"]["keyring"], base64(&fresh));
    assert_eq!(
        puts[1]["repository"]["fingerprints"],
        json!([fingerprint_of(5)])
    );
    assert_eq!(puts[1]["repository"]["name"], "komira");
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
