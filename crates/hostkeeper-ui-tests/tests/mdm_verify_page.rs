//! The verification of the MDM setup page (`Mdm.Verify`), run in the JavaScript engine against the
//! fake gateway of `support`: the call each button sends, how the Macs to push to are chosen, and
//! how each answer of the listener, of Apple about the ABM token and of APNs about a test push is
//! worded.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{bodies, m, page_driver};

/// The time the kit's page context reads.
const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

const STATUS: &str = "Mdm/Status";
const SETTINGS: &str = "Mdm/ListSettings";
const DEVICES: &str = "Mdm/ListDevices";
const VERIFY: &str = "Mdm/Verify";

fn info(kind: &str) -> Value {
    json!({"kind": kind, "version": 2, "fingerprintSha256": "ab".repeat(32),
           "createdUnix": "1799000000"})
}

/// A `Mdm.Status` answer of a configured setup whose certificates and token expire in `days`.
fn held(days: i64) -> Value {
    json!({
        "configured": true, "sealing": true,
        "name": "mdm.example.com", "enrollmentUrl": "https://mdm.example.com/enroll",
        "pushCert": info("push_cert"), "pushKey": info("push_key"),
        "abmToken": info("abm_token"),
        "tlsCert": info("tls_cert"), "tlsKey": info("tls_key"),
        "pushTopic": "com.apple.mgmt.External.0a1b2c3d",
        "pushNotAfterUnix": (NOW + days * DAY).to_string(),
        "abmNotAfterUnix": (NOW + days * DAY).to_string(),
        "tlsNotAfterUnix": (NOW + days * DAY).to_string(),
        "tlsNames": ["mdm.example.com"],
    })
}

/// Scripts one drawing of the page: the status and the settings.
fn draw(ui: &Ui) {
    ui.reply(&m(STATUS), held(90));
    ui.reply(
        &m(SETTINGS),
        json!({"settings": [
            {"name": "push_alert_days", "value": "60,14", "isDefault": true},
            {"name": "abm_alert_days", "value": "60", "isDefault": true},
            {"name": "tls_renew_days", "value": "30", "isDefault": true},
        ]}),
    );
}

/// A `Mdm.Verify` answer for a setup that is held and served, with the ABM check `abm` and the
/// push check `push`, if any.
fn verified(abm: Value, push: Option<Value>) -> Value {
    let mut reply = json!({
        "status": held(90), "listener": true, "serving": true,
        "servedNames": ["mdm.example.com"], "servedNamesTheName": true, "abm": abm,
    });
    if let Some(push) = push {
        reply["push"] = push;
    }
    reply
}

fn accepted() -> Value {
    json!({"result": "MDM_ABM_RESULT_ACCEPTED", "organization": "Komira"})
}

/// The text of the verification's result in the page last drawn.
const RESULT: &str = r#"
const result = () => text(named(seen.shown[seen.shown.length - 1], "verify-result")[0]);"#;

#[test]
fn verify_without_a_mac_sends_an_empty_request_and_shows_what_each_part_found() {
    // Catches: a verification that asks for the Macs when the page is drawn, one that names a Mac
    // nobody chose, a result that leaves out the listener, the ABM check or the setup, a push
    // line shown for a push nobody made, and a report that is not the page's own words.
    let mut ui = Ui::start();
    draw(&ui);
    ui.reply(&m(VERIFY), verified(accepted(), None));
    draw(&ui);
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"{RESULT}
await click(tree, "verify");
return result();"#
        ),
    ));
    assert_eq!(
        ui.methods(),
        [m(STATUS), m(SETTINGS), m(VERIFY), m(STATUS), m(SETTINGS)],
        "no Mac is listed until the button is pressed"
    );
    assert_eq!(bodies(&ui)[2], (m(VERIFY), json!({})));
    let result = out["out"].as_str().expect("the result");
    for piece in [
        "Configured.",
        "The Macs' listener serves a certificate for mdm.example.com, which names the MDM name.",
        "Apple accepts the ABM token. Account: Komira.",
    ] {
        assert!(result.contains(piece), "{piece:?} is not in: {result}");
    }
    assert!(!result.contains("Test push"), "{result}");
    assert_eq!(
        out["seen"]["reports"],
        json!(["Verified. See what each part found."])
    );
}

#[test]
fn a_chosen_enrolled_mac_is_pushed_and_only_enrolled_macs_are_offered() {
    // Catches: a Mac that has not enrolled or has checked out offered for a push, a label without
    // the host id and group that tell Macs apart, the choice not sent as `enrollmentId`, a choice
    // kept after the list is loaded again, and a push line without the Mac and the APNs id.
    let mut ui = Ui::start();
    draw(&ui);
    ui.reply(
        &m(DEVICES),
        json!({"devices": [
            {"enrollmentId": "E1", "udid": "U1", "serial": "C02A", "phase": "MDM_PHASE_ENROLLED",
             "hostId": "lab-mac-1", "group": "mac-builders", "lastCheckinUnix": "1800000000"},
            {"enrollmentId": "E2", "udid": "U2", "serial": "C02B", "phase": "MDM_PHASE_AUTHENTICATED",
             "lastCheckinUnix": "1800000000"},
            {"enrollmentId": "E3", "udid": "U3", "serial": "C02C", "phase": "MDM_PHASE_CHECKED_OUT",
             "lastCheckinUnix": "1800000000"},
            {"enrollmentId": "E4", "udid": "U4", "serial": "C02D", "phase": "MDM_PHASE_ENROLLED",
             "lastCheckinUnix": "1800000000"},
        ]}),
    );
    draw(&ui);
    ui.reply(
        &m(VERIFY),
        verified(
            accepted(),
            Some(
                json!({"enrollmentId": "E1", "result": "MDM_PUSH_RESULT_SENT", "apnsId": "apns-1",
                        "topic": "com.apple.mgmt.External.0a1b2c3d"}),
            ),
        ),
    );
    draw(&ui);
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"{RESULT}
const options = (tree) => nodes(tree).filter((n) => n.tag === "option" && n.attrs["value"] !== undefined)
  .map((n) => [n.attrs["value"], text(n)]);
const before = options(tree);
await click(tree, "load-macs");
const loaded = seen.shown[seen.shown.length - 1];
choose(loaded, "verify-mac", "E1");
await click(loaded, "verify");
return {{ before, offered: options(loaded), result: result() }};"#
        ),
    ));
    assert_eq!(out["out"]["before"], json!([["", "None: do not push"]]));
    assert_eq!(
        out["out"]["offered"],
        json!([
            ["", "None: do not push"],
            ["E1", "lab-mac-1 (mac-builders, E1)"],
            ["E4", "C02D (E4)"],
        ])
    );
    let sent: Vec<(String, Value)> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m(VERIFY) || *method == m(DEVICES))
        .collect();
    assert_eq!(
        sent,
        [
            (m(DEVICES), json!({"pageToken": ""})),
            (m(VERIFY), json!({"enrollmentId": "E1"})),
        ]
    );
    let result = out["out"]["result"].as_str().expect("the result");
    assert!(
        result.contains("Test push to E1"),
        "the push line names the Mac: {result}"
    );
    assert!(result.contains("APNs took the push"), "{result}");
    assert!(result.contains("APNs id apns-1."), "{result}");
    assert_eq!(
        out["seen"]["reports"],
        json!(["2 enrolled Macs.", "Verified. See what each part found."])
    );
}

#[test]
fn each_failed_push_is_worded_apart_so_a_wrong_topic_a_dead_token_and_a_wrong_environment_differ() {
    // Catches: two classes of failure shown by one sentence, the certificate of the wrong
    // environment worded as a dead token, a detail dropped, and a class this page does not know
    // hidden instead of shown as its token.
    let classes = [
        (
            "BAD_DEVICE_TOKEN",
            "BadDeviceToken",
            "APNs refused the Mac's push token (BadDeviceToken).",
        ),
        (
            "UNREGISTERED",
            "",
            "APNs says the Mac's push token is no longer active.",
        ),
        (
            "TOPIC_DISALLOWED",
            "",
            "APNs refused the topic (TopicDisallowed)",
        ),
        (
            "BAD_CERTIFICATE_ENVIRONMENT",
            "BadCertificateEnvironment",
            "a development certificate cannot push to the production service.",
        ),
        (
            "CERTIFICATE_REJECTED",
            "CertificateExpired",
            "refused the push certificate. Store a valid push certificate. (CertificateExpired)",
        ),
        (
            "NO_CLIENT_CERTIFICATE",
            "",
            "APNs saw no client certificate.",
        ),
        ("THROTTLED", "", "too many pushes"),
        (
            "APNS_UNAVAILABLE",
            "503",
            "APNs cannot take a push now. Try again later. (503)",
        ),
        (
            "REJECTED",
            "400 PayloadEmpty",
            "APNs refused the push. (400 PayloadEmpty)",
        ),
        (
            "CONNECTION_FAILED",
            "timed out",
            "The connection to APNs failed, or did not answer in time. (timed out)",
        ),
        (
            "NO_PUSH_IDENTITY",
            "",
            "No push certificate with a matching key is held",
        ),
        ("NO_LISTENER", "", "The coordinator runs no MDM listener"),
        ("NO_SUCH_ENROLLMENT", "", "No live enrollment has that id"),
        ("NO_PUSH_TOKEN", "", "The Mac has sent no push token yet"),
    ];
    let mut ui = Ui::start();
    draw(&ui);
    for (class, detail, _) in classes {
        let mut push = json!({"enrollmentId": "E1", "result": format!("MDM_PUSH_RESULT_{class}")});
        if !detail.is_empty() {
            push["detail"] = json!(detail);
        }
        ui.reply(&m(VERIFY), verified(accepted(), Some(push)));
        draw(&ui);
    }
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"{RESULT}
const found = [];
for (let i = 0; i < {count}; i += 1) {{
  await click(tree, "verify");
  found.push(result());
}}
return found;"#,
            count = classes.len()
        ),
    ));
    let found: Vec<String> = out["out"]
        .as_array()
        .expect("a result each")
        .iter()
        .map(|v| v.as_str().expect("text").to_owned())
        .collect();
    for ((class, _, needle), result) in classes.iter().zip(&found) {
        assert!(
            result.contains(needle),
            "{class}: {needle:?} is not in: {result}"
        );
    }
    let mut distinct: Vec<&String> = found.iter().collect();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        classes.len(),
        "every class reads differently"
    );
}

#[test]
fn the_abm_check_and_the_listener_are_worded_for_each_answer_and_a_refusal_is_exact() {
    // Catches: a rejected token worded as accepted, `not checked` worded as accepted, a listener
    // that serves nothing or is not running worded as healthy, an unreadable token worded as
    // missing, and a refusal reworded or followed by a redrawn page.
    let abm = |result: &str, detail: &str| {
        let mut check = json!({"result": format!("MDM_ABM_RESULT_{result}")});
        if !detail.is_empty() {
            check["detail"] = json!(detail);
        }
        check
    };
    let answers = [
        verified(abm("NO_TOKEN", ""), None),
        verified(abm("NOT_CHECKED", ""), None),
        verified(abm("REJECTED", "401 from the session"), None),
        verified(abm("UNREACHABLE", "timed out"), None),
        verified(abm("UNREADABLE", ""), None),
        {
            let mut not_serving = verified(abm("ACCEPTED", ""), None);
            not_serving["serving"] = json!(false);
            not_serving["servedNames"] = json!([]);
            not_serving["servedNamesTheName"] = json!(false);
            not_serving
        },
        {
            let mut not_running = verified(abm("ACCEPTED", ""), None);
            not_running["listener"] = json!(false);
            not_running["serving"] = json!(false);
            not_running
        },
        {
            let mut other = verified(abm("ACCEPTED", ""), None);
            other["servedNames"] = json!(["other.example.com"]);
            other["servedNamesTheName"] = json!(false);
            other["status"]["configured"] = json!(false);
            other["status"]["missing"] = json!(["MDM_CONDITION_TLS_NAME_MISMATCH"]);
            other
        },
    ];
    let wanted = [
        "No ABM token is held: nothing to check.",
        "An ABM token is held and this coordinator cannot ask Apple about it.",
        "Apple refused the ABM token. (401 from the session)",
        "Apple could not be reached, or its answer says nothing of the token. (timed out)",
        "The ABM token held cannot be read, so Apple was not asked.",
        "The Macs' listener runs and has no TLS certificate to serve: no Mac can connect.",
        "The Macs' listener is not running (--mdm-listen is not set).",
        "serves a certificate for other.example.com, which does not name the MDM name.",
    ];
    let mut ui = Ui::start();
    draw(&ui);
    for answer in &answers {
        ui.reply(&m(VERIFY), answer.clone());
        draw(&ui);
    }
    ui.refuse(&m(VERIFY), 503, "UNAVAILABLE", "audit_unavailable");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"{RESULT}
const found = [];
for (let i = 0; i < {count}; i += 1) {{
  await click(tree, "verify");
  found.push(result());
}}
const drawn = seen.shown.length;
await click(tree, "verify");
return {{ found, redrawn: seen.shown.length - drawn }};"#,
            count = answers.len()
        ),
    ));
    let found = out["out"]["found"].as_array().expect("a result each");
    for (result, want) in found.iter().zip(wanted) {
        let result = result.as_str().expect("text");
        assert!(result.contains(want), "{want:?} is not in: {result}");
    }
    assert!(
        found[7]
            .as_str()
            .expect("text")
            .contains("Not configured: The TLS certificate does not name the MDM name."),
        "{:?}",
        found[7]
    );
    assert_eq!(out["out"]["redrawn"], 0, "a refusal draws nothing new");
    assert_eq!(
        out["seen"]["reports"].as_array().expect("reports").last(),
        Some(&json!("UNAVAILABLE audit_unavailable"))
    );
}
