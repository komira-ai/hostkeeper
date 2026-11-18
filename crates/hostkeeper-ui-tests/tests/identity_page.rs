//! The "Service identity" section of the Host page and its "Approve identity" button, run in the
//! JavaScript engine against the fake gateway of `support`: what the section shows for a host an
//! operator approved, one the coordinator approved itself (approved or not yet), and a retired
//! one; the one call the button sends and the question it asks first; and a refused call, said
//! with its token while the rest of the page renders.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

const KEY: &str = "abababababababababababababababababababababababababababababababab";

/// What `Hosts.Get` answers for `web-2`: one reported service identity and one with an error.
fn host_reply() -> Value {
    json!({"host": {"nodeId": "web-2", "role": "web", "heartbeat": {"serviceIdentities": [
        {"service": "api", "notAfterUnix": "1800028800"},
        {"service": "db", "lastError": "certificate request refused"},
    ]}}})
}

/// What `HostIdentity.Get` answers; `extra` holds the fields that differ.
fn identity(assurance: &str, extra: Value) -> Value {
    let mut reply = json!({"nodeId": "web-2", "assurance": assurance, "keySha256": KEY});
    for (name, value) in extra.as_object().expect("an object") {
        reply[name] = value.clone();
    }
    reply
}

/// The page of `web-2` with `Get` answered as given, then `then` run with `answers` to the confirms.
fn host(get: Value, answers: &str, then: &str) -> (Ui, Value) {
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), host_reply());
    ui.reply(&m("HostIdentity/Get"), get);
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        answers,
        then,
    ));
    (ui, out)
}

/// Counts the Approve buttons (`named` throws when there is none).
const BUTTONS: &str =
    r#"try { return named(tree, "approve-identity").length; } catch (error) { return 0; }"#;

#[test]
fn an_operator_approved_host_shows_its_assurance_and_key_and_offers_no_approval() {
    // Catches: an Approve button on a host that needs none, the key hash or the assurance left
    // out, and the reported service identities (service, end, last error) left out.
    let get = identity("HOST_ASSURANCE_OPERATOR", json!({"allowed": true}));
    let (ui, out) = host(get, "[]", BUTTONS);
    assert_eq!(out["out"], 0);
    assert_shows(
        &out,
        &[
            "Service identity",
            "operator",
            KEY,
            "Certificates signed for its services",
            "yes",
            "api",
            "2027-01-15 16:00:00 UTC",
            "certificate request refused",
        ],
    );
    assert!(bodies(&ui).contains(&(m("HostIdentity/Get"), json!({"nodeId": "web-2"}))));
}

#[test]
fn an_auto_host_not_approved_offers_approval_that_names_the_key_get_answered() {
    // Catches: an approval sent without the operator's yes, a key hash other than the one Get
    // answered, a missing node id, a field the proto does not read, and no refresh or report
    // after the approval.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), host_reply());
    ui.reply(
        &m("HostIdentity/Get"),
        identity("HOST_ASSURANCE_AUTO", json!({})),
    );
    ui.reply(
        &m("HostIdentity/Approve"),
        json!({"nodeId": "web-2", "approval": {"approver": "ops", "approvedUnix": "1800000000"}}),
    );
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[true]",
        r#"const before = named(tree, "approve-identity").length;
await click(tree, "approve-identity");
return before;"#,
    ));
    assert_eq!(out["out"], 1);
    assert_shows(&out, &["Approve identity", "auto", KEY]);
    assert_eq!(
        bodies(&ui).last(),
        Some(&(
            m("HostIdentity/Approve"),
            json!({"nodeId": "web-2", "keySha256": KEY})
        ))
    );
    let seen = &out["seen"];
    assert_eq!(
        seen["questions"],
        json!([format!(
            "Approve the identity of web-2? The coordinator will sign certificates for its services \
             that ask for an identity, for its current key {KEY}."
        )])
    );
    assert_eq!(
        seen["reports"],
        json!([
            "Approved the identity of web-2: the coordinator signs certificates for its services."
        ])
    );
    assert_eq!(seen["refreshes"], 1);
}

#[test]
fn an_auto_host_with_its_key_approved_shows_the_approval_and_offers_none() {
    // Catches: a button on a host whose current key an operator approved already, and an
    // approval shown without its approver or its time.
    let get = identity(
        "HOST_ASSURANCE_AUTO",
        json!({"allowed": true, "approval": {"approver": "ops", "approvedUnix": "1800000000"}}),
    );
    let (_, out) = host(get, "[]", BUTTONS);
    assert_eq!(out["out"], 0);
    assert_shows(&out, &["auto", "ops at 2027-01-15 08:00:00 UTC"]);
}

#[test]
fn a_denied_host_shows_it_is_denied_and_offers_no_approval() {
    // Catches: an Approve button on a retired host, which the coordinator refuses as `denied`.
    let get = identity("HOST_ASSURANCE_AUTO", json!({"denied": true}));
    let (_, out) = host(get, "[]", BUTTONS);
    assert_eq!(out["out"], 0);
    assert_shows(&out, &["an operator retired the host"]);
}

#[test]
fn declining_the_question_sends_no_approval() {
    // Catches: an approval sent after the operator said no, or one reported as done.
    let get = identity("HOST_ASSURANCE_AUTO", json!({}));
    let (ui, out) = host(get, "[false]", r#"await click(tree, "approve-identity");"#);
    assert!(
        !ui.methods().contains(&m("HostIdentity/Approve")),
        "{:?}",
        ui.methods()
    );
    assert_eq!(out["seen"]["questions"].as_array().map(Vec::len), Some(1));
    assert_eq!(out["seen"]["reports"], json!(["Identity not approved."]));
    assert_eq!(out["seen"]["refreshes"], 0);
}

#[test]
fn a_refused_approval_is_said_with_its_token_and_refreshes_nothing() {
    // Catches: a refusal taken as success, its token left out, and a refresh after it.
    let mut ui = Ui::start();
    ui.reply(&m("Hosts/Get"), host_reply());
    ui.reply(
        &m("HostIdentity/Get"),
        identity("HOST_ASSURANCE_AUTO", json!({})),
    );
    ui.refuse(
        &m("HostIdentity/Approve"),
        412,
        "FAILED_PRECONDITION",
        "key_mismatch",
    );
    let out = ui.run(&page_driver(
        "host",
        r#"{ nodeId: "web-2" }"#,
        "[true]",
        r#"await click(tree, "approve-identity");"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!(["FAILED_PRECONDITION key_mismatch"])
    );
    assert_eq!(out["seen"]["refreshes"], 0);
}

#[test]
fn a_refused_get_is_said_in_the_section_and_the_rest_of_the_page_renders() {
    // Catches: a Host page that fails whole when HostIdentity.Get is refused (a host the
    // coordinator has no enrollment of, a coordinator that predates the service), a button
    // offered without a key to name, and a refusal shown without its code or token.
    for (status, code, reason, said) in [
        (404, "NOT_FOUND", "unknown_host", "NOT_FOUND unknown_host"),
        (501, "UNIMPLEMENTED", "", "UNIMPLEMENTED"),
    ] {
        let mut ui = Ui::start();
        ui.reply(&m("Hosts/Get"), host_reply());
        ui.refuse(&m("HostIdentity/Get"), status, code, reason);
        let out = ui.run(&page_driver(
            "host",
            r#"{ nodeId: "web-2" }"#,
            "[]",
            BUTTONS,
        ));
        assert_eq!(out["out"], 0, "{code}");
        assert_shows(
            &out,
            &[
                &format!("The service identity cannot be read: {said}"),
                "Certificate",
                "web-2",
                "certificate request refused",
            ],
        );
    }
}
