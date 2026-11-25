//! The Mac table of the Xcode page (`ui/pages/xcode.js`, `ui/xcodeplan.js`) in the JavaScript
//! engine against the fake gateway of `support`: the due column by the coordinator's rule, the
//! installed Xcodes, the device-management column and the run link.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m};
use support::xcode::*;

/// The Macs of the due-column test: one per case of the rule, in the groups `mac-build` (keep 2,
/// latest), `mac-pin` (keep 2, default 27.1) and `mac-off` (keep 0).
fn rule_fleet() -> Ui {
    let both = [
        ("27.2", "27B52", false, false),
        ("27.1", "27A10", false, false),
    ];
    let with_selected = |version: &str| -> Vec<Installed<'static>> {
        both.iter()
            .map(|(v, b, _, f)| (*v, *b, *v == version, *f))
            .collect()
    };
    let hosts = vec![
        mac("m-ok", "mac-build", &with_selected("27.2"), &[]),
        mac("m-one", "mac-build", &[("27.1", "27A10", true, false)], &[]),
        mac("m-both", "mac-build", &[("27.0", "27A1", true, false)], &[]),
        mac("m-unsel", "mac-build", &with_selected("27.1"), &[]),
        mac(
            "m-foreign",
            "mac-build",
            &[
                ("27.2", "27B52", true, true),
                ("27.1", "27A10", false, false),
            ],
            &[],
        ),
        mac(
            "m-build",
            "mac-build",
            &[
                ("27.2", "27B99", true, false),
                ("27.1", "27A10", false, false),
            ],
            &[],
        ),
        mac("m-pin", "mac-pin", &with_selected("27.2"), &[]),
        mac("m-pin-ok", "mac-pin", &with_selected("27.1"), &[]),
        mac("m-off", "mac-off", &[], &[]),
        mac("m-none", "", &[], &[]),
        json!({"nodeId": "m-noreport", "hostGroup": "mac-build", "heartbeat": {"platform": "PLATFORM_DARWIN_ARM64"}}),
        json!({"nodeId": "lin-1", "hostGroup": "mac-build", "heartbeat": {"platform": "PLATFORM_LINUX_X86_64"},
               "maintenance": {"host": {"osName": "ubuntu"}, "lookedUnix": "1799999400"}}),
    ];
    fleet(
        store(archives(true), None, vec![]),
        hosts,
        vec![
            group("mac-build", 2, None),
            group("mac-off", 0, None),
            group("mac-pin", 2, Some("27.1")),
        ],
        vec![],
    )
}

fn run(id: &str, state: &str, fields: &Value) -> Value {
    let mut out = json!({"id": id, "state": format!("MAINTENANCE_STATE_{state}")});
    for (key, value) in fields.as_object().expect("an object") {
        out[key] = value.clone();
    }
    out
}

#[test]
fn the_due_column_follows_the_rule_of_the_coordinators_plan() {
    // Asserts: with the store holding 27.2, 27.1 and 27.0 and a group keeping the newest two, a
    // Mac with both installed and 27.2 selected is "installed"; one lacking 27.2 is "due: 27.2";
    // one lacking both is "due: 27.2, then 27.1"; one with both, 27.1 selected while the default
    // is `latest`, is "27.2 installed, not selected: due to be selected"; a 27.2 in another
    // directory (foreign), and a 27.2 of another build, do not count as installed; a group whose
    // default is 27.1 wants 27.1 selected; a group that keeps 0, and a Mac in no group, are "left
    // out"; a Mac with no report says so; a Linux host has no row. Rows are in group name order,
    // Unassigned last, each group's Macs by name.
    let mut ui = rule_fleet();
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map((r) => [r.name, r.group, r.keeps, r.due]);",
    );
    assert_eq!(
        out["out"],
        json!([
            [
                "m-both",
                "mac-build",
                "keep 2, default latest",
                "due: 27.2, then 27.1"
            ],
            [
                "m-build",
                "mac-build",
                "keep 2, default latest",
                "due: 27.2"
            ],
            [
                "m-foreign",
                "mac-build",
                "keep 2, default latest",
                "due: 27.2"
            ],
            [
                "m-noreport",
                "mac-build",
                "keep 2, default latest",
                "no report"
            ],
            ["m-ok", "mac-build", "keep 2, default latest", "installed"],
            ["m-one", "mac-build", "keep 2, default latest", "due: 27.2"],
            [
                "m-unsel",
                "mac-build",
                "keep 2, default latest",
                "27.2 installed, not selected: due to be selected"
            ],
            [
                "m-off",
                "mac-off",
                "keep 0, default latest",
                "left out: the group keeps no Xcode"
            ],
            [
                "m-pin",
                "mac-pin",
                "keep 2, default 27.1",
                "27.1 installed, not selected: due to be selected"
            ],
            ["m-pin-ok", "mac-pin", "keep 2, default 27.1", "installed"],
            [
                "m-none",
                "Unassigned",
                "-",
                "left out: the group keeps no Xcode"
            ],
        ])
    );
}

#[test]
fn a_version_of_ten_is_newer_than_a_version_of_nine_and_a_default_no_kept_release_has_is_left_alone()
 {
    // Asserts: 27.10 is ordered after 27.9 by its numbers (a group keeping one release keeps
    // 27.10, so a Mac with 27.10 selected is "installed" and one with 27.9 is "due: 27.10"); a
    // default naming a version the group does not keep (27.9 while only 27.10 is kept) makes no
    // selection due; a store holding no Xcode makes nothing due.
    let mut ui = fleet(
        store(
            vec![archive(A, "27.9", "27A9"), archive(B, "27.10", "27B1")],
            None,
            vec![],
        ),
        vec![
            mac("m-new", "g", &[("27.10", "27B1", true, false)], &[]),
            mac("m-old", "g", &[("27.9", "27A9", true, false)], &[]),
            mac("m-pinned", "p", &[("27.10", "27B1", false, false)], &[]),
        ],
        vec![group("g", 1, None), group("p", 1, Some("27.9"))],
        vec![],
    );
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map((r) => [r.name, r.due]);",
    );
    assert_eq!(
        out["out"],
        json!([
            ["m-new", "installed"],
            ["m-old", "due: 27.10"],
            ["m-pinned", "installed"]
        ])
    );
    let mut empty = fleet(
        store(vec![], None, vec![]),
        vec![mac("m-1", "g", &[], &[])],
        vec![group("g", 2, None)],
        vec![],
    );
    let out = drive(&mut empty, "[]", "return macTable(tree).map((r) => r.due);");
    assert_eq!(out["out"], json!(["nothing due: the store holds no Xcode"]));
}

#[test]
fn each_macs_xcodes_are_listed_with_build_selected_and_foreign() {
    // Asserts: a Mac's Xcodes are listed one each, "27.2 (27B52), selected" and "27.1 (27A10),
    // foreign"; a Mac with none says "none".
    let mut ui = fleet(
        store(archives(false), None, vec![]),
        vec![
            mac(
                "m-1",
                "g",
                &[
                    ("27.2", "27B52", true, false),
                    ("27.1", "27A10", false, true),
                ],
                &[],
            ),
            mac("m-2", "g", &[], &[]),
        ],
        vec![group("g", 2, None)],
        vec![],
    );
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map((r) => [r.installed, r.installedText]);",
    );
    assert_eq!(
        out["out"],
        json!([
            [
                ["27.2 (27B52), selected", "27.1 (27A10), foreign"],
                "27.2 (27B52), selected 27.1 (27A10), foreign"
            ],
            [[], "none"]
        ])
    );
}

#[test]
fn the_device_management_column_says_nothing_blocks_updates_or_names_the_keys() {
    // Asserts: a Mac whose report lists no device-management conflict says "nothing that blocks
    // updates"; one that lists some names them, in the report's order; a Mac with no report says
    // "no report".
    let mut ui = fleet(
        store(archives(false), None, vec![]),
        vec![
            mac("m-1", "g", &[], &[]),
            mac("m-2", "g", &[], &["Deferrals", "xcode-app-store"]),
            json!({"nodeId": "m-3", "hostGroup": "g", "heartbeat": {"platform": "PLATFORM_DARWIN_ARM64"}}),
        ],
        vec![group("g", 2, None)],
        vec![],
    );
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map((r) => [r.name, r.device]);",
    );
    assert_eq!(
        out["out"],
        json!([
            ["m-1", "nothing that blocks updates"],
            ["m-2", "Deferrals, xcode-app-store"],
            ["m-3", "no report"],
        ])
    );
}

#[test]
fn a_mac_in_an_open_run_links_the_maintenance_page_and_says_when_the_run_does_not_take_xcode() {
    // Asserts: a Mac of the group of a running run, and a Mac the run names by id, link "run r-1"
    // and "run r-2", which open the Maintenance page; a Mac whose turn in the run (Maintenance.Get)
    // has the xcode kind does not get the note, one whose turn has other kinds does ("which does not
    // take Xcode: schedule after it ends"), whatever kinds the run itself names; a run that is done
    // or cancelled links nothing, and is not asked for its turns; a Mac in no run has no link.
    let mut ui = fleet_with_turns(
        store(archives(false), None, vec![]),
        vec![
            mac("m-1", "mac-build", &[], &[]),
            mac("m-2", "mac-test", &[], &[]),
            mac("m-3", "mac-old", &[], &[]),
            mac("m-4", "mac-none", &[], &[]),
        ],
        vec![
            group("mac-build", 2, None),
            group("mac-test", 2, None),
            group("mac-old", 2, None),
        ],
        vec![
            (
                run(
                    "r-1",
                    "RUNNING",
                    &json!({"hostGroup": "mac-build", "kinds": ["MAINTENANCE_KIND_XCODE"]}),
                ),
                vec![turn("m-1", &["XCODE"])],
            ),
            (
                run(
                    "r-2",
                    "SCHEDULED",
                    &json!({"hostIds": ["m-2"], "kinds": ["MAINTENANCE_KIND_MACOS"]}),
                ),
                vec![turn("m-2", &["MACOS"])],
            ),
            (run("r-3", "DONE", &json!({"hostGroup": "mac-old"})), vec![]),
            (
                run("r-4", "CANCELLED", &json!({"hostGroup": "mac-old"})),
                vec![],
            ),
        ],
    );
    let out = drive(
        &mut ui,
        "[]",
        r#"
const runs = macTable(tree).map((r) => [r.name, r.run]);
await click(named(tree, "mac")[0], "open-run");
return { runs, navigations: seen.navigations };"#,
    );
    assert_eq!(
        out["out"],
        json!({
            "runs": [
                ["m-1", "run r-1"],
                ["m-4", ""],
                ["m-3", ""],
                ["m-2", "run r-2 , which does not take Xcode: schedule after it ends"],
            ],
            "navigations": [{"name": "maintenance", "params": {}}],
        })
    );
    let asked: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Maintenance/Get"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(asked, [json!({"runId": "r-1"}), json!({"runId": "r-2"})]);
}

#[test]
fn a_run_takes_xcode_for_a_mac_only_by_the_kinds_of_its_turn_and_a_failed_read_is_said() {
    // Asserts: the run's own kinds decide nothing: a run that names no kinds does not take Xcode
    // for a Mac that has no turn in it, or a turn with no kinds, and does for a Mac whose turn has
    // xcode among other kinds (the coordinator fixes each host's kinds when it schedules the run);
    // when Maintenance.List or Maintenance.Get is refused the page says the runs could not be read,
    // links no run, and still shows the Macs.
    let mut ui = fleet_with_turns(
        store(archives(false), None, vec![]),
        vec![
            mac("m-1", "g", &[], &[]),
            mac("m-2", "g", &[], &[]),
            mac("m-3", "g", &[], &[]),
        ],
        vec![group("g", 2, None)],
        vec![(
            run("r-1", "HALTED", &json!({"hostGroup": "g"})),
            vec![turn("m-1", &["XCODE", "MACOS"]), turn("m-2", &[])],
        )],
    );
    let out = drive(&mut ui, "[]", "return macTable(tree).map((r) => r.run);");
    assert_eq!(
        out["out"],
        json!([
            "run r-1",
            "run r-1 , which does not take Xcode: schedule after it ends",
            "run r-1 , which does not take Xcode: schedule after it ends",
        ])
    );

    let ui = Ui::start();
    ui.reply(&m("Xcode/List"), store(archives(false), None, vec![]));
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [mac("m-1", "g", &[], &[])]}),
    );
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [group("g", 2, None)]}),
    );
    ui.reply(
        &m("Maintenance/List"),
        json!({"runs": [run("r-1", "HALTED", &json!({"hostGroup": "g"}))]}),
    );
    ui.refuse(&m("Maintenance/Get"), 503, "UNAVAILABLE", "down");
    let mut ui = ui;
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map((r) => [r.name, r.run]);",
    );
    assert_eq!(out["out"], json!([["m-1", ""]]));
    assert!(
        shown(&out).contains(
            "The runs could not be read (UNAVAILABLE down): no Mac is linked to its run."
        ),
        "{}",
        shown(&out)
    );
}

#[test]
fn a_refused_run_list_is_said_and_the_macs_stay() {
    // Asserts: when Maintenance.List is refused the page says the runs could not be read, links no
    // run, and still shows the Macs.
    let ui = Ui::start();
    ui.reply(&m("Xcode/List"), store(archives(false), None, vec![]));
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [mac("m-1", "g", &[], &[])]}),
    );
    ui.reply(
        &m("HostGroups/List"),
        json!({"hostGroups": [group("g", 2, None)]}),
    );
    ui.refuse(&m("Maintenance/List"), 503, "UNAVAILABLE", "down");
    let mut ui = ui;
    let out = drive(
        &mut ui,
        "[]",
        "return macTable(tree).map((r) => [r.name, r.run]);",
    );
    assert_eq!(out["out"], json!([["m-1", ""]]));
    assert!(
        shown(&out).contains(
            "The runs could not be read (UNAVAILABLE down): no Mac is linked to its run."
        ),
        "{}",
        shown(&out)
    );
}
