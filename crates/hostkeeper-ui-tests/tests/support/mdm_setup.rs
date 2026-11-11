//! What the tests of the MDM setup page share: the clock the page reads, the methods it calls and
//! the answers of a setup that holds its secrets.

use serde_json::{Value, json};

use super::Ui;
use super::pages::m;

/// The time the kit's page context reads.
pub const NOW: i64 = 1_800_000_000;
pub const DAY: i64 = 86_400;

pub const STATUS: &str = "Mdm/Status";
pub const SETTINGS: &str = "Mdm/ListSettings";
pub const CREATE: &str = "Mdm/CreatePushRequest";
pub const REPLY: &str = "Mdm/PutPushReply";
pub const CERTIFICATE: &str = "Mdm/PutPushCertificate";
pub const PUT_SECRET: &str = "Mdm/PutSecret";
pub const PUT_SETTING: &str = "Mdm/PutSetting";

pub fn info(kind: &str) -> Value {
    json!({"kind": kind, "version": 2, "fingerprintSha256": "ab".repeat(32),
           "createdUnix": "1799000000"})
}

/// A `Mdm.Status` answer of a setup with a push pair that expires in `push_days`, an ABM token in
/// `abm_days` and a TLS pair in `tls_days`.
pub fn held(push_days: i64, abm_days: i64, tls_days: i64) -> Value {
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

pub fn settings() -> Value {
    json!({"settings": [
        {"name": "push_alert_days", "value": "60,14", "isDefault": true},
        {"name": "abm_alert_days", "value": "60", "isDefault": true},
        {"name": "tls_renew_days", "value": "30", "isDefault": true},
    ]})
}

pub const CERT_TEXT: &str = "-----BEGIN CERTIFICATE-----\\nAA\\n-----END CERTIFICATE-----\\n";
pub const CERT_B64: &str =
    "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCkFBCi0tLS0tRU5EIENFUlRJRklDQVRFLS0tLS0K";
pub const KEY_TEXT: &str =
    "-----BEGIN RSA PRIVATE KEY-----\\nSECRETKEYBODY\\n-----END RSA PRIVATE KEY-----\\n";
pub const KEY_B64: &str = "LS0tLS1CRUdJTiBSU0EgUFJJVkFURSBLRVktLS0tLQpTRUNSRVRLRVlCT0RZCi0tLS0tRU5EIFJTQSBQUklWQVRFIEtFWS0tLS0tCg==";

/// Scripts one drawing of the page: the status and the settings.
pub fn draw(ui: &Ui, status: Value) {
    ui.reply(&m(STATUS), status);
    ui.reply(&m(SETTINGS), settings());
}

/// A file as the browser hands one over: its name, size and bytes.
pub fn file(name: &str, bytes: &str) -> String {
    format!(
        r#"{{ name: "{name}", size: {bytes}.length, arrayBuffer: async () => Uint8Array.from({bytes}).buffer }}"#
    )
}
