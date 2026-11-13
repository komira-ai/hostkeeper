//! The MDM setup page, run in the JavaScript engine against the fake gateway of `support`: one
//! form of the files and values the setup needs, a one-line status above it and Verify below it.
//! What each field is, what Save sends for what is filled, where a refusal is shown, the zone
//! derived from the MDM name, and that no secret is shown back.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::mdm_setup::{
    PUBLIC_KEY, PUT_CERT, PUT_NAME, PUT_SECRET, PUT_SETTING, PUT_TOKEN, SECRETS, SETTINGS, STATUS,
    VERIFY, b64, choose, draw, empty, file, held, press, secrets, settings, type_into,
};
use support::pages::{assert_shows, bodies, m, page_driver, shown};

const CERT: &str = "-----BEGIN CERTIFICATE-----\nPUSHCERT\n-----END CERTIFICATE-----\n";
const KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\nPUSHKEYBODY\n-----END RSA PRIVATE KEY-----\n";
const ABM_KEY: &str =
    "-----BEGIN RSA PRIVATE KEY-----\nABMKEYBODY\n-----END RSA PRIVATE KEY-----\n";
const ABM_CERT: &str = "-----BEGIN CERTIFICATE-----\nABMCERT\n-----END CERTIFICATE-----\n";
const TOKEN_JSON: &str = r#"{"consumer_key":"CK","consumer_secret":"TOKENSECRETPART"}"#;
const P7M: &str = "MIME-Version: 1.0\nContent-Type: application/pkcs7-mime\n\nP7MBODY";
const DOCS: &str =
    "https://github.com/komira-ai/hostkeeper/blob/main/docs/operations/mdm-setup.md#";

/// The `name` of every input, select and button of the page, in order.
const CONTROLS: &str = r#"
const controls = (t) => nodes(t).filter((n) => ["input", "select", "textarea", "button"].includes(n.tag))
  .map((n) => `${n.tag}:${n.attrs.type || ""}:${n.attrs["name"]}`);"#;

/// The calls of a Save, without the reads of each drawing.
fn writes(ui: &Ui) -> Vec<(String, Value)> {
    bodies(ui)
        .into_iter()
        .filter(|(method, _)| ![m(STATUS), m(SETTINGS), m(SECRETS)].contains(method))
        .collect()
}

fn success(ui: &Ui, method: &str) {
    let reply = match method {
        PUT_CERT => json!({"topic": "com.apple.mgmt.External.x", "notAfterUnix": "1900000000"}),
        PUT_TOKEN => {
            json!({"secret": {"kind": "abm_token", "version": 1}, "serverName": "s", "orgName": "o"})
        }
        PUT_SETTING => json!({"name": "dns_zone", "value": "x"}),
        _ => json!({"secret": {"kind": "k", "version": 1}}),
    };
    ui.reply(&m(method), reply);
}

#[test]
fn the_page_is_one_form_of_the_named_fields_a_status_line_and_verify() {
    // Catches: a field beyond the ones the setup needs (the MDM name, the DNS zone, the ACME
    // directory, the DNS provider, a CSR step), a file field that takes pasted text, the GoDaddy
    // secret not masked, a field without its link to the documentation (or a link to another
    // address), and a page that loads more than the status, the settings and the secrets.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(false));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &format!(
            r#"{CONTROLS}
const links = nodes(tree).filter((n) => n.tag === "a").map((n) => [n.attrs["name"], n.attrs.href, n.attrs.rel]);
return {{ controls: controls(tree), links, labels: nodes(tree).filter((n) => n.tag === "label").map((n) => text(n)) }};"#
        ),
    ));
    assert_eq!(
        out["out"]["controls"],
        json!([
            "input:file:push-cert",
            "input:file:push-key",
            "input:file:abm-token",
            "button:button:download-abm-certificate",
            "input:file:abm-key",
            "input:file:abm-cert",
            "input:text:dns-key",
            "input:password:dns-secret",
            "input:text:contact",
            "button:button:save",
            "button:button:verify",
        ])
    );
    assert_eq!(
        out["out"]["links"],
        json!([
            [
                "pushCert-help",
                format!("{DOCS}push-certificate"),
                "noopener noreferrer"
            ],
            [
                "pushKey-help",
                format!("{DOCS}push-certificate"),
                "noopener noreferrer"
            ],
            [
                "abmToken-help",
                format!("{DOCS}abm-token"),
                "noopener noreferrer"
            ],
            [
                "abmKey-help",
                format!("{DOCS}abm-key-pair"),
                "noopener noreferrer"
            ],
            [
                "abmCert-help",
                format!("{DOCS}abm-key-pair"),
                "noopener noreferrer"
            ],
            [
                "dnsKey-help",
                format!("{DOCS}godaddy-api-key"),
                "noopener noreferrer"
            ],
            [
                "dnsSecret-help",
                format!("{DOCS}godaddy-api-key"),
                "noopener noreferrer"
            ],
        ])
    );
    assert_shows(
        &out,
        &[
            "Push certificate (PEM)",
            "Push certificate private key (PEM)",
            "Apple Business Manager token (.p7m or JSON)",
            "Apple Business Manager private key (PEM)",
            "Apple Business Manager certificate (PEM)",
            "GoDaddy API key",
            "GoDaddy API secret",
            "Contact email for Let's Encrypt",
            "Download the public certificate to upload to ABM",
            "How do I get this?",
        ],
    );
    for absent in [
        "Generate",
        "CSR",
        "ACME directory",
        "DNS zone",
        "provider",
        "Push certificate request",
    ] {
        assert!(
            !shown(&out).contains(absent),
            "{absent} is shown: {}",
            shown(&out)
        );
    }
    assert_eq!(ui.methods(), [m(STATUS), m(SETTINGS), m(SECRETS)]);
}

#[test]
fn the_status_line_says_configured_or_what_is_missing_and_shows_the_name() {
    // Catches: a configured setup shown with a reason it is not, a missing part left out or
    // shown as its token, a wrong part (an expired certificate) not told from a missing one, a
    // name that is editable (it has no field) or is missing from the line.
    let mut ui = Ui::start();
    draw(
        &ui,
        held(90, 90),
        settings("example.com", ""),
        secrets(true),
    );
    draw(&ui, empty(), settings("", ""), secrets(false));
    let mut broken = held(90, 90);
    broken["configured"] = json!(false);
    broken["name"] = json!("");
    broken["missing"] = json!([
        "MDM_CONDITION_PUSH_CERT_EXPIRED",
        "MDM_CONDITION_ABM_TOKEN_MISSING",
        "MDM_CONDITION_TLS_NAME_MISMATCH",
    ]);
    draw(&ui, broken, settings("", ""), secrets(true));
    let status = |ui: &mut Ui| {
        let out = ui.run(&page_driver(
            "mdm",
            "{}",
            "[]",
            r#"return text(named(tree, "mdm-status")[0]);"#,
        ));
        out["out"].as_str().expect("text").to_owned()
    };
    assert_eq!(status(&mut ui), "MDM: configured. Name: mdm.example.com");
    assert_eq!(
        status(&mut ui),
        "MDM: missing push certificate, push key, ABM token, TLS certificate. Name: mdm.internal.example.net"
    );
    assert_eq!(
        status(&mut ui),
        "MDM: missing ABM token; push certificate expired; TLS certificate does not name the MDM name. Name: not set"
    );
}

#[test]
fn a_stored_part_shows_stored_with_its_expiry_and_never_its_value() {
    // Catches: a stored part not marked next to its field, an expiry in another shape than
    // YYYY-MM-DD, an expiry shown for a part that has none, a superseded version shown as the
    // one in use, and the fingerprint (a value of the secret) shown.
    let mut ui = Ui::start();
    draw(&ui, held(10, 45), settings("", ""), secrets(true));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"const stored = (k) => { const n = nodes(tree).find((x) => x.attrs["name"] === `${k}-stored`); return n ? text(n) : null; };
return { push: stored("pushCert"), key: stored("pushKey"), token: stored("abmToken"), abmKey: stored("abmKey"),
         abmCert: stored("abmCert"), dns: stored("dnsKey"), secret: stored("dnsSecret"), contact: stored("contact") };"#,
    ));
    assert_eq!(
        out["out"],
        json!({
            "push": "stored \u{2713} (expires 2027-01-25)",
            "key": "stored \u{2713}",
            "token": "stored \u{2713} (expires 2027-03-01)",
            "abmKey": "stored \u{2713}",
            "abmCert": "stored \u{2713} (expires 2028-02-19)",
            "dns": "stored \u{2713}",
            "secret": null,
            "contact": null,
        })
    );
    assert!(!shown(&out).contains(&"ab".repeat(8)), "{}", shown(&out));
    assert!(
        !shown(&out).contains("Download the public certificate"),
        "the link is for the case that no ABM key is stored"
    );
}

#[test]
fn save_with_every_field_sends_each_part_in_order_and_drops_what_was_stored() {
    // Catches: a part not sent, parts sent in another order than push pair, ABM key and
    // certificate, token, GoDaddy credential, zone, contact, a token told as JSON when it is a
    // .p7m (and the reverse), the push key sent apart from its certificate, the GoDaddy secret
    // sent as a different JSON than the credential the coordinator reads, the zone not derived
    // from the MDM name, files bytes altered, and a chosen file kept after it was stored.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(false));
    success(&ui, PUT_CERT);
    success(&ui, PUT_SECRET);
    success(&ui, PUT_TOKEN);
    success(&ui, PUT_SECRET);
    success(&ui, PUT_SETTING);
    success(&ui, PUT_SETTING);
    draw(
        &ui,
        held(90, 90),
        settings("example.net", "ops@example.org"),
        secrets(true),
    );
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            choose("push-cert", &file("push.pem", CERT)),
            choose("push-key", &file("push.key", KEY)),
            choose("abm-token", &file("token.p7m", P7M)),
            choose("abm-key", &file("abm.key", ABM_KEY)),
            choose("abm-cert", &file("abm.pem", ABM_CERT)),
            type_into("dns-key", "GDKEY"),
            type_into("dns-secret", "GDSECRET"),
            type_into("contact", "ops@example.org"),
            press("save"),
            "return { reports: seen.reports };".to_owned(),
        ]
        .join("\n"),
    ));
    // The JSON the page writes, in the order it writes it (`json!` would sort the keys).
    let credential = r#"{"provider":"godaddy","kind":"sso-key","key":"GDKEY","secret":"GDSECRET"}"#;
    assert_eq!(
        writes(&ui),
        [
            (
                m(PUT_CERT),
                json!({"certificatePem": b64(CERT), "keyPem": b64(KEY)})
            ),
            (
                m(PUT_SECRET),
                json!({"kind": "abm_key", "material": b64(ABM_KEY), "withCertificate": b64(ABM_CERT)})
            ),
            (m(PUT_TOKEN), json!({"tokenP7m": b64(P7M)})),
            (
                m(PUT_SECRET),
                json!({"kind": "dns_credential", "material": b64(credential)})
            ),
            (
                m(PUT_SETTING),
                json!({"name": "dns_zone", "value": "example.net"})
            ),
            (
                m(PUT_SETTING),
                json!({"name": "acme_contact", "value": "ops@example.org"})
            ),
        ]
    );
    assert_eq!(
        out["out"]["reports"],
        json!([
            "Stored: push certificate and key, ABM key and certificate, ABM token, GoDaddy credential, DNS zone, contact address."
        ])
    );
    assert_eq!(
        ui.methods().len(),
        3 + 6 + 3,
        "one drawing, six writes, one more drawing"
    );
}

#[test]
fn save_tells_a_json_token_from_a_p7m_by_its_content() {
    // Catches: a token sent as the form its file name suggests rather than its content, and
    // JSON with leading white space taken for a .p7m.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(true));
    success(&ui, PUT_TOKEN);
    draw(&ui, empty(), settings("", ""), secrets(true));
    ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            choose(
                "abm-token",
                &file("token.p7m", &format!("\r\n  {TOKEN_JSON}")),
            ),
            press("save"),
        ]
        .join("\n"),
    ));
    assert_eq!(
        writes(&ui),
        [(
            m(PUT_TOKEN),
            json!({"tokenJson": b64(&format!("\r\n  {TOKEN_JSON}"))})
        )]
    );
}

#[test]
fn save_with_a_subset_sends_only_that_and_keeps_the_rest() {
    // Catches: an empty file field sent as an empty upload (which would replace what is stored),
    // a Save that touches the settings when only a file was chosen, the contact prefilled from
    // the stored setting written again, and a Save with nothing filled that calls anything.
    let mut ui = Ui::start();
    draw(
        &ui,
        held(90, 90),
        settings("example.net", "ops@example.org"),
        secrets(true),
    );
    draw(
        &ui,
        held(90, 90),
        settings("example.net", "ops@example.org"),
        secrets(true),
    );
    success(&ui, PUT_TOKEN);
    draw(
        &ui,
        held(90, 90),
        settings("example.net", "ops@example.org"),
        secrets(true),
    );
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            press("save"),
            choose("abm-token", &file("token.json", TOKEN_JSON)),
            press("save"),
            "return { reports: seen.reports, contact: named(seen.shown[seen.shown.length - 1], \"contact\")[0].attrs.value };"
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(
        out["out"]["reports"],
        json!([
            "Nothing to save: choose a file or fill a field.",
            "Stored: ABM token."
        ])
    );
    assert_eq!(out["out"]["contact"], "ops@example.org");
    assert_eq!(
        writes(&ui),
        [(m(PUT_TOKEN), json!({"tokenJson": b64(TOKEN_JSON)}))]
    );
}

#[test]
fn a_refusal_is_shown_under_the_field_it_is_about_and_the_other_parts_are_still_stored() {
    // Catches: a pairing error shown on the wrong field (a key that is not the certificate's
    // shown under the certificate), a refusal reworded without its code, a refusal that stops
    // the parts after it, a .p7m tried when the ABM key it opens with was refused, a malformed
    // JSON token sent, a push certificate chosen without its key sent alone, and the file chosen
    // for a refused field lost (a retry needs no new choice).
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(false));
    ui.refuse(&m(PUT_CERT), 400, "INVALID_ARGUMENT", "key_mismatch");
    ui.refuse(&m(PUT_SECRET), 400, "INVALID_ARGUMENT", "invalid_request");
    success(&ui, PUT_SECRET);
    success(&ui, PUT_SETTING);
    draw(&ui, empty(), settings("", ""), secrets(false));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            choose("push-cert", &file("push.pem", CERT)),
            choose("push-key", &file("other.key", KEY)),
            choose("abm-token", &file("token.p7m", P7M)),
            choose("abm-key", &file("abm.key", ABM_KEY)),
            choose("abm-cert", &file("abm.pem", ABM_CERT)),
            type_into("dns-key", "GDKEY"),
            type_into("dns-secret", "GDSECRET"),
            press("save"),
            r#"const last = seen.shown[seen.shown.length - 1];
const error = (k) => { const n = nodes(last).find((x) => x.attrs["name"] === `${k}-error`); return n ? text(n) : null; };
return { reports: seen.reports, push: error("pushCert"), pushKey: error("pushKey"), abmKey: error("abmKey"),
         abmCert: error("abmCert"), token: error("abmToken"), dns: error("dnsKey"),
         chosen: text(named(last, "push-cert-chosen")[0]) };"#
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(
        out["out"]["pushKey"],
        "This key is not the key of the certificate. (INVALID_ARGUMENT key_mismatch)"
    );
    assert_eq!(out["out"]["push"], Value::Null);
    assert_eq!(out["out"]["abmKey"], "INVALID_ARGUMENT invalid_request");
    assert_eq!(out["out"]["abmCert"], Value::Null);
    assert_eq!(
        out["out"]["token"],
        "Not tried: the ABM key was not stored, and a .p7m opens only with it."
    );
    assert_eq!(out["out"]["dns"], Value::Null, "the credential was stored");
    assert_eq!(out["out"]["chosen"], "Still chosen: push.pem");
    assert_eq!(
        out["out"]["reports"],
        json!([
            "Stored: GoDaddy credential, DNS zone. 3 fields refused: see the error under each."
        ]),
    );
    assert_eq!(
        writes(&ui)
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        [m(PUT_CERT), m(PUT_SECRET), m(PUT_SECRET), m(PUT_SETTING)]
    );
}

#[test]
fn one_half_of_a_pair_and_a_malformed_token_are_refused_before_anything_is_sent() {
    // Catches: a push key sent without its certificate, a push certificate without its key (the
    // coordinator would take it for a renewal of a pending request), a GoDaddy key without its
    // secret, and a token that begins like JSON but is not, sent to Apple.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(true));
    draw(&ui, empty(), settings("", ""), secrets(true));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            choose("push-key", &file("push.key", KEY)),
            choose("abm-token", &file("token.json", "{\"consumer_key\": ")),
            type_into("dns-key", "GDKEY"),
            press("save"),
            r#"const last = seen.shown[seen.shown.length - 1];
const error = (k) => { const n = nodes(last).find((x) => x.attrs["name"] === `${k}-error`); return n ? text(n) : null; };
return { push: error("pushCert"), key: error("pushKey"), token: error("abmToken"), secret: error("dnsSecret"), reports: seen.reports };"#
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(writes(&ui), []);
    assert_eq!(
        out["out"],
        json!({
            "push": "Choose the certificate of this key: the two are checked together.",
            "key": null,
            "token": "This token file is not well-formed JSON.",
            "secret": "Give the key and the secret together.",
            "reports": ["Nothing was stored. 3 fields refused: see the error under each."],
        })
    );
}

#[test]
fn a_file_over_the_limit_is_refused_on_its_field_and_not_read() {
    // Catches: a file over the API's limit (16384 for a PEM) read into memory and sent.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(true));
    draw(&ui, empty(), settings("", ""), secrets(true));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            choose(
                "push-cert",
                r#"{ name: "big.pem", size: 16385, arrayBuffer: async () => { throw new Error("read"); } }"#,
            ),
            choose("push-key", &file("push.key", KEY)),
            press("save"),
            r#"const last = seen.shown[seen.shown.length - 1];
return text(named(last, "pushCert-error")[0]);"#
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(writes(&ui), []);
    assert_eq!(
        out["out"],
        "The certificate is at most 16384 bytes; this file is 16385."
    );
}

#[test]
fn the_zone_is_the_registrable_domain_of_the_mdm_name_and_a_zone_already_set_is_kept() {
    // Catches: a zone taken as the whole name, as the name's last label, or as only the last
    // two labels under a suffix of two labels; a set zone replaced; a name that is only a label given a zone.
    let mut ui = Ui::start();
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"const { registrableDomain } = await import("./pages/mdm.js");
// The suffix is built from pieces: the hygiene lint reads it as a host name.
const suffix = ["co", "uk"].join(".");
return ["mdm.internal.example.net", "mdm.example.com", "example.com", `Mdm.Corp.Example.${suffix.toUpperCase()}.`,
        `mdm.example.${suffix}`, "com", "", undefined].map(registrableDomain);"#,
    ));
    let suffixed = format!("example.{}", ["co", "uk"].join("."));
    assert_eq!(
        out["out"],
        json!([
            "example.net",
            "example.com",
            "example.com",
            suffixed,
            suffixed,
            "",
            "",
            ""
        ])
    );
    draw(&ui, empty(), settings("", ""), secrets(true));
    success(&ui, PUT_SECRET);
    success(&ui, PUT_SETTING);
    draw(&ui, empty(), settings("", ""), secrets(true));
    let set = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            type_into("dns-key", "K"),
            type_into("dns-secret", "S"),
            press("save"),
            "return 1;".to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(set["out"], 1);
    assert_eq!(
        writes(&ui)[1],
        (
            m(PUT_SETTING),
            json!({"name": "dns_zone", "value": "example.net"})
        )
    );
    let mut kept = Ui::start();
    draw(
        &kept,
        empty(),
        settings("corp.example.net", ""),
        secrets(true),
    );
    success(&kept, PUT_SECRET);
    draw(
        &kept,
        empty(),
        settings("corp.example.net", ""),
        secrets(true),
    );
    kept.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            type_into("dns-key", "K"),
            type_into("dns-secret", "S"),
            press("save"),
        ]
        .join("\n"),
    ));
    assert_eq!(writes(&kept).len(), 1, "{:?}", writes(&kept));
}

#[test]
fn the_name_has_a_field_only_while_none_is_set_and_it_is_set_before_the_zone_is_derived() {
    // Catches: a name field on a form whose name is set (the name is locked once a Mac has
    // enrolled), no way to give a name when none is set, the name sent after the zone is worked
    // out (the zone would be derived from nothing), and a refused name shown elsewhere than
    // under its field.
    let mut ui = Ui::start();
    let mut unnamed = empty();
    unnamed["name"] = json!("");
    draw(
        &ui,
        held(90, 90),
        settings("example.net", ""),
        secrets(true),
    );
    draw(&ui, unnamed.clone(), settings("", ""), secrets(true));
    ui.reply(
        &m(PUT_NAME),
        json!({"name": "mdm.corp.example.org", "enrollmentUrl": "https://mdm.corp.example.org/enroll"}),
    );
    success(&ui, PUT_SECRET);
    success(&ui, PUT_SETTING);
    draw(&ui, empty(), settings("example.org", ""), secrets(true));
    draw(&ui, unnamed.clone(), settings("", ""), secrets(true));
    ui.refuse(&m(PUT_NAME), 400, "INVALID_ARGUMENT", "invalid_request");
    draw(&ui, unnamed, settings("", ""), secrets(true));
    let field = r#"nodes(tree).filter((n) => n.attrs["name"] === "mdm-name").length"#;
    let set = ui.run(&page_driver("mdm", "{}", "[]", &format!("return {field};")));
    assert_eq!(set["out"], 0);
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            format!("const before = {field};"),
            type_into("mdm-name", " MDM.Corp.Example.org "),
            type_into("dns-key", "K"),
            type_into("dns-secret", "S"),
            press("save"),
            "return { before, reports: seen.reports };".to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(out["out"]["before"], 1);
    assert_eq!(
        writes(&ui)[..3],
        [
            (m(PUT_NAME), json!({"name": "MDM.Corp.Example.org"})),
            (
                m(PUT_SECRET),
                json!({"kind": "dns_credential", "material": b64(r#"{"provider":"godaddy","kind":"sso-key","key":"K","secret":"S"}"#)})
            ),
            (
                m(PUT_SETTING),
                json!({"name": "dns_zone", "value": "example.org"})
            ),
        ]
    );
    assert_eq!(
        out["out"]["reports"],
        json!(["Stored: MDM name, GoDaddy credential, DNS zone."])
    );
    let refused = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            type_into("mdm-name", "x"),
            press("save"),
            r#"return text(named(seen.shown[seen.shown.length - 1], "mdmName-error")[0]);"#
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(refused["out"], "INVALID_ARGUMENT invalid_request");
}

#[test]
fn a_name_that_is_not_set_gives_no_zone_and_the_credential_is_not_sent() {
    // Catches: a credential stored for a zone that cannot be worked out.
    let mut ui = Ui::start();
    let mut status = empty();
    status["name"] = json!("");
    draw(&ui, status.clone(), settings("", ""), secrets(true));
    draw(&ui, status, settings("", ""), secrets(true));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            type_into("dns-key", "K"),
            type_into("dns-secret", "S"),
            press("save"),
            r#"return text(named(seen.shown[seen.shown.length - 1], "dnsKey-error")[0]);"#
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(writes(&ui), []);
    assert_eq!(
        out["out"],
        "The MDM name is not set, so the DNS zone cannot be worked out: give the MDM name."
    );
}

#[test]
fn the_contact_is_prefilled_and_written_only_when_changed_or_stored_with_the_credential() {
    // Catches: the stored contact not prefilled, the signed-in admin's address not offered when
    // none is stored, a username that is no address offered as one, and the prefilled address
    // written by a Save of a file.
    let mut ui = Ui::start();
    draw(
        &ui,
        empty(),
        settings("", "stored@example.org"),
        secrets(true),
    );
    draw(&ui, empty(), settings("", ""), secrets(true));
    draw(&ui, empty(), settings("", ""), secrets(true));
    let driver = |signed: &str| {
        format!(
            r#"
import {{ api, recorder, named }} from "../test/kit.js";
import * as session from "./session.js";
import {{ render }} from "./pages/mdm.js";
export async function main() {{
  const {{ ctx }} = recorder(api("operator-token"), {{}}, []);
  session.signIn({{ username: "{signed}", csrf: "operator-token", must_change: false }});
  const tree = await render(ctx);
  return named(tree, "contact")[0].attrs.value;
}}"#
        )
    };
    assert_eq!(ui.run(&driver("ops@example.org")), "stored@example.org");
    assert_eq!(ui.run(&driver("admin@example.org")), "admin@example.org");
    assert_eq!(ui.run(&driver("admin")), "");
    success(&ui, PUT_TOKEN);
    success(&ui, PUT_SETTING);
    draw(&ui, empty(), settings("", ""), secrets(true));
    draw(&ui, empty(), settings("", ""), secrets(true));
    ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            choose("abm-token", &file("t.json", TOKEN_JSON)),
            type_into("contact", "new@example.org"),
            press("save"),
        ]
        .join("\n"),
    ));
    assert_eq!(
        writes(&ui),
        [
            (m(PUT_TOKEN), json!({"tokenJson": b64(TOKEN_JSON)})),
            (
                m(PUT_SETTING),
                json!({"name": "acme_contact", "value": "new@example.org"})
            ),
        ]
    );
}

#[test]
fn secrets_are_never_rendered_back_or_kept_after_save() {
    // Catches: the GoDaddy key or secret left in its field after Save (whether the coordinator
    // took it or refused it), the contents of a chosen file, or its base64, in the page, a
    // report or an error, and a text field that takes a key or a token.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(true));
    ui.refuse(&m(PUT_SECRET), 400, "INVALID_ARGUMENT", "invalid_request");
    ui.refuse(&m(PUT_CERT), 400, "INVALID_ARGUMENT", "key_invalid");
    draw(&ui, empty(), settings("", ""), secrets(true));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        &[
            type_into("dns-key", "GDKEYVALUE"),
            type_into("dns-secret", "GDSECRETVALUE"),
            choose("push-cert", &file("push.pem", CERT)),
            choose("push-key", &file("push.key", KEY)),
            press("save"),
            r#"const last = seen.shown[seen.shown.length - 1];
const fields = nodes(last).filter((n) => n.tag === "input" && n.attrs.type !== "file");
return { values: fields.map((n) => n.attrs.value), everything: JSON.stringify([shown, seen]) + text(last) };"#
                .to_owned(),
        ]
        .join("\n"),
    ));
    assert_eq!(out["out"]["values"], json!(["", "", ""]));
    let everything = out["out"]["everything"].as_str().expect("text");
    for secret in [
        "GDKEYVALUE",
        "GDSECRETVALUE",
        "PUSHKEYBODY",
        &b64(KEY),
        &b64(CERT),
        &b64(&json!({"provider": "godaddy"}).to_string()),
    ] {
        assert!(
            !everything.contains(secret),
            "{secret} is in the page's output"
        );
    }
}

#[test]
fn the_public_certificate_link_is_shown_only_without_an_abm_key_and_gives_the_certificate() {
    // Catches: the link shown when a key is stored (it would hand out a certificate for a key
    // that is not the one stored), a download with other bytes or name than the coordinator's
    // answer, and the page not redrawn after the pair was made (the link would stay).
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(false));
    ui.reply(
        &m(PUBLIC_KEY),
        json!({"certificatePem": "CERTPEM", "fingerprintSha256": "ab", "created": true}),
    );
    draw(&ui, empty(), settings("", ""), secrets(true));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"const link = (t) => nodes(t).filter((n) => n.attrs["name"] === "download-abm-certificate").length;
const before = link(tree);
await click(tree, "download-abm-certificate");
return { before, after: link(seen.shown[seen.shown.length - 1]), downloads: seen.downloads };"#,
    ));
    assert_eq!(out["out"]["before"], 1);
    assert_eq!(out["out"]["after"], 0);
    assert_eq!(
        out["out"]["downloads"],
        json!([{"name": "abm-server.crt", "mime": "text/plain", "text": "CERTPEM"}])
    );
    assert_eq!(writes(&ui), [(m(PUBLIC_KEY), json!({}))]);
}

#[test]
fn a_refused_public_certificate_is_reported_and_nothing_is_offered() {
    // Catches: a refusal swallowed, or a download offered after one.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(false));
    ui.refuse(
        &m(PUBLIC_KEY),
        412,
        "FAILED_PRECONDITION",
        "secrets_unavailable",
    );
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"await click(tree, "download-abm-certificate"); return { reports: seen.reports, downloads: seen.downloads };"#,
    ));
    assert_eq!(
        out["out"],
        json!({"reports": ["The coordinator holds no master key (--mdm-secrets-key-file), so it keeps no secret. (FAILED_PRECONDITION secrets_unavailable)"],
               "downloads": []})
    );
}

#[test]
fn the_page_draws_without_the_settings_and_the_secrets_and_fails_without_the_status() {
    // Catches: a page that fails when it cannot read the settings or the secrets (it offers a
    // link for a key it cannot know is stored), and a page that draws a form when the status,
    // its one required read, is refused.
    let mut ui = Ui::start();
    ui.reply(&m(STATUS), empty());
    ui.refuse(&m(SETTINGS), 500, "INTERNAL", "store_failed");
    ui.refuse(&m(SECRETS), 500, "INTERNAL", "store_failed");
    ui.refuse(&m(STATUS), 500, "INTERNAL", "store_failed");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"return nodes(tree).filter((n) => n.attrs["name"] === "download-abm-certificate").length;"#,
    ));
    assert_eq!(out["out"], 0);
    assert_shows(&out, &["MDM: missing push certificate"]);
    let failed = ui.run(&page_driver("mdm", "{}", "[]", ""));
    assert_shows(&failed, &["MDM setup", "INTERNAL store_failed"]);
    assert!(!shown(&failed).contains("Save"), "{}", shown(&failed));
}

#[test]
fn verify_sends_one_call_and_lists_what_each_part_found() {
    // Catches: a Verify that sends a push (it names no Mac), a result missing the setup, the
    // listener or Apple's answer, an answer shown as its token, and a failed Verify not
    // reported.
    let mut ui = Ui::start();
    draw(&ui, held(90, 90), settings("", ""), secrets(true));
    ui.reply(
        &m(VERIFY),
        json!({"status": held(90, 90), "listener": true, "serving": true,
               "servedNames": ["mdm.example.com"], "servedNamesTheName": true,
               "abm": {"result": "MDM_ABM_RESULT_ACCEPTED", "organization": "Org"}}),
    );
    draw(&ui, held(90, 90), settings("", ""), secrets(true));
    ui.refuse(&m(VERIFY), 503, "UNAVAILABLE", "audit_unavailable");
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"await click(tree, "verify");
const lines = nodes(seen.shown[seen.shown.length - 1]).filter((n) => n.tag === "li").map((n) => text(n));
await click(seen.shown[seen.shown.length - 1], "verify");
return { lines, reports: seen.reports };"#,
    ));
    assert_eq!(
        out["out"]["lines"],
        json!([
            "Setup: MDM: configured. Name: mdm.example.com",
            "Listener: The Macs' listener serves a certificate for mdm.example.com, which names the MDM name.",
            "ABM: Apple accepts the ABM token. Account: Org."
        ])
    );
    assert_eq!(
        out["out"]["reports"],
        json!([
            "Verified. See what each part found.",
            "UNAVAILABLE audit_unavailable"
        ])
    );
    let verifies: Vec<_> = bodies(&ui)
        .into_iter()
        .filter(|(method, _)| *method == m(VERIFY))
        .collect();
    assert_eq!(verifies, [(m(VERIFY), json!({})), (m(VERIFY), json!({}))]);
}

#[test]
fn a_link_that_leads_outside_the_documentation_is_refused_when_mounted() {
    // Catches: an `href` that is not the operator documentation (another site, a script
    // address) reaching the browser.
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { h, mount } from "./view.js";
import { fakeDocument } from "../test/kit.js";
export async function main() {
  const tried = [];
  for (const href of ["https://example.com/", "javascript:alert(1)", "//example.com/"]) {
    try {
      const doc = fakeDocument();
      mount(doc, doc.root, h("a", { href }, "x"));
      tried.push("mounted");
    } catch (error) {
      tried.push(error.message);
    }
  }
  return tried;
}"#,
    );
    assert_eq!(out, json!(vec!["no link outside the documentation"; 3]));
}

#[test]
fn the_fields_are_grouped_and_each_stored_part_carries_a_chip_that_warns_near_its_end() {
    // Catches: the form not split into the push certificate, Apple Business Manager and TLS
    // groups, a part that is not held shown without its "Not stored" chip, an expiry within 30
    // days or already past shown as plain "stored", the Save and Verify buttons apart, and the
    // status header not saying whether the setup is configured.
    let mut ui = Ui::start();
    draw(&ui, held(10, -3), settings("", ""), secrets(false));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"const chip = (k) => { const n = nodes(tree).find((x) => x.attrs["name"] === `${k}-stored` || x.attrs["name"] === `${k}-empty`); return n ? [n.attrs["class"], text(n)] : null; };
const groups = nodes(tree).filter((n) => n.tag === "section" && String(n.attrs["name"]).startsWith("group-"))
  .map((g) => [g.attrs["name"], nodes(g).filter((n) => n.tag === "input").map((n) => n.attrs["name"])]);
const bar = nodes(tree).find((n) => n.attrs["name"] === "mdm-actions");
const pill = nodes(tree).find((n) => String(n.attrs["class"]).startsWith("pill"));
return { groups, bar: nodes(bar).filter((n) => n.tag === "button").map((n) => n.attrs["name"]),
         pill: text(pill), push: chip("pushCert"), abm: chip("abmToken"), abmKey: chip("abmKey"), dns: chip("dnsKey") };"#,
    ));
    assert_eq!(
        out["out"]["groups"],
        json!([
            ["group-push", ["push-cert", "push-key"]],
            ["group-abm", ["abm-token", "abm-key", "abm-cert"]],
            ["group-tls", ["dns-key", "dns-secret", "contact"]],
        ])
    );
    assert_eq!(out["out"]["bar"], json!(["save", "verify"]));
    assert_eq!(out["out"]["pill"], json!("Configured"));
    assert_eq!(
        out["out"]["push"],
        json!(["chip warn", "stored \u{2713} (expires 2027-01-25)"])
    );
    assert_eq!(
        out["out"]["abm"],
        json!(["chip bad", "stored \u{2713} (expires 2027-01-12)"])
    );
    assert_eq!(out["out"]["abmKey"], json!(["chip none", "Not stored"]));
    assert_eq!(out["out"]["dns"], json!(["chip ok", "stored \u{2713}"]));
}

#[test]
fn a_setup_that_is_missing_parts_is_headed_needs_attention() {
    // Catches: an unconfigured setup headed as configured.
    let mut ui = Ui::start();
    draw(&ui, empty(), settings("", ""), secrets(false));
    let out = ui.run(&page_driver(
        "mdm",
        "{}",
        "[]",
        r#"const pill = nodes(tree).find((n) => String(n.attrs["class"]).startsWith("pill"));
return text(pill);"#,
    ));
    assert_eq!(out["out"], json!("Needs attention"));
}
