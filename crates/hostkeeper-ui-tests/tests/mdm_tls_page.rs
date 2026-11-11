//! The TLS step of the MDM setup page, run in the JavaScript engine against the fake gateway of
//! `support`: the certificate and key upload, the ACME settings, the DNS credential and the
//! renewal status.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::mdm_setup::{
    CERT_B64, CERT_TEXT, DAY, KEY_B64, KEY_TEXT, NOW, PUT_SECRET, PUT_SETTING, SETTINGS, STATUS,
    draw, file, held, info, settings,
};
use support::pages::{bodies, m, page_driver};

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
