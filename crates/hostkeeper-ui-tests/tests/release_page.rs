//! The Release page (`ui/pages/release.js`) against the fake gateway: the targets a version was
//! set with, as `Releases.GetRelease` answers them, and each host's pick of them from what
//! `Hosts.List` says of the host; with no target, with several platforms, with an installer no
//! daemon knows, refused, with the hosts unreadable, and with more hosts than the table shows.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{m, page_driver, shown};

const ARCHIVE_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const DEB_DIGEST: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const ODD_DIGEST: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const MAC_DIGEST: &str = "4444444444444444444444444444444444444444444444444444444444444444";
const BUNDLE_DIGEST: &str = "5555555555555555555555555555555555555555555555555555555555555555";
const INDEX_DIGEST: &str = "6666666666666666666666666666666666666666666666666666666666666666";

/// One entry of a release, as GetRelease answers it.
fn entry(installer: &str, digest: &str, package: Option<&str>) -> Value {
    let mut manifest = json!({"minHostkeeper": "0.1.170"});
    if let Some(package) = package {
        manifest["package"] = json!(package);
    }
    json!({
        "installer": installer,
        "locations": [{"store": "main", "locator": format!("web/{installer}.bin")},
                      {"store": "mirror", "locator": format!("w/{installer}")}],
        "digest": digest,
        "bytes": "4096",
        "attestation": {"digest": BUNDLE_DIGEST, "bytes": "900",
                        "locations": [{"store": "main", "locator": "web/bundle.json"}]},
        "manifest": manifest,
    })
}

/// A release of three platforms and a key the page does not know: on `linux-x86_64` an installer
/// no daemon lists first, then `deb`, then the archive; `linux-arm64` only the unknown one.
fn release() -> Value {
    json!({
        "targets": {
            "macos-arm64": {"entries": [entry("hostkeeper-archive", MAC_DIGEST, None)]},
            "linux-x86_64": {"entries": [
                entry("future-kind", ODD_DIGEST, None),
                entry("deb", DEB_DIGEST, Some("web-server")),
                entry("hostkeeper-archive", ARCHIVE_DIGEST, None),
            ]},
            "linux-arm64": {"entries": [entry("future-kind", ODD_DIGEST, None)]},
            "linux-riscv64": {"entries": [entry("hostkeeper-archive", ARCHIVE_DIGEST, None)]},
        },
        "releaseIndexDigest": INDEX_DIGEST,
    })
}

/// A host of Hosts.List: its platform, the features its daemon announced, the installers the
/// coordinator read of it, and its word of `web` (its health and installer), if any.
fn host(
    id: &str,
    platform: Option<&str>,
    features: &[&str],
    installers: &[&str],
    web: Option<(&str, Option<&str>)>,
) -> Value {
    let mut beat = json!({"features": features});
    if let Some(platform) = platform {
        beat["platform"] = json!(format!("PLATFORM_{platform}"));
    }
    if let Some((health, installer)) = web {
        let mut word = json!({"name": "web", "version": "1.0.0", "health": health});
        if let Some(installer) = installer {
            word["installer"] = json!(installer);
        }
        beat["services"] = json!([word]);
    }
    json!({
        "nodeId": id, "role": "web", "live": true, "lastHeartbeatUnix": "1800000000",
        "heartbeat": beat, "installers": installers,
        "services": [{"service": "web", "gateReason": "GATE_REASON_NONE"}],
    })
}

/// The hosts of `web`, one for each way a pick goes, and one host of another service only.
fn hosts() -> Value {
    let v1 = &["heartbeat-v2", "target-v1"][..];
    let old = &["heartbeat-v2"][..];
    let both = &["hostkeeper-archive", "deb"][..];
    let mut other = host("db-1", Some("LINUX_X86_64"), old, &[], None);
    other["services"] = json!([{"service": "db", "gateReason": "GATE_REASON_NONE"}]);
    json!({"hosts": [
        host("old-1", Some("LINUX_X86_64"), old, &[], Some(("HEALTH_HEALTHY", None))),
        host("new-1", Some("LINUX_X86_64"), v1, both, None),
        host("runs-1", Some("LINUX_X86_64"), v1, both, Some(("HEALTH_HEALTHY", None))),
        host("runs-2", Some("LINUX_X86_64"), v1, both, Some(("HEALTH_UNHEALTHY", Some("deb")))),
        host("absent-1", Some("LINUX_X86_64"), v1, both, Some(("HEALTH_ABSENT", Some("rpm")))),
        host("drop-1", Some("LINUX_X86_64"), v1, &["hostkeeper-archive"], Some(("HEALTH_HEALTHY", Some("rpm")))),
        host("arm-1", Some("LINUX_ARM64"), v1, both, None),
        host("odd-1", Some("LINUX_ARM64"), v1, &["future-kind"], None),
        host("mac-1", Some("DARWIN_ARM64"), old, &[], None),
        host("quiet-1", Some("LINUX_X86_64"), v1, &[], None),
        host("blank-1", None, old, &[], None),
        other,
    ]})
}

/// Renders the Release page of `web` `1.0.0`, then runs `then`; returns `{text, seen, out}`.
fn release_page(ui: &mut Ui, then: &str) -> Value {
    ui.run(&page_driver(
        "release",
        r#"{ service: "web", version: "1.0.0" }"#,
        "[]",
        then,
    ))
}

/// The table rows of the page's `section` of class `class`.
const SECTION_ROWS: &str = r#"
const section = (cls) => nodes(tree).filter((n) => n.attrs["class"] === cls);
return {
  platforms: section("platform").map((s) => ({ heading: text(s.children[0]), rows: rows(s) })),
  picks: section("picks").map((s) => rows(s)),
  counts: section("picks").map((s) => nodes(s).filter((n) => n.tag === "li").map(text)),
  alerts: nodes(tree).filter((n) => n.attrs["role"] === "alert").map(text),
  digest: cell(tree, "Release index digest"),
  keys: cell(tree, "Platforms"),
};"#;

/// The row of one entry of a platform's table.
fn entry_row(n: &str, installer: &str, digest: &str, store: &str, package: &str) -> Value {
    json!([
        n,
        installer,
        digest,
        "4096",
        format!("main:web/{installer}.bin, mirror:w/{store}"),
        BUNDLE_DIGEST,
        package,
        "0.1.170"
    ])
}

const ENTRY_HEAD: [&str; 8] = [
    "#",
    "Installer",
    "Digest",
    "Bytes",
    "Locations",
    "Attestation",
    "Package",
    "Oldest daemon",
];

/// Catches a platform shown out of the order of the platforms, or a key the page does not know
/// dropped; entries out of the order set; an installer no daemon knows shown otherwise than the
/// others, or not at all; a field of an entry (digest, size, every location in order, the
/// attestation's digest, the package, the oldest daemon) left out or taken from another entry;
/// the index digest missing; a call other than GetRelease of the page's service and version
/// then Hosts.List.
#[test]
fn the_page_shows_every_platform_and_entry_in_order_with_an_unknown_installer_as_any_other() {
    let mut ui = Ui::start();
    ui.reply(&m("Releases/GetRelease"), release());
    ui.reply(&m("Hosts/List"), hosts());
    let out = release_page(&mut ui, SECTION_ROWS);
    assert_eq!(
        ui.requests()
            .iter()
            .map(|r| (r.method().to_owned(), r.body()))
            .collect::<Vec<_>>(),
        vec![
            (
                m("Releases/GetRelease"),
                json!({"service": "web", "version": "1.0.0"})
            ),
            (m("Hosts/List"), json!({})),
        ]
    );
    let got = &out["out"];
    assert_eq!(got["digest"], json!(INDEX_DIGEST));
    assert_eq!(
        got["keys"],
        json!("linux-x86_64, linux-arm64, macos-arm64, linux-riscv64")
    );
    let archive = |n: &str, digest: &str| {
        entry_row(n, "hostkeeper-archive", digest, "hostkeeper-archive", "-")
    };
    let odd = |n: &str| entry_row(n, "future-kind", ODD_DIGEST, "future-kind", "-");
    assert_eq!(
        got["platforms"],
        json!([
            {"heading": "linux-x86_64 (3 entries)", "rows": [
                ENTRY_HEAD, odd("1"),
                entry_row("2", "deb", DEB_DIGEST, "deb", "web-server"),
                archive("3", ARCHIVE_DIGEST)]},
            {"heading": "linux-arm64 (1 entry)", "rows": [ENTRY_HEAD, odd("1")]},
            {"heading": "macos-arm64 (1 entry)", "rows": [ENTRY_HEAD, archive("1", MAC_DIGEST)]},
            {"heading": "linux-riscv64 (1 entry)", "rows": [ENTRY_HEAD, archive("1", ARCHIVE_DIGEST)]},
        ])
    );
    assert_eq!(got["alerts"], json!([]));
}

/// Catches a pick that is not the coordinator's rule: a host that runs the service not given
/// the entry of the installer it runs it with (the archive when its heartbeat names none), one
/// that does not given another entry than the first it lists, a host whose daemon predates
/// `target-v1` not counted as listing the archive alone, a word of health `absent` read as
/// running, a dropped installer or a list not read yet not said by its reason; a host whose
/// platform Hosts.List does not name given a reason of the coordinator's rather than shown as
/// unknown; a host of another service shown; the counts not counting each
/// pick once.
#[test]
fn each_host_of_the_service_is_shown_with_its_pick_or_why_it_has_none() {
    let mut ui = Ui::start();
    ui.reply(&m("Releases/GetRelease"), release());
    ui.reply(&m("Hosts/List"), hosts());
    let out = release_page(&mut ui, SECTION_ROWS);
    let row = |id: &str, platform: &str, listed: &str, runs: &str, pick: &str| {
        json!([id, platform, listed, runs, "2027-01-15 08:00:00 UTC", pick])
    };
    assert_eq!(
        out["out"]["picks"],
        json!([[
            [
                "Host",
                "Platform",
                "Installers listed",
                "Runs with",
                "Last heartbeat",
                "Picked"
            ],
            row(
                "old-1",
                "linux-x86_64",
                "-",
                "-",
                "hostkeeper-archive (entry 3)"
            ),
            row(
                "new-1",
                "linux-x86_64",
                "hostkeeper-archive, deb",
                "-",
                "deb (entry 2)"
            ),
            row(
                "runs-1",
                "linux-x86_64",
                "hostkeeper-archive, deb",
                "-",
                "hostkeeper-archive (entry 3)"
            ),
            row(
                "runs-2",
                "linux-x86_64",
                "hostkeeper-archive, deb",
                "deb",
                "deb (entry 2)"
            ),
            row(
                "absent-1",
                "linux-x86_64",
                "hostkeeper-archive, deb",
                "rpm",
                "deb (entry 2)"
            ),
            row(
                "drop-1",
                "linux-x86_64",
                "hostkeeper-archive",
                "rpm",
                "none: installer_dropped"
            ),
            row(
                "arm-1",
                "linux-arm64",
                "hostkeeper-archive, deb",
                "-",
                "none: no_build_for_host"
            ),
            row(
                "odd-1",
                "linux-arm64",
                "future-kind",
                "-",
                "future-kind (entry 1)"
            ),
            row(
                "mac-1",
                "macos-arm64",
                "-",
                "-",
                "hostkeeper-archive (entry 1)"
            ),
            row("quiet-1", "linux-x86_64", "-", "-", "none: unheard"),
            row("blank-1", "-", "-", "-", "unknown: platform not reported"),
        ]])
    );
    assert_eq!(
        out["out"]["counts"],
        json!([[
            "deb (entry 2): 3",
            "future-kind (entry 1): 1",
            "hostkeeper-archive (entry 1): 1",
            "hostkeeper-archive (entry 3): 2",
            "none: installer_dropped: 1",
            "none: no_build_for_host: 1",
            "none: unheard: 1",
            "unknown: platform not reported: 1",
        ]])
    );
    assert!(shown(&out).contains("Hosts of web (11)"), "{}", shown(&out));
    assert!(!shown(&out).contains("db-1"), "{}", shown(&out));
}

/// Catches a release with no target shown as if it had platforms, a missing index digest shown
/// as anything but `-`, and a host of such a release picked anything: each is `no_target`.
#[test]
fn a_release_with_no_target_says_so_and_picks_every_host_nothing() {
    let mut ui = Ui::start();
    ui.reply(&m("Releases/GetRelease"), json!({}));
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [host("old-1", Some("LINUX_X86_64"), &["heartbeat-v2"], &[],
                              Some(("HEALTH_HEALTHY", None)))]}),
    );
    let out = release_page(&mut ui, SECTION_ROWS);
    assert_eq!(out["out"]["digest"], json!("-"));
    assert_eq!(out["out"]["keys"], json!("none"));
    assert_eq!(out["out"]["platforms"], json!([]));
    assert_eq!(out["out"]["counts"], json!([["none: no_target: 1"]]));
    assert!(
        shown(&out).contains("This release names no targets."),
        "{}",
        shown(&out)
    );
}

/// Catches a refusal of GetRelease shown with other words than its code and reason, the page's
/// form not offered again, or the hosts read for a release that could not be: a release the
/// coordinator does not hold (`NOT_FOUND release_unknown`) and an unreachable coordinator each
/// show their error, the form keeps the service and version, and Hosts.List is never called.
#[test]
fn a_refused_release_shows_the_refusal_and_reads_no_host() {
    for (refusal, said) in [
        (
            Some((404, "NOT_FOUND", "release_unknown")),
            "NOT_FOUND release_unknown",
        ),
        (None, "NETWORK"),
    ] {
        let mut ui = Ui::start();
        match refusal {
            Some((status, code, reason)) => {
                ui.refuse(&m("Releases/GetRelease"), status, code, reason);
            }
            None => ui.drop_request(&m("Releases/GetRelease")),
        }
        let out = release_page(
            &mut ui,
            r#"return {
  alerts: nodes(tree).filter((n) => n.attrs["role"] === "alert").map(text),
  service: named(tree, "service")[0].attrs.value,
  version: named(tree, "version")[0].attrs.value,
};"#,
        );
        assert_eq!(
            out["out"],
            json!({"alerts": [said], "service": "web", "version": "1.0.0"})
        );
        assert_eq!(ui.methods(), vec![m("Releases/GetRelease")]);
        assert!(shown(&out).contains("web 1.0.0"), "{}", shown(&out));
    }
}

/// Catches a failure of Hosts.List that hides the release it read, or that is shown without its
/// code and reason.
#[test]
fn hosts_that_cannot_be_read_leave_the_targets_shown() {
    let mut ui = Ui::start();
    ui.reply(&m("Releases/GetRelease"), release());
    ui.refuse(&m("Hosts/List"), 503, "UNAVAILABLE", "store_unavailable");
    let out = release_page(&mut ui, SECTION_ROWS);
    assert_eq!(
        out["out"]["alerts"],
        json!(["The hosts could not be read: UNAVAILABLE store_unavailable"])
    );
    assert_eq!(out["out"]["platforms"].as_array().map(Vec::len), Some(4));
}

/// `count` hosts of `web` on `linux-x86_64` whose daemon predates `target-v1`, split over two
/// pages of Hosts.List.
fn many(ui: &Ui, count: usize) {
    let all: Vec<Value> = (0..count)
        .map(|i| {
            host(
                &format!("h-{i:04}"),
                Some("LINUX_X86_64"),
                &["heartbeat-v2"],
                &[],
                None,
            )
        })
        .collect();
    let (first, second) = all.split_at(count / 2);
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": first, "nextPageToken": "page-2"}),
    );
    ui.reply(&m("Hosts/List"), json!({"hosts": second}));
}

/// Catches a table that shows more hosts than its bound, shows none past it without saying so,
/// says so when nothing is left out, or counts only the hosts it shows; and hosts of a page of
/// Hosts.List after the first left out: at 200 hosts every one is a row and nothing is said, at
/// 201 the first 200 are rows, the note names both numbers, and the counts count all 201.
#[test]
fn the_host_table_shows_at_most_200_hosts_and_says_how_many_it_left_out() {
    for (count, note) in [
        (200, None),
        (
            201,
            Some("Showing the first 200 of 201 hosts; the counts above count every one."),
        ),
    ] {
        let mut ui = Ui::start();
        ui.reply(&m("Releases/GetRelease"), release());
        many(&ui, count);
        let out = release_page(
            &mut ui,
            r#"
const picks = nodes(tree).find((n) => n.attrs["class"] === "picks");
return {
  rows: rows(picks).length - 1,
  last: rows(picks).slice(-1)[0][0],
  notes: nodes(picks).filter((n) => n.attrs["class"] === "note").map(text).slice(1),
  counts: nodes(picks).filter((n) => n.tag === "li").map(text),
};"#,
        );
        let bodies: Vec<Value> = ui
            .requests()
            .iter()
            .filter(|r| r.method() == m("Hosts/List"))
            .map(support::Request::body)
            .collect();
        assert_eq!(bodies, vec![json!({}), json!({"pageToken": "page-2"})]);
        assert_eq!(out["out"]["rows"], json!(200));
        assert_eq!(out["out"]["last"], json!("h-0199"));
        assert_eq!(
            out["out"]["notes"],
            json!(note.map_or_else(Vec::new, |n| vec![n]))
        );
        assert_eq!(
            out["out"]["counts"],
            json!([format!("hostkeeper-archive (entry 3): {count}")])
        );
        assert!(
            shown(&out).contains(&format!("Hosts of web ({count})")),
            "{}",
            shown(&out)
        );
    }
}

/// Catches a page without a service or a version that calls anything, a Show that navigates
/// with a field empty, and one that does not trim what was typed.
#[test]
fn the_form_asks_for_a_service_and_a_version_before_it_opens_a_release() {
    let mut ui = Ui::start();
    let out = ui.run(&page_driver(
        "release",
        r#"{ service: "web" }"#,
        "[]",
        r#"
await click(tree, "show");
type(tree, "service", "  web ");
type(tree, "version", "");
await click(tree, "show");
type(tree, "version", " 2.0.0 ");
await click(tree, "show");"#,
    ));
    assert!(ui.methods().is_empty(), "{:?}", ui.methods());
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Name a service and a version first.",
            "Name a service and a version first."
        ])
    );
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "release", "params": {"service": "web", "version": "2.0.0"}}])
    );
    assert!(
        shown(&out).contains("Name a service and a version to see the targets it was set with."),
        "{}",
        shown(&out)
    );
}

/// Catches a host row that does not open the Host page of that host.
#[test]
fn a_host_opens_its_host_page() {
    let mut ui = Ui::start();
    ui.reply(&m("Releases/GetRelease"), release());
    ui.reply(&m("Hosts/List"), hosts());
    let out = release_page(&mut ui, r#"await click(tree, "open-host", 2);"#);
    assert_eq!(
        out["seen"]["navigations"],
        json!([{"name": "host", "params": {"nodeId": "runs-1"}}])
    );
}

/// Catches a pick read from anything but the host's own platform, features, installers and word
/// of the service, at the edges the page tests do not reach: no host, no heartbeat, targets
/// absent, a list of no entry, an unknown platform enum, an empty installer named, and a word of
/// another service.
#[test]
fn the_pick_at_its_edges() {
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { pickFor, platformKey, platformOrder } from "./pages/release.js";
export async function main() {
  const x86 = { "linux-x86_64": { entries: [{ installer: "deb" }, { installer: "hostkeeper-archive" }] } };
  const v1 = (installers, services) => ({ heartbeat: { platform: "PLATFORM_LINUX_X86_64",
    features: ["target-v1"], services }, installers });
  return {
    picks: [
      pickFor(undefined, "web", x86),
      pickFor({}, "web", x86),
      pickFor(v1(["deb"], []), "web", undefined),
      pickFor(v1(["deb"], []), "web", { "linux-x86_64": {} }),
      pickFor({ heartbeat: { platform: "PLATFORM_LATER" } }, "web", x86),
      pickFor(v1(["hostkeeper-archive"], [{ name: "web", installer: "" }]), "web", x86),
      pickFor(v1(["deb"], [{ name: "db", installer: "rpm" }]), "web", x86),
      pickFor(v1(["deb"], [{ name: "web" }]), "web", x86),
    ],
    keys: ["PLATFORM_LINUX_X86_64", "PLATFORM_LINUX_ARM64", "PLATFORM_DARWIN_ARM64",
      "PLATFORM_UNSPECIFIED", "toString", undefined].map(platformKey),
    order: [platformOrder(undefined), platformOrder({ b: {}, "macos-arm64": {}, a: {}, "linux-x86_64": {} })],
  };
}"#,
    );
    assert_eq!(
        out["picks"],
        json!([
            {"unknown": "platform not reported"},
            {"unknown": "platform not reported"},
            {"reason": "no_target"},
            {"reason": "no_build_for_host"},
            {"unknown": "platform not reported"},
            {"installer": "hostkeeper-archive", "position": 2},
            {"installer": "deb", "position": 1},
            {"reason": "installer_dropped"},
        ])
    );
    assert_eq!(
        out["keys"],
        json!([
            "linux-x86_64",
            "linux-arm64",
            "macos-arm64",
            null,
            null,
            null
        ])
    );
    assert_eq!(
        out["order"],
        json!([[], ["linux-x86_64", "macos-arm64", "a", "b"]])
    );
}
