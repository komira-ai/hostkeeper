//! The web app's sources against the protobuf descriptors and the rules for what they may hold.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_ui_tests::scan::{Finding, fetches, inline_code, network_and_code, token_keepers};
use hostkeeper_ui_tests::{
    CallSite, UI_FILES, UiFile, UnknownCall, call_sites, check_calls, descriptor_methods,
    listed_calls,
};
use prost::Message;
use prost_types::FileDescriptorSet;

fn descriptors() -> FileDescriptorSet {
    FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("hostkeeper-proto's descriptors decode")
}

/// Every call site of every module of the app.
fn sites() -> Vec<CallSite> {
    let mut out = Vec::new();
    for file in UI_FILES
        .iter()
        .filter(|f| f.is_script() && f.path != "api.js")
    {
        out.extend(call_sites(file.path, file.text).unwrap_or_else(|e| panic!("{e}")));
    }
    out
}

fn listed() -> Vec<hostkeeper_ui_tests::Listed> {
    let calls = UiFile::named("calls.json").expect("calls.json is held");
    listed_calls(calls.text).expect("calls.json reads")
}

fn owned(calls: &[&str]) -> Vec<String> {
    calls.iter().map(|c| (*c).to_owned()).collect()
}

fn unknown(call: &str) -> Vec<UnknownCall> {
    vec![UnknownCall {
        call: call.to_owned(),
    }]
}

#[test]
fn every_ui_call_is_a_method_in_the_descriptors() {
    // Catches: a page that calls a method the protos do not define, or a method renamed in the
    // protos and not in the app (or the other way round), in the source or in calls.json.
    let set = descriptors();
    let mut calls: Vec<String> = sites().into_iter().map(|s| s.method).collect();
    calls.extend(listed().into_iter().map(|l| l.method));
    assert!(calls.len() > 1, "the app makes calls: {calls:?}");
    if let Err(unknown) = check_calls(&calls, &set) {
        let names: Vec<String> = unknown.iter().map(ToString::to_string).collect();
        panic!("{}", names.join("\n"));
    }
}

#[test]
fn the_call_checker_refuses_a_method_that_is_not_in_the_descriptors() {
    // Catches: a checker that matches the service only, or the method name only, or ignores the
    // package, or passes everything against an empty descriptor set.
    let set = descriptors();
    let real = "hostkeeper.operator.v1.Hosts/List";
    assert_eq!(check_calls(&owned(&[real]), &set), Ok(()));
    for made_up in [
        "hostkeeper.operator.v1.Releases/Teleport",
        "hostkeeper.operator.v1.Hosts/Reboot",
        "hostkeeper.agent.v1.Hosts/List",
        "hostkeeper.operator.v2.Hosts/List",
        "hostkeeper.operator.v1.Fleet/List",
        "hostkeeper.operator.v1.Hosts.List",
    ] {
        assert_eq!(
            check_calls(&owned(&[real, made_up]), &set),
            Err(unknown(made_up)),
            "{made_up}"
        );
    }
    assert_eq!(
        check_calls(&owned(&[real]), &FileDescriptorSet::default()),
        Err(unknown(real))
    );
}

#[test]
fn calls_json_matches_the_source() {
    // Catches: a page that adds a call without listing it, a listed call no page makes any more,
    // a call listed under the wrong page, and a call outside the pages.
    let mut in_source = BTreeSet::new();
    for site in sites() {
        let page = site
            .page()
            .unwrap_or_else(|| panic!("{}:{} calls outside pages/", site.path, site.line));
        in_source.insert((site.method.clone(), page.to_owned()));
    }
    let mut in_list = BTreeSet::new();
    for entry in listed() {
        assert!(!entry.pages.is_empty(), "{} lists no page", entry.method);
        for page in entry.pages {
            assert!(
                in_list.insert((entry.method.clone(), page.clone())),
                "{} lists {page} twice",
                entry.method
            );
        }
    }
    let unlisted: Vec<_> = in_source.difference(&in_list).collect();
    let stale: Vec<_> = in_list.difference(&in_source).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "called and not listed: {unlisted:?}; listed and not called: {stale:?}"
    );
}

#[test]
fn no_ui_call_names_a_method_the_ci_token_set_desired_path_owns() {
    // Catches: a page that grows a set-desired button, or a list that names the method.
    for file in UI_FILES {
        assert!(
            !file.text.contains("SetDesired"),
            "{} names SetDesired: CI sets the desired version",
            file.path
        );
    }
    assert!(listed().iter().all(|l| !l.method.contains("SetDesired")));
}

fn assert_none(found: Vec<Finding>) {
    let lines: Vec<String> = found.iter().map(ToString::to_string).collect();
    assert!(lines.is_empty(), "{}", lines.join("\n"));
}

#[test]
fn ui_source_never_stores_or_leaks_the_token() {
    // Catches: browser storage, a cookie, the token in the address or the history, or the
    // console (which would log it), anywhere in the app. The scan is first shown to find each of
    // them in a text that holds them.
    // `location` and `.search` are apart in the source: joined, the hygiene lint reads them as a
    // domain.
    let held = concat!(
        "localStorage.setItem(\"t\", token);\nsessionStorage.t = token;\nindexedDB.open(\"t\");\n",
        "document.cookie = token;\nlocation.hash = token;\nconst q = location",
        ".search;\nhistory.replaceState(null, \"\", token);\nconsole.log(token);"
    );
    let found: Vec<usize> = token_keepers("pages/x.js", held)
        .iter()
        .map(|f| f.line)
        .collect();
    assert_eq!(found, (1..=8).collect::<Vec<_>>());
    for file in UI_FILES {
        assert_none(token_keepers(file.path, file.text));
    }
}

#[test]
fn ui_source_loads_nothing_from_the_network_and_runs_no_inline_script() {
    // Catches: a URL of another origin, a CDN or web font, an inline script or style, an on*=
    // handler, eval or new Function, and markup built from text (innerHTML). The scans are first
    // shown to find each in a text that holds it.
    let html = "<link rel=\"stylesheet\" href=\"https://fonts.example.com/a.css\">\n\
                <script>start()</script>\n<button onclick=\"go()\">x</button>";
    assert_eq!(network_and_code("index.html", html).len(), 1);
    assert_eq!(inline_code("index.html", html).len(), 2);
    let js = "el.innerHTML = reply.reason;\neval(text);\nnew Function(text);\nel.onclick = go;";
    assert_eq!(network_and_code("pages/x.js", js).len(), 3);
    assert_eq!(inline_code("pages/x.js", js).len(), 1);
    for file in UI_FILES {
        assert_none(network_and_code(file.path, file.text));
        assert_none(inline_code(file.path, file.text));
    }
}

#[test]
fn only_the_data_layer_and_the_entry_module_call_fetch() {
    // Catches: a page that talks to the network on its own, past the data layer.
    for file in UI_FILES {
        let found = fetches(file.path, file.text);
        match file.path {
            "api.js" | "main.js" => assert_eq!(found.len(), 1, "{}", file.path),
            _ => assert_none(found),
        }
    }
}

/// Every file under `dir`, as a path under `base` with `/` between components.
fn listing(base: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("read a directory of ui/") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            listing(base, &path, out);
        } else {
            let rel = path.strip_prefix(base).expect("under ui/");
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.insert(parts.join("/"));
        }
    }
}

#[test]
fn every_file_under_ui_is_scanned() {
    // Catches: a file added to ui/ and not to this crate's list, which no scan would read.
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let mut on_disk = BTreeSet::new();
    listing(&ui, &ui, &mut on_disk);
    let held: BTreeSet<String> = UI_FILES.iter().map(|f| f.path.to_owned()).collect();
    assert_eq!(on_disk, held);
}

#[test]
fn ui_files_stay_under_1000_lines() {
    for file in UI_FILES {
        assert!(
            file.text.lines().count() < 1000,
            "{} is too long",
            file.path
        );
    }
}

#[test]
fn ui_adds_no_proto_change() {
    // Catches: an app that needs a method of another package, or one the merged protos do not
    // have. Every call is a method of `hostkeeper.operator.v1` as hostkeeper-proto defines it,
    // and the other tests read every body the app sends, and every reply they script, against the
    // same descriptors with unknown fields refused, so a field added for the app fails them too.
    // The protos' own compatibility is hostkeeper-proto's test against
    // proto/compat/baseline.binpb.
    let operator: BTreeSet<String> = descriptor_methods(&descriptors())
        .into_iter()
        .filter(|m| m.starts_with("hostkeeper.operator.v1."))
        .collect();
    let calls: Vec<String> = listed().into_iter().map(|l| l.method).collect();
    assert!(!calls.is_empty());
    for call in calls {
        assert!(
            operator.contains(&call),
            "{call} is not a method of hostkeeper.operator.v1"
        );
    }
}
