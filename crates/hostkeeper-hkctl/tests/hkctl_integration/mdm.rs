//! The `Mdm` setup commands (`mdm put-secret`, `put-setting`, `list-secrets`, `list-settings`) as
//! the real script runs them against the real gateway: the path and the JSON body each posts, the
//! `session_required` refusal of a write made with a bearer token, and the answer of each read.

use crate::gateway;
use crate::rig::{Rig, Script, args};

/// `words` after the global flags of a call to `port`.
fn call(rig: &Rig, port: u16, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(port);
    all.extend(args(words));
    all
}

/// Proves: `mdm put-setting` posts to `Mdm/PutSetting` with exactly `name` and `value` in its
/// body, and the coordinator refuses it `PERMISSION_DENIED` `session_required` because the
/// call carries a bearer token: `hkctl` exits 3, prints the code and the `refused:` line, and
/// nothing on stdout.
#[test]
fn put_setting_posts_its_fields_and_a_bearer_token_is_refused_session_required() {
    let rig = Rig::new("mdm-put-setting");
    let gateway = gateway::start(&rig.path(""));
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &[
                "mdm",
                "put-setting",
                "--name",
                "push_alert_days",
                "--value",
                "60,14",
            ],
        ),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].target(), "/hostkeeper.operator.v1.Mdm/PutSetting");
    assert_eq!(
        String::from_utf8_lossy(&seen[0].body),
        "{\"name\":\"push_alert_days\",\"value\":\"60,14\"}"
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            3,
            "",
            "hkctl: the coordinator answered PERMISSION_DENIED\nrefused: session_required\n"
        )
    );
}

/// Proves: `mdm put-secret` with a request file posts to `Mdm/PutSecret` the file's `material`
/// (bytes, so base64) with the `--kind` and `--not-after-unix` flags (a 64-bit integer, so
/// written as a string), and a bearer token is refused `session_required` (exit 3).
#[test]
fn put_secret_posts_kind_material_and_expiry_and_is_refused_session_required() {
    let rig = Rig::new("mdm-put-secret");
    let gateway = gateway::start(&rig.path(""));
    let request = rig.put("secret.json", b"{\"material\":\"AAEC\"}", 0o644);
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &[
                "mdm",
                "put-secret",
                "--request",
                &request,
                "--kind",
                "push_cert",
                "--not-after-unix",
                "1900000000",
            ],
        ),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].target(), "/hostkeeper.operator.v1.Mdm/PutSecret");
    assert_eq!(
        String::from_utf8_lossy(&seen[0].body),
        "{\"material\":\"AAEC\",\"kind\":\"push_cert\",\"notAfterUnix\":\"1900000000\"}"
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            3,
            "",
            "hkctl: the coordinator answered PERMISSION_DENIED\nrefused: session_required\n"
        )
    );
}

/// Proves: `mdm list-settings` posts an empty request to `Mdm/ListSettings` and prints the
/// settings the gateway answered, each at its default on a fresh coordinator, with exit 0.
#[test]
fn list_settings_prints_every_setting_at_its_default() {
    let rig = Rig::new("mdm-list-settings");
    let gateway = gateway::start(&rig.path(""));
    let ran = rig.run(
        Script::Real,
        &call(&rig, gateway.port, &["mdm", "list-settings"]),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].target(), "/hostkeeper.operator.v1.Mdm/ListSettings");
    assert_eq!(String::from_utf8_lossy(&seen[0].body), "{}");
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let answer: serde_json::Value = serde_json::from_str(&ran.stdout).expect("a JSON answer");
    let settings = answer["settings"].as_array().expect("the settings");
    let named: Vec<(&str, &str, bool)> = settings
        .iter()
        .map(|s| {
            (
                s["name"].as_str().expect("a name"),
                s["value"].as_str().expect("a value"),
                s["isDefault"].as_bool().expect("is_default"),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            ("push_alert_days", "60,14", true),
            ("abm_alert_days", "60", true),
            ("tls_renew_days", "30", true),
            ("secret_grace_days", "7", true),
            ("push_retry_s", "3600", true),
            ("push_expiry_s", "86400", true),
            ("push_priority", "10", true),
            ("push_timeout_s", "30", true),
        ]
    );
}

/// Proves: `mdm list-secrets` posts an empty request to `Mdm/ListSecrets` and, on a coordinator
/// that holds no master key and no secret, prints an answer with no secret, with exit 0.
#[test]
fn list_secrets_prints_an_answer_that_holds_no_secret_without_a_master_key() {
    let rig = Rig::new("mdm-list-secrets");
    let gateway = gateway::start(&rig.path(""));
    let ran = rig.run(
        Script::Real,
        &call(&rig, gateway.port, &["mdm", "list-secrets"]),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].target(), "/hostkeeper.operator.v1.Mdm/ListSecrets");
    assert_eq!(String::from_utf8_lossy(&seen[0].body), "{}");
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let answer: serde_json::Value = serde_json::from_str(&ran.stdout).expect("a JSON answer");
    assert!(
        answer["secrets"].as_array().is_none_or(Vec::is_empty),
        "{answer}"
    );
}
