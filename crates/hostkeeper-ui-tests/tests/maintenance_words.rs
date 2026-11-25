//! What the maintenance pages say and how they are laid out, as an operator meets them: a refusal
//! of Maintenance.Schedule, Cancel, RetryHost or PutBack in words, the window line of the Schedule
//! form before anything is typed, the buttons of a group's card and of the form in a spaced row,
//! a date and time field drawn as the other fields, and the host page's tables kept inside a phone's
//! width. Run in the JavaScript engine against the fake gateway of `support`, and over the
//! stylesheets as held.

mod support;

use hostkeeper_ui_tests::UiFile;
use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// The words of each refusal of Maintenance.Schedule an operator can act on, with the HTTP status
/// and code the gateway answers it with.
const SCHEDULE_REFUSALS: [(u16, &str, &str, &str); 7] = [
    (
        412,
        "FAILED_PRECONDITION",
        "nothing_due",
        "Nothing of what was asked is due on those hosts, or every host with something due would \
         be skipped: no run was stored.",
    ),
    (
        412,
        "FAILED_PRECONDITION",
        "last_host",
        "That would take every host of the group at once: no run was stored. To take them all, \
         use the Schedule form of the Maintenance page or the host page with \"Take the group's \
         last host\" ticked.",
    ),
    (
        412,
        "FAILED_PRECONDITION",
        "host_in_maintenance",
        "A host it would take is already in a run that has not ended: no run was stored.",
    ),
    (
        412,
        "FAILED_PRECONDITION",
        "window_ended",
        "The window has already ended: no run was stored.",
    ),
    (
        404,
        "NOT_FOUND",
        "unknown_host",
        "The coordinator knows no host of this id: no run was stored.",
    ),
    (
        404,
        "NOT_FOUND",
        "unknown_group",
        "The coordinator knows no host group of this name: no run was stored.",
    ),
    (
        503,
        "UNAVAILABLE",
        "audit_unavailable",
        "The coordinator could not write the audit entry. Nothing was done; try again.",
    ),
];

/// A Linux report of `due`.
fn report(due: Value) -> Value {
    json!({"host": {"osName": "ubuntu", "osVersion": "24.04", "bootId": "b-1"}, "due": due,
           "lookedUnix": "1799999400"})
}

fn security_due() -> Value {
    json!([{"kind": "MAINTENANCE_KIND_LINUX", "from": "1.0", "to": "1.1", "label": "openssl", "security": true}])
}

/// The host page of `h-1` (group `g`, one security update due).
fn host_ui() -> Ui {
    let ui = Ui::start();
    ui.reply(
        &m("Hosts/Get"),
        json!({"host": {"nodeId": "h-1", "hostGroup": "g", "maintenance": report(security_due()),
                        "maintenanceLookAgeSecs": "600"}}),
    );
    ui.reply(&m("Services/List"), json!({}));
    ui.reply(&m("Layout/Get"), json!({}));
    ui
}

/// The Maintenance page over group `g` (`h-1` due, `h-2` not) with no run.
fn maintenance_ui() -> Ui {
    let ui = Ui::start();
    ui.reply(
        &m("Hosts/List"),
        json!({"hosts": [
            {"nodeId": "h-1", "hostGroup": "g", "maintenance": report(security_due()), "maintenanceLookAgeSecs": "600"},
            {"nodeId": "h-2", "hostGroup": "g", "maintenance": report(json!([])), "maintenanceLookAgeSecs": "600"},
        ]}),
    );
    ui.reply(&m("Maintenance/List"), json!({}));
    ui.reply(
        &m("HostGroups/Get"),
        json!({"hostGroup": {"group": {"name": "g", "packages": ["hostkeeper"], "policy": {}}}}),
    );
    ui
}

/// The Maintenance page with run `r-1` running, its host `h-1` halted.
fn halted_ui() -> Ui {
    let ui = Ui::start();
    ui.reply(&m("Hosts/List"), json!({}));
    let run = json!({"id": "r-1", "state": "MAINTENANCE_STATE_RUNNING", "hostGroup": "g"});
    ui.reply(&m("Maintenance/List"), json!({ "runs": [run] }));
    ui.reply(
        &m("Maintenance/Get"),
        json!({"run": run, "hosts": [{"hostId": "h-1", "hostGroup": "g",
                                      "step": "MAINTENANCE_HOST_STEP_HALTED", "reason": "apply exited 1"}]}),
    );
    ui
}

/// The Maintenance calls sent other than its reads (List, Get), in order.
fn writes(ui: &Ui) -> Vec<String> {
    bodies(ui)
        .into_iter()
        .map(|(method, _)| method)
        .filter(|method| {
            method.starts_with(&m("Maintenance/"))
                && *method != m("Maintenance/List")
                && *method != m("Maintenance/Get")
        })
        .collect()
}

#[test]
fn a_refused_update_says_in_words_why_no_run_was_stored() {
    // Asserts: the host page's Update, refused by the coordinator with each refusal an operator
    // can act on, reports exactly that refusal's sentence (not the code and token); one
    // Maintenance.Schedule was sent each time, and the page was not told to refresh.
    // Catches: "FAILED_PRECONDITION nothing_due" or "... last_host" shown to an operator who
    // pressed Update on a host with nothing due, or on a group's last host.
    for (status, code, token, words) in SCHEDULE_REFUSALS {
        let mut ui = host_ui();
        ui.refuse(&m("Maintenance/Schedule"), status, code, token);
        let out = ui.run(&page_driver(
            "host",
            r#"{ nodeId: "h-1" }"#,
            "[true]",
            r#"
type(tree, "update-reason", "monthly patch");
await click(tree, "update");
return { reports: seen.reports, refreshes: seen.refreshes };"#,
        ));
        assert_eq!(
            out["out"],
            json!({"reports": [words], "refreshes": 0}),
            "{token}"
        );
        assert_eq!(writes(&ui), [m("Maintenance/Schedule")], "{token}");
    }
}

#[test]
fn a_refusal_with_no_words_of_its_own_is_said_as_its_code_and_token() {
    // Asserts: a refusal this page has no sentence for (a token the coordinator may add later,
    // and `invalid_request`) is reported as the code and token the coordinator answered.
    // Catches: an unknown refusal shown as one of the known sentences, or as nothing.
    for (status, code, token) in [
        (412, "FAILED_PRECONDITION", "brand_new_reason"),
        (400, "INVALID_ARGUMENT", "invalid_request"),
    ] {
        let mut ui = host_ui();
        ui.refuse(&m("Maintenance/Schedule"), status, code, token);
        let out = ui.run(&page_driver(
            "host",
            r#"{ nodeId: "h-1" }"#,
            "[true]",
            r#"
type(tree, "update-reason", "monthly patch");
await click(tree, "update");
return seen.reports;"#,
        ));
        assert_eq!(out["out"], json!([format!("{code} {token}")]), "{token}");
    }
}

#[test]
fn a_refused_check_or_schedule_of_the_form_says_why_in_words() {
    // Asserts: on the Maintenance page, the Schedule form of group `g` opened, a Check refused
    // `last_host` and a Schedule refused `nothing_due` each report their sentence; two
    // Maintenance.Schedule calls were sent (the Check without `confirm`, the Schedule with it).
    let mut ui = maintenance_ui();
    ui.refuse(
        &m("Maintenance/Schedule"),
        412,
        "FAILED_PRECONDITION",
        "last_host",
    );
    ui.refuse(
        &m("Maintenance/Schedule"),
        412,
        "FAILED_PRECONDITION",
        "nothing_due",
    );
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[true]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
await click(tree, "open-schedule", 0);
await click(cur(), "schedule-check");
type(cur(), "schedule-reason", "monthly patch");
await click(cur(), "schedule-confirm");
return seen.reports;"#,
    ));
    assert_eq!(
        out["out"],
        json!([SCHEDULE_REFUSALS[1].3, SCHEDULE_REFUSALS[0].3])
    );
    let sent: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m("Maintenance/Schedule"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        sent,
        [
            json!({"hostGroup": "g"}),
            json!({"hostGroup": "g", "reason": "monthly patch", "confirm": true}),
        ]
    );
}

#[test]
fn a_refused_cancel_retry_or_put_back_says_why_in_words() {
    // Asserts: Cancel refused `run_ended`, Retry refused `not_halted` and Put back refused
    // `not_in_run` and `unknown_run` each report their sentence, one call each, and the runs are
    // not read again after a refusal.
    for (button, method, token, words) in [
        (
            "cancel-run",
            "Maintenance/Cancel",
            "run_ended",
            "The run has already ended.",
        ),
        (
            "retry-host",
            "Maintenance/RetryHost",
            "not_halted",
            "This host is not halted any more.",
        ),
        (
            "put-back-host",
            "Maintenance/PutBack",
            "not_in_run",
            "This host is not in the run.",
        ),
        (
            "put-back-host",
            "Maintenance/PutBack",
            "unknown_run",
            "The coordinator knows no run of this id.",
        ),
    ] {
        let mut ui = halted_ui();
        let (status, code) = if token.starts_with("unknown") || token == "not_in_run" {
            (404, "NOT_FOUND")
        } else {
            (412, "FAILED_PRECONDITION")
        };
        ui.refuse(&m(method), status, code, token);
        let out = ui.run(&page_driver(
            "maintenance",
            "{}",
            "[true]",
            &format!(
                r#"
type(tree, "run-reason", "again", 0);
await click(tree, "{button}", 0);
return {{ reports: seen.reports, shown: seen.shown.length }};"#
            ),
        ));
        assert_eq!(
            out["out"],
            json!({"reports": [words], "shown": 0}),
            "{token}"
        );
        assert_eq!(writes(&ui), [m(method)], "{token}");
    }
}

#[test]
fn a_window_field_left_empty_is_said_to_be_not_set() {
    // Asserts: choosing "in a window" with nothing typed shows "Start not set; end not set"; a
    // start typed alone (2027-01-15T22:00 in the fake zone UTC-05:00) shows its UTC time and the
    // end still not set; a start that is no date and time is said to be so.
    // Catches: "Start not a date and time; end not a date and time" shown before the operator
    // has typed anything.
    let mut ui = host_ui();
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "h-1" }"#,
        "[]",
        r#"
const cur = () => seen.shown[seen.shown.length - 1];
const line = () => text(named(cur(), "window-utc")[0]);
choose(tree, "when", "window");
const empty = line();
type(cur(), "window-start", "2027-01-15T22:00");
named(cur(), "window-start")[0].on.change({});
const start = line();
type(cur(), "window-start", "soon");
named(cur(), "window-start")[0].on.change({});
return { empty, start, bad: line() };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "empty": "Start not set; end not set",
            "start": "Start 2027-01-16 03:00:00 UTC; end not set",
            "bad": "Start not a date and time; end not set",
        })
    );
}

/// The names of the buttons of each `div` of class `buttons` in the tree, in order.
const BUTTON_ROWS: &str = r#"
const rowsOf = (t) => nodes(t).filter((n) => n.tag === "div" && n.attrs["class"] === "buttons")
  .map((d) => nodes(d).filter((n) => n.tag === "button").map((n) => n.attrs["name"]));"#;

#[test]
fn the_buttons_of_a_group_card_and_of_the_form_sit_in_a_spaced_row() {
    // Asserts: on the Maintenance page the group card's Update the group and Schedule... sit in
    // one row of class `buttons`, after the card's Reason field; once the form is open, its Check
    // and Schedule sit in another; and the stylesheet gives that class a gap.
    // Catches: two buttons drawn touching, with no space between them.
    let mut ui = maintenance_ui();
    let out = ui.run(&page_driver(
        "maintenance",
        "{}",
        "[]",
        &format!(
            r#"{BUTTON_ROWS}
const cur = () => seen.shown[seen.shown.length - 1];
const closed = rowsOf(tree);
await click(tree, "open-schedule", 0);
return {{ closed, open: rowsOf(cur()) }};"#
        ),
    ));
    assert_eq!(
        out["out"],
        json!({
            "closed": [["update", "open-schedule"]],
            "open": [["update", "open-schedule"], ["schedule-check", "schedule-confirm"]],
        })
    );
    let css = stylesheet("app.css");
    let body = rule(&css, ".buttons");
    assert!(
        body.contains("display: flex") && body.contains("gap: 8px"),
        "{body}"
    );
}

/// The text of the held stylesheet `name`.
fn stylesheet(name: &str) -> String {
    UiFile::named(name)
        .unwrap_or_else(|| panic!("{name} is held"))
        .text
        .to_owned()
}

/// The body of the first rule of `css` whose selector list is exactly `selector` or holds it as
/// one of its comma-separated selectors.
fn rule(css: &str, selector: &str) -> String {
    let mut at = 0;
    while let Some(open) = css[at..].find('{') {
        let open = at + open;
        let close = open + css[open..].find('}').expect("a rule closes");
        let head = css[at..open].rsplit(['}', '/']).next().unwrap_or("").trim();
        if head.split(',').any(|one| one.trim() == selector) {
            return css[open + 1..close].to_owned();
        }
        at = close + 1;
    }
    panic!("no rule for {selector}")
}

#[test]
fn a_date_and_time_field_is_drawn_as_the_other_text_fields() {
    // Asserts: the rule that gives a text field its height, padding, border and colours also
    // names `input[type="datetime-local"]` (the window fields of the Schedule form).
    // Catches: the window fields drawn as the browser's bare control, a thin black box with no
    // padding, beside fields of another height and border.
    let css = stylesheet("app.css");
    let body = rule(&css, "input[type=\"datetime-local\"]");
    assert_eq!(body, rule(&css, "input[type=\"text\"]"));
    for declaration in [
        "height: 32px",
        "padding: 0 10px",
        "border: 1px solid var(--rule-strong)",
    ] {
        assert!(body.contains(declaration), "{declaration} in {body}");
    }
}

#[test]
fn a_host_page_table_scrolls_inside_its_box_on_a_narrow_screen() {
    // Asserts: the host page's tables scroll sideways inside their box (`overflow-x: auto`), as
    // every other page's do, and are not let out of it (`overflow: visible`).
    // Catches: the Updates tab's Due table, six columns wide, making a phone's page 42 px wider
    // than its screen.
    let body = rule(&stylesheet("styles/host.css"), ".hostpage .table-wrap");
    assert!(body.contains("overflow-x: auto"), "{body}");
    assert!(!body.contains("overflow: visible"), "{body}");
}
