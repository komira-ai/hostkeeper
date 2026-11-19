//! The web app's sources against the protobuf descriptors and the rules for what they may hold.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_ui_tests::client_gen::functions;
use hostkeeper_ui_tests::scan::{
    Finding, SOCKET_OPENER, fetches, inline_code, network_and_code, opener, scan_file, sockets,
    token_keepers, vendored_code,
};
use hostkeeper_ui_tests::vendor::{PINS, pins, unpinned};
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

/// The modules that are not scanned for calls: the data layer, which sends them, and the
/// generated client, which names every method.
const SENDERS: [&str; 2] = ["api.js", "gen/client.js"];

/// Every call site of every module of the app.
fn sites() -> Vec<CallSite> {
    let mut out = Vec::new();
    for file in UI_FILES
        .iter()
        .filter(|f| f.is_script() && !SENDERS.contains(&f.path))
    {
        out.extend(call_sites(file.path, file.text).unwrap_or_else(|e| panic!("{e}")));
    }
    out
}

/// The method a site calls, through the generated client's function.
fn method(site: &CallSite) -> String {
    functions(COMMANDS)
        .into_iter()
        .find(|f| f.call() == site.function)
        .unwrap_or_else(|| {
            panic!(
                "{}:{}: api.{} is not a function of the generated client",
                site.path, site.line, site.function
            )
        })
        .method
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
    let mut calls: Vec<String> = sites().iter().map(method).collect();
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
        in_source.insert((method(&site), page.to_owned()));
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
fn only_the_rollout_module_calls_set_desired() {
    // Catches: a page other than the Rollout module that grows a set-desired call, a call site of
    // it outside pages/, a list that names it under another page, and a Rollout module that stops
    // calling it while the list still names it. The package page offers the button by importing
    // the Rollout module's `setDesired`; it makes no call of its own. The generated client defines the function, and
    // so names the method; it calls nothing by itself.
    let method = "hostkeeper.operator.v1.Releases/SetDesired";
    let callers: BTreeSet<String> = sites()
        .into_iter()
        .filter(|s| self::method(s) == method)
        .map(|s| s.page().unwrap_or("outside pages/").to_owned())
        .collect();
    assert_eq!(callers, BTreeSet::from(["rollout".to_owned()]));
    let pages: Vec<Vec<String>> = listed()
        .into_iter()
        .filter(|l| l.method == method)
        .map(|l| l.pages)
        .collect();
    assert_eq!(pages, vec![vec!["rollout".to_owned()]]);
    for file in UI_FILES.iter().filter(|f| {
        ![
            "pages/rollout.js",
            "pages/package.js",
            "calls.json",
            "gen/client.js",
            "gen/client.d.ts",
            "gen/hostkeeper_operator_v1.d.ts",
        ]
        .contains(&f.path)
    }) {
        assert!(
            !file.text.contains("SetDesired") && !file.text.contains("setDesired"),
            "{} names SetDesired: only the Rollout module sets the desired version",
            file.path
        );
    }
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
    for file in app_files() {
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
    for file in app_files() {
        assert_none(network_and_code(file.path, file.text));
        assert_none(inline_code(file.path, file.text));
    }
}

#[test]
fn only_the_data_layer_and_the_entry_module_call_fetch() {
    // Catches: a page that talks to the network on its own, past the data layer.
    for file in app_files() {
        let found = fetches(file.path, file.text);
        match file.path {
            "api.js" | "main.js" => assert_eq!(found.len(), 1, "{}", file.path),
            _ => assert_none(found),
        }
    }
}

#[test]
fn only_the_entry_module_opens_a_socket_and_it_opens_exactly_one() {
    // Catches: a page, the data layer or a vendored library that opens a socket of its own, past
    // the injected `ctx.openSocket`, an entry module that opens none or more than the one, and a
    // socket whose address is not `socketAddress`'s (the page's own host, a path of a fixed
    // shape). A vendored library may name `WebSocket` (to check what it is handed) and never call it.
    assert_eq!(SOCKET_OPENER, "main.js");
    for file in UI_FILES {
        let found = sockets(file.path, file.text);
        match file.path {
            "main.js" => assert_eq!(found.len(), 1, "{}", file.path),
            _ => assert_none(found),
        }
        if file.path != "main.js" && !file.is_vendored() {
            assert!(
                !file.text.contains("WebSocket"),
                "{} names WebSocket: only main.js does",
                file.path
            );
        }
    }
    let main = UiFile::named(SOCKET_OPENER).expect("main.js is held");
    assert_none(opener(main.path, main.text));
    // The import is a line of its own, `socketAddress` and `socketProtocols` are bound nowhere else
    // in main.js, and the word `WebSocket` stands only in the one call (no alias such as
    // `const W = WebSocket`).
    let import = "import { socketAddress, socketProtocols } from \"./socket_address.js\";";
    assert_eq!(
        main.text.lines().filter(|l| *l == import).count(),
        1,
        "main.js takes socketAddress and socketProtocols from socket_address.js"
    );
    for binding in [
        "const socketAddress",
        "let socketAddress",
        "var socketAddress",
        "function socketAddress",
        "socketAddress =",
        "const socketProtocols",
        "let socketProtocols",
        "var socketProtocols",
        "function socketProtocols",
        "socketProtocols =",
        "const window",
        "let window",
        "var window",
    ] {
        assert!(!main.text.contains(binding), "main.js binds {binding}");
    }
    assert_eq!(
        main.text.matches("WebSocket").count(),
        1,
        "main.js names WebSocket once"
    );
}

/// Every file of the app but the vendored libraries.
fn app_files() -> impl Iterator<Item = &'static UiFile> {
    UI_FILES.iter().filter(|f| !f.is_vendored())
}

#[test]
fn every_ui_file_passes_the_scan_of_its_directory() {
    // Catches: a file of the app, or a vendored one, that holds what its directory's rules refuse.
    for file in UI_FILES {
        assert_none(scan_file(file.path, file.text));
    }
}

#[test]
fn vendored_files_match_their_pinned_digests() {
    // Catches: a vendored library changed, added or removed without the change to its pins, and
    // pins that do not read. The checks are first shown to find each in a list that holds it.
    let pinned = pins(PINS).unwrap_or_else(|e| panic!("{e}"));
    assert_none(unpinned(UI_FILES, &pinned));
    let changed = [UiFile {
        path: "vendor/lib/lib.js",
        text: "changed",
    }];
    let held =
        pins("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  vendor/lib/lib.js")
            .expect("one pin reads");
    assert_eq!(unpinned(&changed, &held).len(), 1);
    assert_eq!(unpinned(&changed, &[]).len(), 1);
    assert_eq!(unpinned(&[], &held).len(), 1);
}

#[test]
fn no_vendored_file_calls_new_websocket() {
    // Catches: a vendored library that would open a socket of its own (it is handed the app's one
    // socket, open), or that holds inline script or a handler made from text. The check is first
    // shown to find a call in a vendored text that holds one.
    assert_eq!(
        vendored_code(
            "vendor/lib/websock.js",
            "open(uri, protocols) { this.attach(new WebSocket(uri, protocols)); }"
        )
        .len(),
        1
    );
    for file in UI_FILES.iter().filter(|f| f.is_vendored()) {
        assert_none(vendored_code(file.path, file.text));
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
    // A vendored file is the library's own text, pinned by its digest (vendor.sha256), and is not
    // split: noVNC's core/rfb.js is one module of over 3000 lines. Every file of the app's own is
    // held to the limit.
    for file in UI_FILES.iter().filter(|f| !f.is_vendored()) {
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
