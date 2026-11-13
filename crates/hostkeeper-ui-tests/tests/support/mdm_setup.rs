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
pub const SECRETS: &str = "Mdm/ListSecrets";
pub const PUT_NAME: &str = "Mdm/PutName";
pub const PUT_CERT: &str = "Mdm/PutPushCertificate";
pub const PUT_SECRET: &str = "Mdm/PutSecret";
pub const PUT_SETTING: &str = "Mdm/PutSetting";
pub const PUT_TOKEN: &str = "Mdm/PutAbmToken";
pub const PUBLIC_KEY: &str = "Mdm/GetAbmPublicKey";
pub const VERIFY: &str = "Mdm/Verify";

pub fn info(kind: &str) -> Value {
    json!({"kind": kind, "version": 2, "fingerprintSha256": "ab".repeat(32),
           "createdUnix": "1799000000"})
}

/// A `Mdm.Status` answer of a setup that holds everything, the push pair ending in `push_days`
/// and the ABM token in `abm_days`.
pub fn held(push_days: i64, abm_days: i64) -> Value {
    json!({
        "configured": true, "sealing": true,
        "name": "mdm.example.com", "enrollmentUrl": "https://mdm.example.com/enroll",
        "pushCert": info("push_cert"), "pushKey": info("push_key"),
        "abmToken": info("abm_token"), "dnsCredential": info("dns_credential"),
        "tlsCert": info("tls_cert"), "tlsKey": info("tls_key"),
        "pushNotAfterUnix": (NOW + push_days * DAY).to_string(),
        "abmNotAfterUnix": (NOW + abm_days * DAY).to_string(),
    })
}

/// A `Mdm.Status` answer of a coordinator that holds nothing but its name.
pub fn empty() -> Value {
    json!({
        "configured": false, "sealing": true, "name": "mdm.internal.example.net",
        "missing": ["MDM_CONDITION_PUSH_CERT_MISSING", "MDM_CONDITION_PUSH_KEY_MISSING",
                    "MDM_CONDITION_ABM_TOKEN_MISSING", "MDM_CONDITION_TLS_CERT_MISSING"],
    })
}

/// A `Mdm.ListSettings` answer holding `zone` and `contact`.
pub fn settings(zone: &str, contact: &str) -> Value {
    json!({"settings": [
        {"name": "dns_zone", "value": zone, "isDefault": zone.is_empty()},
        {"name": "acme_contact", "value": contact, "isDefault": contact.is_empty()},
        {"name": "acme_directory", "value": "https://acme-v02.api.letsencrypt.org/directory", "isDefault": true},
    ]})
}

/// A `Mdm.ListSecrets` answer holding the ABM pair when `abm` is true, and a superseded ABM key.
pub fn secrets(abm: bool) -> Value {
    let mut list = vec![json!({"kind": "abm_key", "version": 1, "supersededUnix": "1799000000"})];
    if abm {
        list.push(info("abm_key"));
        let mut cert = info("abm_cert");
        cert["notAfterUnix"] = json!((NOW + 400 * DAY).to_string());
        list.push(cert);
    }
    json!({"secrets": list, "sealing": true})
}

/// Scripts one drawing of the page.
pub fn draw(ui: &Ui, status: Value, settings: Value, secrets: Value) {
    ui.reply(&m(STATUS), status);
    ui.reply(&m(SETTINGS), settings);
    ui.reply(&m(SECRETS), secrets);
}

/// A file as the browser hands one over: its name, size and the bytes of `content`.
pub fn file(name: &str, content: &str) -> String {
    let bytes: Vec<String> = content.bytes().map(|b| b.to_string()).collect();
    format!(
        r#"{{ name: "{name}", size: {n}, arrayBuffer: async () => Uint8Array.from([{list}]).buffer }}"#,
        n = bytes.len(),
        list = bytes.join(", ")
    )
}

/// JavaScript that chooses `file` in the control `name` of the page last drawn.
pub fn choose(name: &str, file: &str) -> String {
    format!(
        r#"named(seen.shown.length ? seen.shown[seen.shown.length - 1] : tree, "{name}")[0].on.change({{ target: {{ files: [{file}] }} }});"#
    )
}

/// JavaScript that types `value` into the field `name` of the page last drawn.
pub fn type_into(name: &str, value: &str) -> String {
    format!(
        r#"type(seen.shown.length ? seen.shown[seen.shown.length - 1] : tree, "{name}", "{value}");"#
    )
}

/// JavaScript that clicks the button `name` of the page last drawn.
pub fn press(name: &str) -> String {
    format!(
        r#"await click(seen.shown.length ? seen.shown[seen.shown.length - 1] : tree, "{name}");"#
    )
}

/// `text` as standard base64 with padding, as the API's canonical JSON writes bytes.
pub fn b64(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
