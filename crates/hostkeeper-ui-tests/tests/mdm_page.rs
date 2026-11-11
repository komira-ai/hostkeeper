//! The MDM setup page, run in the JavaScript engine against the fake gateway of `support`: the
//! status panel and its reminders, and the push certificate step (create the request, check the
//! emailed reply, store the certificate, upload an existing certificate and key): the call each
//! button sends, what the page shows and offers as a download, and that no secret is shown.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

/// The time the kit's page context reads.
const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

// The bytes the tests upload, as the API's canonical JSON writes them (base64).
const HEX_TEXT_B64: &str = "MDBmZg=="; // the text `00ff`
const RAW_REPLY_B64: &str = "MID/"; // the bytes 30 80 ff
const CERT_TEXT: &str = "-----BEGIN CERTIFICATE-----\\nAA\\n-----END CERTIFICATE-----\\n";
const CERT_B64: &str =
    "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCkFBCi0tLS0tRU5EIENFUlRJRklDQVRFLS0tLS0K";
const KEY_TEXT: &str =
    "-----BEGIN RSA PRIVATE KEY-----\\nSECRETKEYBODY\\n-----END RSA PRIVATE KEY-----\\n";
const KEY_B64: &str = "LS0tLS1CRUdJTiBSU0EgUFJJVkFURSBLRVktLS0tLQpTRUNSRVRLRVlCT0RZCi0tLS0tRU5EIFJTQSBQUklWQVRFIEtFWS0tLS0tCg==";

const STATUS: &str = "Mdm/Status";
const SETTINGS: &str = "Mdm/ListSettings";
const CREATE: &str = "Mdm/CreatePushRequest";
const REPLY: &str = "Mdm/PutPushReply";
const CERTIFICATE: &str = "Mdm/PutPushCertificate";
const PUT_SECRET: &str = "Mdm/PutSecret";
const PUT_SETTING: &str = "Mdm/PutSetting";

fn info(kind: &str) -> Value {
    json!({"kind": kind, "version": 2, "fingerprintSha256": "ab".repeat(32),
           "createdUnix": "1799000000"})
}

/// A `Mdm.Status` answer of a setup with a push pair that expires in `push_days`, an ABM token in
/// `abm_days` and a TLS pair in `tls_days`.
fn held(push_days: i64, abm_days: i64, tls_days: i64) -> Value {
    json!({
        "configured": true, "sealing": true,
        "name": "mdm.example.com", "enrollmentUrl": "https://mdm.example.com/enroll",
        "pushCert": info("push_cert"), "pushKey": info("push_key"),
        "abmToken": info("abm_token"),
        "tlsCert": info("tls_cert"), "tlsKey": info("tls_key"),
        "pushTopic": "com.apple.mgmt.External.0a1b2c3d",
        "pushNotAfterUnix": (NOW + push_days * DAY).to_string(),
        "abmNotAfterUnix": (NOW + abm_days * DAY).to_string(),
        "tlsNotAfterUnix": (NOW + tls_days * DAY).to_string(),
        "tlsNames": ["mdm.example.com"],
    })
}

fn settings() -> Value {
    json!({"settings": [
        {"name": "push_alert_days", "value": "60,14", "isDefault": true},
        {"name": "abm_alert_days", "value": "60", "isDefault": true},
        {"name": "tls_renew_days", "value": "30", "isDefault": true},
    ]})
}

/// Scripts one drawing of the page: the status and the settings.
fn draw(ui: &Ui, status: Value) {
    ui.reply(&m(STATUS), status);
    ui.reply(&m(SETTINGS), settings());
}

/// A page driver that finds the table row labelled `label`: `[class, cells]`.
const ROW: &str = r#"
const row = (label) => {
  const found = nodes(tree).find((n) => n.tag === "tr" && n.children[0] && text(n.children[0]) === label);
  return [found.attrs.class, found.children.map((c) => text(c))];
};"#;

#[test]
fn the_status_panel_names_what_is_missing_and_colours_the_days_left() {
    // Catches: a page that calls another method than Status and ListSettings, a missing
    // condition left out or shown as its token, a held secret shown without its version and
    // fingerprint, and a reminder that does not follow the settings: the push certificate at 60
    // and 14 days, the ABM token at 60, the TLS certificate at 30, each at the day itself.
    let mut ui = Ui::start();
    let mut status = held(10, 45, 31);
    status["configured"] = json!(false);
    status["missing"] = json!([
        "MDM_CONDITION_ABM_TOKEN_UNREADABLE",
        "MDM_CONDITION_TLS_NAME_MISMATCH"
    ]);
    status.as_object_mut().expect("an object").remove("pushKey");
    draw(&ui, status);
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"{ROW}
return {{ push: row("Push certificate"), key: row("Push key"), abm: row("ABM token"),
          tls: row("TLS certificate"), names: row("TLS names"), topic: row("Push topic"),
          url: row("Enrollment URL") }};"#
        ),
    ));
    assert_eq!(ui.methods(), [m(STATUS), m(SETTINGS)]);
    assert_eq!(
        bodies(&ui),
        [(m(STATUS), json!({})), (m(SETTINGS), json!({}))]
    );
    assert_shows(
        &out,
        &[
            "Not configured, because:",
            "The ABM token cannot be read, or its expiry is not a time.",
            "The TLS certificate does not name the MDM name.",
            "Push certificate expires soon: 10 days left.",
            "ABM token expires soon: 45 days left.",
        ],
    );
    assert!(
        !shown(&out).contains("TLS certificate expires"),
        "{}",
        shown(&out)
    );
    let fingerprint = &"ab".repeat(8);
    assert_eq!(
        out["out"]["push"],
        json!([
            "warning",
            [
                "Push certificate",
                format!("version 2, {fingerprint}"),
                "2027-01-25 08:00:00 UTC",
                "10 days left"
            ]
        ])
    );
    assert_eq!(
        out["out"]["key"],
        json!(["", ["Push key", "not held", "-", "-"]])
    );
    assert_eq!(out["out"]["abm"][0], "warning");
    assert_eq!(out["out"]["tls"][0], "");
    assert_eq!(out["out"]["tls"][1][3], "31 days left");
    assert_eq!(
        out["out"]["names"][1],
        json!(["TLS names", "mdm.example.com"])
    );
    assert_eq!(
        out["out"]["topic"][1],
        json!(["Push topic", "com.apple.mgmt.External.0a1b2c3d"])
    );
    assert_eq!(
        out["out"]["url"][1],
        json!(["Enrollment URL", "https://mdm.example.com/enroll"])
    );
}

#[test]
fn a_configured_setup_says_so_and_an_expired_certificate_is_named_expired() {
    // Catches: a configured setup shown with a reason it is not, an expired certificate shown
    // as days left, a coordinator with no master key shown without its warning, and a setup
    // that keeps secrets shown with it.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    let mut expired = held(-1, 90, 90);
    expired["sealing"] = json!(false);
    expired["missing"] = json!(["MDM_CONDITION_PUSH_CERT_EXPIRED"]);
    expired["configured"] = json!(false);
    draw(&ui, expired);
    let configured = ui.run(&page_driver("mdm", "{}", "[]", ""));
    let broken = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!("{ROW} return row(\"Push certificate\");"),
    ));
    assert_shows(&configured, &["Configured."]);
    assert!(
        !shown(&configured).contains("Not configured"),
        "{}",
        shown(&configured)
    );
    assert!(
        !shown(&configured).contains("master key"),
        "{}",
        shown(&configured)
    );
    assert_eq!(configured["out"], Value::Null);
    assert_shows(
        &broken,
        &[
            "Not configured, because:",
            "The push certificate has expired.",
            "Push certificate has expired.",
            "The coordinator holds no master key (--mdm-secrets-key-file), so it keeps no secret: an upload is refused.",
        ],
    );
    assert_eq!(broken["out"][0], "warning");
    assert_eq!(broken["out"][1][3], "expired");
}

#[test]
fn a_refused_status_is_shown_as_the_api_says_it_and_unread_settings_leave_expiries_coloured() {
    // Catches: a refusal shown in other words or with the rest of the page drawn beside it, and
    // a failed ListSettings that fails the page or colours a certificate by a guessed threshold.
    let mut ui = Ui::start();
    ui.refuse(&m(STATUS), 503, "UNAVAILABLE", "store_failed");
    let refused = ui.run(&page_driver("mdm", "{}", "[]", ""));
    assert_eq!(shown(&refused), "MDM setup UNAVAILABLE store_failed");

    ui.reply(&m(STATUS), held(10, 10, 10));
    ui.refuse(&m(SETTINGS), 503, "UNAVAILABLE", "store_failed");
    let unread = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!("{ROW} return row(\"Push certificate\");"),
    ));
    assert_shows(
        &unread,
        &["The thresholds of the reminders could not be read; only an expiry is coloured."],
    );
    assert_eq!(unread["out"][0], "", "no threshold, no colour");
    assert_eq!(unread["out"][1][3], "10 days left");
    assert!(
        !shown(&unread).contains("expires soon"),
        "{}",
        shown(&unread)
    );
}

#[test]
fn reminders_cross_their_thresholds_at_the_day_and_words_stay_exact() {
    // Catches: an off-by-one at a threshold (60 days left is soon, 61 is not; 14 is near, 15 is
    // soon), the second left before an expiry shown as a day, a single threshold giving the
    // urgent level, a setting that is not a list of days read as one, and a condition this page
    // does not know shown as nothing.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { conditionWords, expiry, thresholdDays } from "./pages/mdm.js";
export async function main() {
  const now = 1800000000;
  const day = 86400;
  const at = (days, list) => expiry(String(now + days * day), now, list).level;
  const two = [60, 14];
  return {
    two: [61, 60, 15, 14, 1, 0].map((d) => at(d, two)),
    one: [31, 30, 1].map((d) => at(d, [30])),
    none: [at(1, null), at(1, [])],
    words: [
      expiry(String(now + 86399), now, two),
      expiry(String(now + day), now, two),
      expiry(String(now + 2 * day + 5), now, two),
      expiry(String(now), now, two),
      expiry(String(now - 1), now, two),
    ],
    absent: [expiry(undefined, now, two), expiry("", now, two), expiry("0", now, two), expiry("x", now, two)],
    days: ["60,14", "14,60", " 30 ", "", "60,", "a", "-1", undefined].map(thresholdDays),
    conditions: [
      conditionWords("MDM_CONDITION_NAME_MISSING"),
      conditionWords("MDM_CONDITION_FUTURE_THING"),
    ],
  };
}"#,
    );
    assert_eq!(
        out["two"],
        json!(["ok", "soon", "soon", "near", "near", "expired"])
    );
    assert_eq!(out["one"], json!(["ok", "soon", "soon"]));
    assert_eq!(out["none"], json!(["ok", "ok"]));
    assert_eq!(
        out["words"],
        json!([
            {"level": "near", "words": "less than a day left"},
            {"level": "near", "words": "1 day left"},
            {"level": "near", "words": "2 days left"},
            {"level": "expired", "words": "expired"},
            {"level": "expired", "words": "expired"},
        ])
    );
    assert_eq!(
        out["absent"],
        json!(vec![json!({"level": "none", "words": "-"}); 4])
    );
    assert_eq!(
        out["days"],
        json!([[60, 14], [60, 14], [30], null, null, null, null, null])
    );
    assert_eq!(
        out["conditions"],
        json!(["No MDM name is set.", "future_thing"])
    );
}

#[test]
fn create_request_sends_the_address_and_shows_the_public_request_with_downloads() {
    // Catches: a request sent without the address or with `submit` dropped, the CSR not shown
    // or not offered as a file, the encryption certificate not offered, the instructions that
    // the reply comes by email missing, and a second drawing that is not of the same page.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.reply(
        &m(CREATE),
        json!({"csrPem": "-----BEGIN CERTIFICATE REQUEST-----\nCSR\n-----END CERTIFICATE REQUEST-----\n",
               "encryptionCertificatePem": "-----BEGIN CERTIFICATE-----\nENC\n-----END CERTIFICATE-----\n",
               "submitted": true}),
    );
    draw(&ui, held(90, 90, 90));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
type(tree, "push-email", " ops@example.org ");
await click(tree, "create-request");
const after = seen.shown[seen.shown.length - 1];
await click(after, "download-csr");
await click(after, "download-encryption-certificate");
return text(after);
"#,
    ));
    assert_eq!(
        bodies(&ui)[2],
        (
            m(CREATE),
            json!({"email": "ops@example.org", "submit": true})
        )
    );
    let after = out["out"].as_str().expect("the second drawing's text");
    for piece in [
        "The signed request comes by email from mdmcert.download to ops@example.org, as an attachment. Upload that attachment under Check the emailed reply.",
        "The CSR and the encryption certificate are public.",
        "-----BEGIN CERTIFICATE REQUEST-----\nCSR\n-----END CERTIFICATE REQUEST-----\n",
    ] {
        assert!(after.contains(piece), "{piece:?} is not shown in: {after}");
    }
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Request made and sent to mdmcert.download: it emails the signed request to ops@example.org."
        ])
    );
    assert_eq!(
        out["seen"]["downloads"],
        json!([
            {"name": "push-request.csr", "mime": "text/plain",
             "text": "-----BEGIN CERTIFICATE REQUEST-----\nCSR\n-----END CERTIFICATE REQUEST-----\n"},
            {"name": "push-encryption.crt", "mime": "text/plain",
             "text": "-----BEGIN CERTIFICATE-----\nENC\n-----END CERTIFICATE-----\n"},
        ])
    );
}

#[test]
fn create_request_without_sending_says_nothing_was_sent_and_a_refusal_is_exact() {
    // Catches: `submit: false` sent as true, a request not sent described as emailed, an empty
    // address sent to the coordinator, and a refusal reworded or followed by a redrawn page.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.reply(
        &m(CREATE),
        json!({"csrPem": "CSRPEM", "encryptionCertificatePem": "ENCPEM"}),
    );
    draw(&ui, held(90, 90, 90));
    ui.refuse(&m(CREATE), 503, "UNAVAILABLE", "mdmcert_unavailable");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
await click(tree, "create-request");
type(tree, "push-email", "ops@example.org");
tick(tree, "push-submit", false);
await click(tree, "create-request");
const after = seen.shown[seen.shown.length - 1];
tick(after, "push-submit", true);
await click(after, "create-request");
return text(after);
"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give the email address the request is made for.",
            "Request made. Nothing was sent.",
            "UNAVAILABLE mdmcert_unavailable",
        ])
    );
    let sent: Vec<_> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m(CREATE))
        .collect();
    assert_eq!(
        sent,
        [
            (
                m(CREATE),
                json!({"email": "ops@example.org", "submit": false})
            ),
            (
                m(CREATE),
                json!({"email": "ops@example.org", "submit": true})
            ),
        ]
    );
    assert_eq!(
        out["seen"]["shown"].as_array().map(Vec::len),
        Some(1),
        "a refusal redraws nothing"
    );
    let after = out["out"].as_str().expect("text");
    assert!(
        after.contains("The request was not sent. Take the CSR to the signer you use."),
        "{after}"
    );
}

/// A file as the browser hands one over: its name, size and bytes.
fn file(name: &str, bytes: &str) -> String {
    format!(
        r#"{{ name: "{name}", size: {bytes}.length, arrayBuffer: async () => Uint8Array.from({bytes}).buffer }}"#
    )
}

#[test]
fn a_reply_pasted_or_chosen_is_sent_as_bytes_and_the_signed_request_is_offered() {
    // Catches: pasted hex sent as anything but its text bytes, a chosen file's bytes altered, the
    // pasted text winning over a chosen file, the signed request not offered or offered as
    // text, the instructions for Apple's portal missing, and the inputs kept after the upload.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.reply(&m(REPLY), json!({"signedRequest": "PHBsaXN0Lz4="}));
    draw(&ui, held(90, 90, 90));
    ui.reply(&m(REPLY), json!({"signedRequest": "PHBsaXN0Lz4="}));
    draw(&ui, held(90, 90, 90));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
type(tree, "reply-text", "00ff");
await click(tree, "check-reply");
const first = seen.shown[seen.shown.length - 1];
type(first, "reply-text", "ignored text");
named(first, "reply-file")[0].on.change({{ target: {{ files: [{file}] }} }});
await click(first, "check-reply");
const second = seen.shown[seen.shown.length - 1];
await click(second, "download-signed");
return {{ first: text(first), second: text(second), chosen: text(named(second, "reply-file-chosen")[0]) }};
"#,
            file = file("reply.bin", "[0x30, 0x80, 0xff]")
        ),
    ));
    assert_eq!(
        bodies(&ui)
            .into_iter()
            .filter(|(method, _)| *method == m(REPLY))
            .collect::<Vec<_>>(),
        [
            (m(REPLY), json!({"reply": HEX_TEXT_B64})),
            (m(REPLY), json!({"reply": RAW_REPLY_B64})),
        ]
    );
    assert_eq!(
        out["seen"]["downloads"],
        json!([{"name": "push-signed-request.plist", "mime": "application/x-plist",
                "base64": "PHBsaXN0Lz4="}])
    );
    let first = out["out"]["first"].as_str().expect("text");
    assert!(
        first.contains("Upload the signed request at identity.apple.com/pushcert, sign in with the Apple ID that owns the certificate, and download the certificate Apple returns. Store it under Store the certificate Apple returned."),
        "{first}"
    );

    assert_eq!(
        out["out"]["chosen"], "",
        "the choice is dropped after the upload"
    );
    assert_eq!(
        out["seen"]["reports"],
        json!(vec![
            "The reply is the signed request for the pending CSR. Download it and upload it to Apple's push portal.";
            2
        ])
    );
}

#[test]
fn a_reply_that_is_missing_too_big_or_refused_is_reported_exactly() {
    // Catches: an empty upload sent, a file over the API's 32768 bytes read and sent, a file
    // whose size is not known but whose bytes are too many sent, the text over the limit sent,
    // and a refusal reworded.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.refuse(&m(REPLY), 400, "INVALID_ARGUMENT", "reply_not_for_request");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
const choose_file = (file) => named(tree, "reply-file")[0].on.change({ target: { files: [file] } });
await click(tree, "check-reply");
type(tree, "reply-text", "   ");
await click(tree, "check-reply");
type(tree, "reply-text", "a".repeat(32769));
await click(tree, "check-reply");
type(tree, "reply-text", "");
let read = false;
choose_file({ name: "big", size: 32769, arrayBuffer: async () => { read = true; return new ArrayBuffer(0); } });
await click(tree, "check-reply");
choose_file({ name: "liar", arrayBuffer: async () => new ArrayBuffer(32769) });
await click(tree, "check-reply");
choose_file({ name: "ok", size: 2, arrayBuffer: async () => Uint8Array.from([1, 2]).buffer });
await click(tree, "check-reply");
choose_file(null);
named(tree, "reply-file")[0].on.change({ target: { files: [] } });
await click(tree, "check-reply");
return read;
"#,
    ));
    assert_eq!(out["out"], false, "a file over the limit is not read");
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the reply or paste its text.",
            "Choose the reply or paste its text.",
            "The reply is at most 32768 bytes; this text is 32769.",
            "The reply is at most 32768 bytes; this file is 32769.",
            "The reply is at most 32768 bytes; this file is 32769.",
            "INVALID_ARGUMENT reply_not_for_request",
            "Choose the reply or paste its text.",
        ])
    );
    assert_eq!(
        bodies(&ui).len(),
        3,
        "only the one good upload was sent: {:?}",
        bodies(&ui)
    );
}

#[test]
fn a_certificate_is_stored_and_the_page_says_what_was_stored() {
    // Catches: a certificate sent with a key it was not given, pasted text and chosen file mixed
    // up, the topic or expiry missing from the answer, a renewal not said, and a refusal
    // (a key that does not match) reworded.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.reply(
        &m(CERTIFICATE),
        json!({"topic": "com.apple.mgmt.External.0a1b2c3d", "notBeforeUnix": "1799000000",
               "notAfterUnix": "1831000000", "secret": info("push_cert"), "renewal": false}),
    );
    draw(&ui, held(90, 90, 90));
    ui.reply(
        &m(CERTIFICATE),
        json!({"topic": "com.apple.mgmt.External.0a1b2c3d", "notBeforeUnix": "1799000000",
               "notAfterUnix": "1831000000", "secret": info("push_cert"), "renewal": true}),
    );
    draw(&ui, held(90, 90, 90));
    ui.refuse(&m(CERTIFICATE), 400, "INVALID_ARGUMENT", "key_mismatch");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
await click(tree, "put-certificate");
type(tree, "certificate-text", "{CERT_TEXT}");
await click(tree, "put-certificate");
const second = seen.shown[seen.shown.length - 1];
named(second, "certificate-file")[0].on.change({{ target: {{ files: [{file}] }} }});
await click(second, "put-certificate");
const third = seen.shown[seen.shown.length - 1];
type(third, "certificate-text", "{CERT_TEXT}");
await click(third, "put-certificate");
"#,
            file = file("apple.pem", "[0x63, 0x65, 0x72, 0x74, 0x2d, 0x66, 0x72, 0x6f, 0x6d, 0x2d, 0x66, 0x69, 0x6c, 0x65]")
        ),
    ));
    let sent: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m(CERTIFICATE))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        sent,
        [
            json!({"certificatePem": CERT_B64}),
            json!({"certificatePem": "Y2VydC1mcm9tLWZpbGU="}),
            json!({"certificatePem": CERT_B64}),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the certificate or paste its text.",
            "Stored the push certificate for com.apple.mgmt.External.0a1b2c3d, valid until 2028-01-09 03:06:40 UTC.",
            "Stored the push certificate for com.apple.mgmt.External.0a1b2c3d, valid until 2028-01-09 03:06:40 UTC (it replaces the one in use).",
            "INVALID_ARGUMENT key_mismatch",
        ])
    );
}

#[test]
fn an_existing_certificate_and_its_pkcs1_key_are_sent_together_and_the_key_is_never_shown() {
    // Catches: the key sent without its certificate or the certificate without the key, a key
    // typed into the page or kept in it after the call, any part of the key (as text or base64)
    // in the page, a report or a download, a chosen key lost by a refusal (the retry needs no new
    // choice), and a missing key file sent as an empty key.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.refuse(&m(CERTIFICATE), 400, "INVALID_ARGUMENT", "key_mismatch");
    ui.reply(
        &m(CERTIFICATE),
        json!({"topic": "com.apple.mgmt.External.0a1b2c3d", "notBeforeUnix": "1799000000",
               "notAfterUnix": "1831000000", "secret": info("push_cert"), "renewal": false}),
    );
    draw(&ui, held(90, 90, 90));
    let key_bytes: Vec<String> = format!("{}\n", KEY_TEXT.replace("\\n", "\n").trim_end())
        .bytes()
        .map(|b| b.to_string())
        .collect();
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
type(tree, "existing-certificate-text", "{CERT_TEXT}");
await click(tree, "put-existing");
named(tree, "existing-key-file")[0].on.change({{ target: {{ files: [{key}] }} }});
await click(tree, "put-existing");
const redrawn = seen.shown.length;
await click(tree, "put-existing");
const after = seen.shown[seen.shown.length - 1];
return {{ redrawn, chosen: text(named(after, "existing-key-file-chosen")[0]),
         keyFields: nodes(after).filter((n) => n.tag === "textarea" && /key/.test(n.attrs["name"])).length,
         everything: JSON.stringify([shown, seen]) + text(after) }};
"#,
            key = file("key.pem", &format!("[{}]", key_bytes.join(", ")))
        ),
    ));
    let sent: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m(CERTIFICATE))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        sent,
        [
            json!({"certificatePem": CERT_B64, "keyPem": KEY_B64}),
            json!({"certificatePem": CERT_B64, "keyPem": KEY_B64}),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the key file.",
            "INVALID_ARGUMENT key_mismatch",
            "Stored the push certificate for com.apple.mgmt.External.0a1b2c3d, valid until 2028-01-09 03:06:40 UTC.",
        ])
    );
    assert_eq!(out["out"]["redrawn"], 0, "a refusal draws nothing new");
    assert_eq!(
        out["out"]["chosen"], "",
        "the key file is dropped once stored"
    );
    assert_eq!(
        out["out"]["keyFields"], 0,
        "there is no field to type a key into"
    );
    let everything = out["out"]["everything"].as_str().expect("text");
    for secret in ["SECRETKEYBODY", KEY_B64, "-----BEGIN RSA PRIVATE KEY-----"] {
        assert!(
            !everything.contains(secret),
            "{secret} is in the page's output"
        );
    }
}

#[test]
fn bytes_are_utf8_and_base64_as_the_api_reads_them() {
    // Catches: UTF-8 of one, two, three and four byte characters and of a lone surrogate (which
    // is U+FFFD, as a text encoder writes it), and base64 of 0, 1, 2 and 3 bytes with its padding.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { toBase64, utf8 } from "./bytes.js";
export async function main() {
  const lone = String.fromCharCode(0xd800);
  return {
    utf8: [utf8("a"), utf8("é"), utf8("€"), utf8("\u{1F600}"), utf8(lone), utf8("")].map((b) => Array.from(b)),
    base64: [[], [0x66], [0x66, 0x6f], [0x66, 0x6f, 0x6f], [0xff, 0xfe, 0xfd, 0xfc]]
      .map((b) => toBase64(Uint8Array.from(b))),
    text: toBase64(utf8("é€\u{1F600}")),
  };
}"#,
    );
    assert_eq!(
        out["utf8"],
        json!([
            [97],
            [0xc3, 0xa9],
            [0xe2, 0x82, 0xac],
            [0xf0, 0x9f, 0x98, 0x80],
            [0xef, 0xbf, 0xbd],
            []
        ])
    );
    assert_eq!(
        out["base64"],
        json!(["", "Zg==", "Zm8=", "Zm9v", "//79/A=="])
    );
    assert_eq!(out["text"], "w6nigqzwn5iA");
}

// The TLS step. ------------------------------------------------------------------------------

const PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
const STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// The settings of the coordinator with the three of the ACME half at the values given.
fn settings_with(zone: &str, directory: &str, contact: &str) -> Value {
    let mut all = settings();
    let list = all["settings"].as_array_mut().expect("a list");
    for (name, value) in [
        ("dns_zone", zone),
        ("acme_directory", directory),
        ("acme_contact", contact),
    ] {
        list.push(json!({"name": name, "value": value, "isDefault": value.is_empty()}));
    }
    all
}

/// Scripts one drawing with the ACME settings at the values given.
fn draw_acme(ui: &Ui, status: Value, zone: &str, directory: &str, contact: &str) {
    ui.reply(&m(STATUS), status);
    ui.reply(&m(SETTINGS), settings_with(zone, directory, contact));
}

fn put_secret_reply(kind: &str, version: u32) -> Value {
    json!({"secret": {"kind": kind, "version": version, "fingerprintSha256": "cd".repeat(32),
                      "createdUnix": "1799000000"}})
}

/// The bodies sent to `method`, in order.
fn sent_to(ui: &Ui, method: &str) -> Vec<Value> {
    bodies(ui)
        .into_iter()
        .filter(|(called, _)| called == method)
        .map(|(_, body)| body)
        .collect()
}

#[test]
fn the_tls_key_is_stored_first_and_then_the_certificate_and_the_key_is_never_shown() {
    // Catches: the certificate stored before its key, a key typed in the page or kept after it is
    // stored, any part of the key (text or base64) in the page or a report, a chosen key lost
    // by the certificate's refusal (the retry needs no new choice) or kept after a success, a
    // missing key file sent as an empty key, and a refusal that does not say the key was stored.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.reply(&m(PUT_SECRET), put_secret_reply("tls_key", 3));
    ui.refuse(&m(PUT_SECRET), 400, "INVALID_ARGUMENT", "invalid_request");
    draw(&ui, held(90, 90, 90));
    ui.reply(&m(PUT_SECRET), put_secret_reply("tls_key", 4));
    ui.reply(&m(PUT_SECRET), put_secret_reply("tls_cert", 5));
    draw(&ui, held(90, 90, 90));
    let key_bytes: Vec<String> = format!("{}\n", KEY_TEXT.replace("\\n", "\n").trim_end())
        .bytes()
        .map(|b| b.to_string())
        .collect();
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
type(tree, "tls-certificate-text", "{CERT_TEXT}");
await click(tree, "put-tls");
named(tree, "tls-key-file")[0].on.change({{ target: {{ files: [{key}] }} }});
await click(tree, "put-tls");
const afterRefusal = seen.shown[seen.shown.length - 1];
const chosenAfterRefusal = text(named(afterRefusal, "tls-key-file-chosen")[0]);
named(afterRefusal, "tls-key-file")[0].on.change({{ target: {{ files: [{key}] }} }});
await click(afterRefusal, "put-tls");
const after = seen.shown[seen.shown.length - 1];
return {{ chosenAfterRefusal, chosen: text(named(after, "tls-key-file-chosen")[0]),
         keyFields: nodes(after).filter((n) => n.tag === "textarea" && /key/.test(n.attrs["name"])).length,
         everything: JSON.stringify([shown, seen]) + text(after) }};
"#,
            key = file("key.pem", &format!("[{}]", key_bytes.join(", ")))
        ),
    ));
    assert_eq!(
        sent_to(&ui, &m(PUT_SECRET)),
        [
            json!({"kind": "tls_key", "material": KEY_B64}),
            json!({"kind": "tls_cert", "material": CERT_B64}),
            json!({"kind": "tls_key", "material": KEY_B64}),
            json!({"kind": "tls_cert", "material": CERT_B64}),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the key file.",
            "The key was stored (version 3), the certificate was not: INVALID_ARGUMENT invalid_request. \
             Until a certificate for this key is stored, the status shows a pair that does not match.",
            "Stored the TLS key and certificate (certificate version 5). The status says whether it is a pair, \
             names the MDM name and has not expired.",
        ])
    );
    assert_eq!(
        out["out"]["chosenAfterRefusal"], "",
        "the key file is dropped once it is stored, even when the certificate is refused"
    );
    assert_eq!(out["out"]["chosen"], "");
    assert_eq!(out["out"]["keyFields"], 0);
    let everything = out["out"]["everything"].as_str().expect("text");
    for secret in ["SECRETKEYBODY", KEY_B64, "-----BEGIN RSA PRIVATE KEY-----"] {
        assert!(
            !everything.contains(secret),
            "{secret} is in the page's output"
        );
    }
}

#[test]
fn a_missing_or_oversize_tls_certificate_is_refused_before_any_call() {
    // Catches: an empty certificate sent as a secret, and a certificate over the 32 KiB limit of
    // `PutSecret` sent to be refused by the coordinator.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
await click(tree, "put-tls");
named(tree, "tls-certificate-file")[0].on.change({ target: { files: [{ name: "big.pem", size: 32769, arrayBuffer: async () => new ArrayBuffer(32769) }] } });
await click(tree, "put-tls");
"#,
    ));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the certificate or paste its text.",
            "The certificate is at most 32768 bytes; this file is 32769."
        ])
    );
    assert!(sent_to(&ui, &m(PUT_SECRET)).is_empty());
}

#[test]
fn the_acme_settings_send_only_what_changed_and_name_what_was_saved() {
    // Catches: a setting sent that did not change (an audit entry for nothing), one that changed
    // left out, a value not trimmed, the current values not offered as the fields' start, a
    // failure that does not say which setting it was or what was saved before it, and a click
    // with no change that sends anything.
    let mut ui = Ui::start();
    draw_acme(&ui, held(90, 90, 90), "", PRODUCTION, "");
    draw_acme(&ui, held(90, 90, 90), "example.com", STAGING, "");
    ui.reply(
        &m(PUT_SETTING),
        json!({"name": "dns_zone", "value": "example.com"}),
    );
    ui.reply(
        &m(PUT_SETTING),
        json!({"name": "acme_directory", "value": STAGING}),
    );
    ui.refuse(&m(PUT_SETTING), 400, "INVALID_ARGUMENT", "invalid_request");
    draw_acme(&ui, held(90, 90, 90), "example.com", STAGING, "");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
const start = ["acme-dns-zone", "acme-directory", "acme-contact"].map((n) => named(tree, n)[0].attrs.value);
await click(tree, "put-acme-settings");
type(tree, "acme-dns-zone", "  example.com ");
type(tree, "acme-directory", "{STAGING}");
await click(tree, "put-acme-settings");
const next = seen.shown[seen.shown.length - 1];
const kept = ["acme-dns-zone", "acme-directory", "acme-contact"].map((n) => named(next, n)[0].attrs.value);
await click(next, "put-acme-settings");
type(next, "acme-contact", "not an address");
await click(next, "put-acme-settings");
return {{ start, kept }};
"#
        ),
    ));
    assert_eq!(out["out"]["start"], json!(["", PRODUCTION, ""]));
    assert_eq!(out["out"]["kept"], json!(["example.com", STAGING, ""]));
    assert_eq!(
        sent_to(&ui, &m(PUT_SETTING)),
        [
            json!({"name": "dns_zone", "value": "example.com"}),
            json!({"name": "acme_directory", "value": STAGING}),
            json!({"name": "acme_contact", "value": "not an address"}),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Nothing changed.",
            "Saved dns_zone, acme_directory.",
            "Nothing changed.",
            "INVALID_ARGUMENT invalid_request (acme_contact)",
        ])
    );
}

#[test]
fn a_dns_credential_is_sent_as_the_providers_json_and_is_never_kept_or_shown() {
    // Catches: an sso-key credential sent without its secret or a token credential with a key,
    // the kind chosen not the kind sent, a value in the page, a report or the held line after the
    // upload or after a refusal, the typed values still in the fields after a refusal, and an
    // empty field sent.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90, 90));
    ui.reply(&m(PUT_SECRET), put_secret_reply("dns_credential", 1));
    draw(&ui, held(90, 90, 90));
    ui.refuse(&m(PUT_SECRET), 400, "INVALID_ARGUMENT", "invalid_request");
    draw(&ui, held(90, 90, 90));
    ui.reply(&m(PUT_SECRET), put_secret_reply("dns_credential", 2));
    draw(&ui, held(90, 90, 90));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
await click(tree, "put-dns-credential");
type(tree, "dns-key", " GDKEY123 ");
await click(tree, "put-dns-credential");
type(tree, "dns-secret", "GDSECRET456");
await click(tree, "put-dns-credential");
const first = seen.shown[seen.shown.length - 1];
const typed = (t) => ["dns-key", "dns-secret", "dns-token"].map((n) => named(t, n)[0].attrs.value);
const afterFirst = typed(first);
choose(first, "dns-kind", "bearer");
type(first, "dns-token", "PATTOKEN789");
await click(first, "put-dns-credential");
const second = seen.shown[seen.shown.length - 1];
const afterRefusal = typed(second);
choose(second, "dns-kind", "bearer");
type(second, "dns-token", "PATTOKEN789");
await click(second, "put-dns-credential");
const third = seen.shown[seen.shown.length - 1];
return { afterFirst, afterRefusal, afterThird: typed(third),
         everything: JSON.stringify([shown, seen.reports, third]) + text(third) };
"#,
    ));
    // the first drawing's kind is sso-key: the second click has a key and no secret; the third both
    assert_eq!(
        sent_to(&ui, &m(PUT_SECRET)),
        [
            json!({"kind": "dns_credential",
                   "material": "eyJwcm92aWRlciI6ImdvZGFkZHkiLCJraW5kIjoic3NvLWtleSIsImtleSI6IkdES0VZMTIzIiwic2VjcmV0IjoiR0RTRUNSRVQ0NTYifQ=="}),
            json!({"kind": "dns_credential",
                   "material": "eyJwcm92aWRlciI6ImdvZGFkZHkiLCJraW5kIjoiYmVhcmVyIiwidG9rZW4iOiJQQVRUT0tFTjc4OSJ9"}),
            json!({"kind": "dns_credential",
                   "material": "eyJwcm92aWRlciI6ImdvZGFkZHkiLCJraW5kIjoiYmVhcmVyIiwidG9rZW4iOiJQQVRUT0tFTjc4OSJ9"}),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give the key and the secret.",
            "Give the key and the secret.",
            "Stored the DNS credential (version 1).",
            "INVALID_ARGUMENT invalid_request",
            "Stored the DNS credential (version 2).",
        ])
    );
    for stage in ["afterFirst", "afterRefusal", "afterThird"] {
        assert_eq!(out["out"][stage], json!(["", "", ""]), "{stage}");
    }
    let everything = out["out"]["everything"].as_str().expect("text");
    for secret in ["GDKEY123", "GDSECRET456", "PATTOKEN789"] {
        assert!(
            !everything.contains(secret),
            "{secret} is in the page's output"
        );
    }
}

#[test]
fn the_renewal_status_says_what_it_lacks_that_it_is_on_and_why_it_failed() {
    // Catches: a renewal said to be on without the name, the zone or the credential, a failing
    // renewal that omits the last error, the first failure, the attempts, the provider's refusal
    // or the next attempt, a wait of no time shown as a date, a failure shown with no alert line in
    // the status panel, and a healthy renewal that shows a failure.
    let mut ui = Ui::start();
    let mut bare = held(90, 90, 90);
    bare.as_object_mut().expect("an object").remove("name");
    draw_acme(&ui, bare, "", PRODUCTION, "");
    let mut on = held(90, 90, 90);
    on["dnsCredential"] = info("dns_credential");
    draw_acme(&ui, on.clone(), "example.com", PRODUCTION, "");
    let mut failing = on.clone();
    failing["tlsRenewal"] = json!({
        "sinceUnix": (NOW - DAY).to_string(), "attempts": 3,
        "message": "the provider refused the credential",
        "dnsRejected": "401 Unauthorized", "retryAtUnix": (NOW + 3600).to_string(),
    });
    draw_acme(&ui, failing, "example.com", PRODUCTION, "");
    let mut waiting = on;
    waiting["tlsRenewal"] = json!({
        "sinceUnix": (NOW - 60).to_string(), "attempts": 1, "message": "the order did not finish",
    });
    draw_acme(&ui, waiting, "example.com", PRODUCTION, "");
    let render_state = |ui: &mut Ui| {
        let out = ui.run(&page_driver(
            "mdm",
            "{}",
            "[]",
            r#"
const present = (name) => nodes(tree).some((n) => n.attrs && n.attrs["name"] === name);
return { off: present("renewal-off") ? text(named(tree, "renewal-off")[0]) : null,
         waiting: present("renewal-waiting") ? text(named(tree, "renewal-waiting")[0]) : null,
         detail: present("renewal-failing-detail"), alert: present("renewal-failing"),
         rows: rows(named(tree, "renewal-status")[0]) };
"#,
        ));
        out["out"].clone()
    };
    let off = render_state(&mut ui);
    assert_eq!(
        off["off"],
        "The renewal by ACME is off: it needs the MDM name, the DNS zone, a DNS credential. A certificate uploaded below is served and not renewed."
    );
    assert_eq!(off["detail"], false);
    let on = render_state(&mut ui);
    assert_eq!(on["off"], Value::Null);
    assert_eq!(
        on["waiting"],
        "The renewal by ACME is on. No attempt has failed since the coordinator started; it orders a certificate when the one in use has 30 days left or fewer."
    );
    assert_eq!(on["alert"], false);
    let failed = render_state(&mut ui);
    assert_eq!(failed["waiting"], Value::Null);
    assert_eq!(failed["alert"], true);
    assert_eq!(
        failed["rows"],
        json!([
            ["", ""],
            ["Last error", "the provider refused the credential"],
            ["First failed", "2027-01-14 08:00:00 UTC"],
            ["Failed attempts", "3"],
            ["DNS provider", "refused the credential: 401 Unauthorized"],
            ["Next attempt", "not before 2027-01-15 09:00:00 UTC"],
        ])
    );
    let waited = render_state(&mut ui);
    assert_eq!(
        waited["rows"],
        json!([
            ["", ""],
            ["Last error", "the order did not finish"],
            ["First failed", "2027-01-15 07:59:00 UTC"],
            ["Failed attempts", "1"],
            ["Next attempt", "at the next look"],
        ])
    );
}
