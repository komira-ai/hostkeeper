//! The Apple Business Manager step and the name step of the MDM setup page, run in the JavaScript
//! engine against the fake gateway of `support`: the token of a takeover and the `.p7m` as files, the
//! key pair, the public key, the last sync and the expiry of the token in the status panel, the two
//! faults Apple names, and the name with its enrollment URL, which locks once a Mac has enrolled.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver, shown};

/// The time the kit's page context reads.
const NOW: i64 = 1_800_000_000;
/// `2027-10-31T22:01:36Z`, the shape of the expiry of the token the spike read.
const TOKEN_ENDS: i64 = 1_825_020_096;

const STATUS: &str = "Mdm/Status";
const SETTINGS: &str = "Mdm/ListSettings";
const PUT_TOKEN: &str = "Mdm/PutAbmToken";
const PUT_SECRET: &str = "Mdm/PutSecret";
const PUBLIC_KEY: &str = "Mdm/GetAbmPublicKey";
const PUT_NAME: &str = "Mdm/PutName";

const TOKEN_TEXT: &str = r#"{"consumer_secret":"SECRETTOKENPART"}"#;
const TOKEN_B64: &str = "eyJjb25zdW1lcl9zZWNyZXQiOiJTRUNSRVRUT0tFTlBBUlQifQ==";
const KEY_TEXT: &str =
    "-----BEGIN RSA PRIVATE KEY-----\nABMKEYBODY\n-----END RSA PRIVATE KEY-----\n";
const KEY_B64: &str = "LS0tLS1CRUdJTiBSU0EgUFJJVkFURSBLRVktLS0tLQpBQk1LRVlCT0RZCi0tLS0tRU5EIFJTQSBQUklWQVRFIEtFWS0tLS0tCg==";
const CERT_TEXT: &str = "-----BEGIN CERTIFICATE-----\nABMCERT\n-----END CERTIFICATE-----\n";
const CERT_B64: &str =
    "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCkFCTUNFUlQKLS0tLS1FTkQgQ0VSVElGSUNBVEUtLS0tLQo=";

const CLOCK_FAULT: &str = "Apple refused the time of the machine the coordinator runs on: it must be within an hour of Apple's. Set that clock, then try again.";
const CREDENTIAL_FAULT: &str = "Apple refused the token's signature: its four parts do not belong together, or the token was revoked in ABM. Upload the token ABM or NanoDEP holds now.";

fn info(kind: &str) -> Value {
    json!({"kind": kind, "version": 1, "fingerprintSha256": "cd".repeat(32),
           "createdUnix": "1799000000", "notAfterUnix": TOKEN_ENDS.to_string()})
}

/// A `Mdm.Status` answer of a setup that holds an ABM token ending at `TOKEN_ENDS`.
fn held() -> Value {
    json!({
        "configured": true, "sealing": true,
        "name": "mdm.example.com", "enrollmentUrl": "https://mdm.example.com/enroll",
        "abmToken": info("abm_token"),
        "abmNotAfterUnix": TOKEN_ENDS.to_string(),
        "abmLastSyncUnix": NOW.to_string(),
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

/// A file as the browser hands one over: its name, size and the bytes of `content`.
fn file(name: &str, content: &str) -> String {
    let bytes: Vec<String> = content.bytes().map(|b| b.to_string()).collect();
    format!(
        r#"{{ name: "{name}", size: {n}, arrayBuffer: async () => Uint8Array.from([{list}]).buffer }}"#,
        n = bytes.len(),
        list = bytes.join(", ")
    )
}

/// JavaScript that chooses `file` in the control `name` of `tree`.
fn choose(tree: &str, name: &str, file: &str) -> String {
    format!("named({tree}, \"{name}\")[0].on.change({{ target: {{ files: [{file}] }} }});")
}

#[test]
fn the_status_panel_shows_the_token_expiry_the_last_sync_and_a_failed_sync_with_what_to_do() {
    // Catches: an expiry shown in another shape than the token's own (2027-10-31 22:01:36 UTC), a
    // last sync shown as a time when there is none, a failed sync shown as healthy, the clock
    // fault and the credential fault shown without what to do, a failure Apple does not name
    // shown with advice, and a good sync shown with a warning.
    let mut ui = Ui::start();
    draw(&ui, held());
    let mut clock = held();
    clock
        .as_object_mut()
        .expect("an object")
        .remove("abmLastSyncUnix");
    clock["abmSyncError"] = json!(
        "ABM refused the timestamp (timestamp_refused): this machine's clock is off by an hour or more"
    );
    draw(&ui, clock);
    let mut credential = held();
    credential["abmSyncError"] =
        json!("ABM refused the token's signature (signature_invalid): the token is not good");
    draw(&ui, credential);
    let mut other = held();
    other["abmSyncError"] = json!("ABM answered status 500");
    draw(&ui, other);
    let mut none = held();
    none["abmUnavailable"] = json!(true);
    none.as_object_mut()
        .expect("an object")
        .remove("abmLastSyncUnix");
    draw(&ui, none);
    let drive = |ui: &mut Ui| {
        ui.run(&page_driver(
            "mdm",
            "{}",
            "[]",
            &format!("{ROW} return {{ token: row(\"ABM token\"), sync: row(\"ABM last sync\") }};"),
        ))
    };
    let good = drive(&mut ui);
    let clock = drive(&mut ui);
    let credential = drive(&mut ui);
    let other = drive(&mut ui);
    let none = drive(&mut ui);

    assert_eq!(
        good["out"]["token"],
        json!([
            "",
            [
                "ABM token",
                format!("version 1, {}", "cd".repeat(8)),
                "2027-10-31 22:01:36 UTC",
                "289 days left"
            ]
        ])
    );
    assert_eq!(
        good["out"]["sync"][1],
        json!(["ABM last sync", "2027-01-15 08:00:00 UTC"])
    );
    assert!(!shown(&good).contains("sync failed"), "{}", shown(&good));

    assert_eq!(
        none["out"]["sync"][1],
        json!(["ABM last sync", "this coordinator runs no ABM client"])
    );
    assert_eq!(
        clock["out"]["sync"][1],
        json!(["ABM last sync", "none since the coordinator started"])
    );
    assert_shows(
        &clock,
        &[
            "The last ABM sync failed: ABM refused the timestamp (timestamp_refused): this machine's clock is off by an hour or more",
            CLOCK_FAULT,
        ],
    );
    assert_shows(
        &credential,
        &[
            "The last ABM sync failed: ABM refused the token's signature (signature_invalid): the token is not good",
            CREDENTIAL_FAULT,
        ],
    );
    assert!(!shown(&credential).contains(CLOCK_FAULT));
    assert_shows(
        &other,
        &["The last ABM sync failed: ABM answered status 500"],
    );
    assert!(!shown(&other).contains(CLOCK_FAULT) && !shown(&other).contains(CREDENTIAL_FAULT));
}

#[test]
fn the_token_of_a_takeover_is_sent_as_a_file_and_never_shown() {
    // Catches: a token sent from pasted text (there is no field for one), a token sent with no
    // file chosen, the file's bytes altered, the answer not naming the server entry, the
    // organisation and the expiry, the chosen file kept after it was stored, a part of the token
    // (as text or as base64) in the page or a report, and a refusal reworded or followed by a
    // redrawn page.
    let mut ui = Ui::start();
    draw(&ui, held());
    ui.reply(
        &m(PUT_TOKEN),
        json!({"secret": info("abm_token"), "serverName": "js-mdm", "orgName": "Org"}),
    );
    draw(&ui, held());
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
await click(tree, "put-abm-token");
{choose}
await click(tree, "put-abm-token");
const after = seen.shown[seen.shown.length - 1];
return {{ chosen: text(named(after, "abm-token-file-chosen")[0]),
         textFields: nodes(after).filter((n) => n.tag === "textarea" && /token|abm/.test(n.attrs["name"])).length,
         everything: JSON.stringify([shown, seen]) + text(after) }};
"#,
            choose = choose("tree", "abm-token-file", &file("token.json", TOKEN_TEXT))
        ),
    ));
    assert_eq!(
        bodies(&ui)
            .into_iter()
            .filter(|(method, _)| *method == m(PUT_TOKEN))
            .collect::<Vec<_>>(),
        [(m(PUT_TOKEN), json!({"tokenJson": TOKEN_B64}))]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the token file.",
            "Apple accepted the token and it is stored, for js-mdm of Org, valid until 2027-10-31 22:01:36 UTC."
        ])
    );
    assert_eq!(out["out"]["chosen"], "", "the file is dropped once stored");
    let everything = out["out"]["everything"].as_str().expect("text");
    for secret in ["SECRETTOKENPART", TOKEN_B64] {
        assert!(
            !everything.contains(secret),
            "{secret} is in the page's output"
        );
    }
    assert_eq!(
        out["out"]["textFields"].as_u64(),
        Some(0),
        "no text field takes a token or a key"
    );
}

#[test]
fn a_token_that_is_too_big_or_refused_is_reported_exactly_with_what_to_do_about_apples_faults() {
    // Catches: a file over the API's 32768 bytes read and sent, a refusal reworded, the clock
    // fault and the credential fault of Apple shown without what to do about them, a fault
    // explained with the advice of the other, the chosen file lost by a refusal (the retry needs
    // no new choice), and a refusal followed by a redrawn page.
    let mut ui = Ui::start();
    draw(&ui, held());
    ui.refuse(
        &m(PUT_TOKEN),
        412,
        "FAILED_PRECONDITION",
        "abm_clock_refused",
    );
    ui.refuse(
        &m(PUT_TOKEN),
        412,
        "FAILED_PRECONDITION",
        "abm_credential_refused",
    );
    ui.refuse(&m(PUT_TOKEN), 503, "UNAVAILABLE", "abm_unreachable");
    ui.refuse(&m(PUT_TOKEN), 400, "INVALID_ARGUMENT", "invalid_request");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
let read = false;
named(tree, "abm-token-file")[0].on.change({{ target: {{ files: [{{ name: "big", size: 32769,
  arrayBuffer: async () => {{ read = true; return new ArrayBuffer(0); }} }}] }} }});
await click(tree, "put-abm-token");
{choose}
await click(tree, "put-abm-token");
await click(tree, "put-abm-token");
await click(tree, "put-abm-token");
await click(tree, "put-abm-token");
return {{ read, redrawn: seen.shown.length }};
"#,
            choose = choose("tree", "abm-token-file", &file("token.json", TOKEN_TEXT))
        ),
    ));
    assert_eq!(out["out"], json!({"read": false, "redrawn": 0}));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "The token is at most 32768 bytes; this file is 32769.",
            format!("FAILED_PRECONDITION abm_clock_refused. {CLOCK_FAULT}"),
            format!("FAILED_PRECONDITION abm_credential_refused. {CREDENTIAL_FAULT}"),
            "UNAVAILABLE abm_unreachable. No answer came from Apple. Try again.",
            "INVALID_ARGUMENT invalid_request",
        ])
    );
    assert_eq!(
        bodies(&ui)
            .into_iter()
            .filter(|(method, _)| *method == m(PUT_TOKEN))
            .count(),
        4,
        "the file over the limit was not sent"
    );
}

#[test]
fn a_p7m_is_sent_as_bytes_and_its_refusals_say_what_to_do() {
    // Catches: a .p7m sent in the field of the JSON (or the JSON in the .p7m's), its bytes
    // altered, a missing file sent as an empty one, and the refusals of a missing key and of a
    // file that does not open reported without what to do.
    let mut ui = Ui::start();
    draw(&ui, held());
    ui.reply(
        &m(PUT_TOKEN),
        json!({"secret": info("abm_token"), "serverName": "js-mdm", "orgName": "Org"}),
    );
    draw(&ui, held());
    ui.refuse(&m(PUT_TOKEN), 412, "FAILED_PRECONDITION", "abm_key_missing");
    ui.refuse(
        &m(PUT_TOKEN),
        400,
        "INVALID_ARGUMENT",
        "abm_token_unreadable",
    );
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
await click(tree, "put-abm-p7m");
{choose}
await click(tree, "put-abm-p7m");
const after = seen.shown[seen.shown.length - 1];
{choose_again}
await click(after, "put-abm-p7m");
await click(after, "put-abm-p7m");
"#,
            choose = choose("tree", "abm-p7m-file", "{ name: \"t.p7m\", size: 3, arrayBuffer: async () => Uint8Array.from([0x30, 0x80, 0xff]).buffer }"),
            choose_again = choose("after", "abm-p7m-file", "{ name: \"t.p7m\", size: 3, arrayBuffer: async () => Uint8Array.from([0x30, 0x80, 0xff]).buffer }"),
        ),
    ));
    assert_eq!(
        bodies(&ui)
            .into_iter()
            .filter(|(method, _)| *method == m(PUT_TOKEN))
            .collect::<Vec<_>>(),
        vec![(m(PUT_TOKEN), json!({"tokenP7m": "MID/"})); 3]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the .p7m file.",
            "Apple accepted the token and it is stored, for js-mdm of Org, valid until 2027-10-31 22:01:36 UTC.",
            "FAILED_PRECONDITION abm_key_missing. The ABM key is not held here, or a certificate is held without it. Take over the key and its certificate (a .p7m opens only with the key held).",
            "INVALID_ARGUMENT abm_token_unreadable. The file does not open with the ABM key held. It must be the .p7m ABM made for that key.",
        ])
    );
}

#[test]
fn the_key_and_its_certificate_are_sent_in_one_call_from_files_and_the_key_is_never_shown() {
    // Catches: the key sent on its own so that a wrong certificate leaves the held key replaced, a
    // pair sent with the certificate missing, a refused pair that drops the files so the retry
    // needs them chosen again, a certificate alone not sent when the key is stored already, a part of the key (as text or as base64) in the page or a report, a field to
    // type a key into, and the files kept after the pair was stored.
    let mut ui = Ui::start();
    draw(&ui, held());
    ui.refuse(
        &m(PUT_SECRET),
        412,
        "FAILED_PRECONDITION",
        "invalid_request",
    );
    ui.reply(&m(PUT_SECRET), json!({"secret": info("abm_key")}));
    draw(&ui, held());
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"
await click(tree, "put-abm-keypair");
{key}
await click(tree, "put-abm-keypair");
{cert}
await click(tree, "put-abm-keypair");
await click(tree, "put-abm-keypair");
await click(tree, "put-abm-keypair");
const after = seen.shown[seen.shown.length - 1];
return {{ chosen: [text(named(after, "abm-key-file-chosen")[0]), text(named(after, "abm-certificate-file-chosen")[0])],
         keyFields: nodes(after).filter((n) => n.tag === "textarea" && /key/.test(n.attrs["name"])).length,
         everything: JSON.stringify([shown, seen]) + text(after) }};
"#,
            key = choose("tree", "abm-key-file", &file("abm.key", KEY_TEXT)),
            cert = choose("tree", "abm-certificate-file", &file("abm.crt", CERT_TEXT)),
        ),
    ));
    let sent: Vec<Value> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m(PUT_SECRET))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        sent,
        [
            // the third press: the key and its certificate go in one call, which is refused
            json!({"kind": "abm_key", "material": KEY_B64, "withCertificate": CERT_B64}),
            // the fourth: the same pair, taken; the fifth sends nothing, both files are dropped
            json!({"kind": "abm_key", "material": KEY_B64, "withCertificate": CERT_B64}),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Choose the certificate file.",
            "Choose the certificate file.",
            "FAILED_PRECONDITION invalid_request",
            "Stored the ABM key and its certificate.",
            "Choose the certificate file.",
        ])
    );
    assert_eq!(
        out["out"]["chosen"],
        json!(["", ""]),
        "both files are dropped once stored"
    );
    assert_eq!(out["out"]["keyFields"], 0);
    let everything = out["out"]["everything"].as_str().expect("text");
    for secret in ["ABMKEYBODY", KEY_B64, "-----BEGIN RSA PRIVATE KEY-----"] {
        assert!(
            !everything.contains(secret),
            "{secret} is in the page's output"
        );
    }
}

#[test]
fn the_public_key_is_shown_with_its_fingerprint_and_offered_as_a_file() {
    // Catches: a key pair made on the page's own, a certificate shown without saying whether it
    // was made now or held, a fingerprint left out, the certificate not offered as a file, and a
    // refusal reworded.
    let mut ui = Ui::start();
    draw(&ui, held());
    ui.reply(
        &m(PUBLIC_KEY),
        json!({"certificatePem": "-----BEGIN CERTIFICATE-----\nPUB\n-----END CERTIFICATE-----\n",
               "fingerprintSha256": "ef".repeat(32), "created": true}),
    );
    draw(&ui, held());
    ui.reply(
        &m(PUBLIC_KEY),
        json!({"certificatePem": "-----BEGIN CERTIFICATE-----\nPUB\n-----END CERTIFICATE-----\n",
               "fingerprintSha256": "ef".repeat(32)}),
    );
    draw(&ui, held());
    ui.refuse(
        &m(PUBLIC_KEY),
        412,
        "FAILED_PRECONDITION",
        "abm_key_missing",
    );
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
await click(tree, "get-abm-key");
const first = text(seen.shown[seen.shown.length - 1]);
await click(seen.shown[seen.shown.length - 1], "get-abm-key");
const second = seen.shown[seen.shown.length - 1];
await click(second, "download-abm-certificate");
await click(second, "get-abm-key");
return { first, second: text(second) };
"#,
    ));
    assert_eq!(
        bodies(&ui)
            .into_iter()
            .filter(|(method, _)| *method == m(PUBLIC_KEY))
            .collect::<Vec<_>>(),
        vec![(m(PUBLIC_KEY), json!({})); 3]
    );
    let first = out["out"]["first"].as_str().expect("text");
    let second = out["out"]["second"].as_str().expect("text");
    for piece in [
        "The coordinator made this key pair now.",
        &format!("SHA-256 of the certificate: {}", "ef".repeat(32)),
        "-----BEGIN CERTIFICATE-----\nPUB\n-----END CERTIFICATE-----\n",
    ] {
        assert!(first.contains(piece), "{piece:?} is not shown in: {first}");
    }
    assert!(
        second.contains("The coordinator already held this key pair."),
        "{second}"
    );
    assert_eq!(
        out["seen"]["downloads"],
        json!([{"name": "abm-server.crt", "mime": "text/plain",
                "text": "-----BEGIN CERTIFICATE-----\nPUB\n-----END CERTIFICATE-----\n"}])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Made the key pair.",
            "The key pair held is shown.",
            "FAILED_PRECONDITION abm_key_missing. The ABM key is not held here, or a certificate is held without it. Take over the key and its certificate (a .p7m opens only with the key held).",
        ])
    );
}

#[test]
fn the_name_is_set_with_its_enrollment_url_and_a_locked_name_has_no_form() {
    // Catches: a name sent untrimmed or empty, the enrollment URL not shown (before the name is set
    // or after), a name set on a page that shows no URL for it, a refusal reworded, a name that is
    // locked still offered for change, and the locked name not said.
    let mut ui = Ui::start();
    let mut unnamed = held();
    {
        let status = unnamed.as_object_mut().expect("an object");
        status.remove("name");
        status.remove("enrollmentUrl");
    }
    draw(&ui, unnamed);
    ui.reply(
        &m(PUT_NAME),
        json!({"name": "mdm.internal.example.com", "enrollmentUrl": "https://mdm.internal.example.com/enroll"}),
    );
    let mut named = held();
    named["name"] = json!("mdm.internal.example.com");
    named["enrollmentUrl"] = json!("https://mdm.internal.example.com/enroll");
    draw(&ui, named);
    ui.refuse(&m(PUT_NAME), 400, "INVALID_ARGUMENT", "invalid_request");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"
const before = text(tree);
await click(tree, "put-name");
type(tree, "mdm-name", "  mdm.internal.example.com ");
await click(tree, "put-name");
const after = seen.shown[seen.shown.length - 1];
const kept = named(after, "mdm-name")[0].attrs.value;
type(after, "mdm-name", "not a name");
await click(after, "put-name");
return { before, after: text(after), kept };
"#,
    ));
    assert_eq!(
        bodies(&ui)
            .into_iter()
            .filter(|(method, _)| *method == m(PUT_NAME))
            .collect::<Vec<_>>(),
        [
            (m(PUT_NAME), json!({"name": "mdm.internal.example.com"})),
            (m(PUT_NAME), json!({"name": "not a name"})),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Give the name the Macs enroll at.",
            "The MDM name is mdm.internal.example.com. Macs enroll at https://mdm.internal.example.com/enroll.",
            "INVALID_ARGUMENT invalid_request",
        ])
    );
    let before = out["out"]["before"].as_str().expect("text");
    assert!(before.contains("No name is set."), "{before}");
    assert!(
        before.contains("on the LAN, to the address of this coordinator's MDM listener (port 443)"),
        "{before}"
    );
    let after = out["out"]["after"].as_str().expect("text");
    assert!(
        after.contains("Macs enroll at https://mdm.internal.example.com/enroll"),
        "{after}"
    );
    assert_eq!(out["out"]["kept"], "mdm.internal.example.com");

    draw(&ui, {
        let mut locked = held();
        locked["nameLocked"] = json!(true);
        locked
    });
    let locked = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        "return [\"mdm-name\", \"put-name\"].map((name) => nodes(tree).filter((n) => n.attrs && n.attrs[\"name\"] === name).length);",
    ));
    assert_eq!(locked["out"], json!([0, 0]), "a locked name has no form");
    assert_shows(
        &locked,
        &[
            "Macs enroll at https://mdm.example.com/enroll",
            "The name is locked: a Mac has enrolled at mdm.example.com, and the enrollment profile it holds carries it. Another name would need every Mac enrolled again.",
        ],
    );
}
