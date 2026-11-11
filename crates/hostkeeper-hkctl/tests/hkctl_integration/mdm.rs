//! The `Mdm` setup and push-certificate calls, as the real script sends them: the path each
//! posts to and the body curl reads, the exit code and the answer printed, and the refusal
//! `session_required` the gateway answers a bearer token's write with, printed as any refusal.

use crate::gateway;
use crate::rig::{Rig, Script, args};

/// `words` after the global flags of a call to a coordinator at a port nothing listens on: the
/// fake curl answers from a canned reply and the test reads what it was sent.
fn call(rig: &Rig, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(1);
    all.extend(args(words));
    all
}

/// The target and the body of the only `curl` call the rig recorded.
fn posted(rig: &Rig) -> (String, String) {
    let curls = rig.curls();
    assert_eq!(curls.len(), 1, "{curls:#?}");
    let url = curls[0]
        .argv
        .iter()
        .skip_while(|a| *a != "--url")
        .nth(1)
        .expect("a url")
        .clone();
    let body = curls[0]
        .read_after("--data-binary", 0)
        .expect("the body is read from a pipe");
    (url, body)
}

/// `words` after the global flags of a call to the real gateway at `port`.
fn call_at(rig: &Rig, port: u16, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(port);
    all.extend(args(words));
    all
}

fn url(method: &str) -> String {
    format!("https://127.0.0.1:1/hostkeeper.operator.v1.Mdm/{method}")
}

/// Proves: the two reads of the setup post an empty request to their methods, exit 0, and print
/// the coordinator's answer on stdout as it came, never a secret's value.
#[test]
fn list_secrets_and_list_settings_post_an_empty_request_and_print_the_answer() {
    for (words, method, answer) in [
        (
            &["mdm", "list-secrets"][..],
            "ListSecrets",
            "{\"secrets\":[{\"kind\":\"push_key\",\"version\":1,\"fingerprintSha256\":\"ab\"}],\"sealing\":true}",
        ),
        (
            &["mdm", "list-settings"][..],
            "ListSettings",
            "{\"settings\":[{\"name\":\"secret_grace_days\",\"value\":\"7\",\"isDefault\":true}]}",
        ),
    ] {
        let rig = Rig::new("mdm-reads");
        rig.reply(200, answer);
        let ran = rig.run(Script::Real, &call(&rig, words), b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{method}");
        assert_eq!(ran.stdout.trim_end(), answer, "{method}");
        assert_eq!(posted(&rig), (url(method), "{}".to_owned()), "{method}");
    }
}

/// Proves: `put-setting` posts exactly its name and value, `put-secret` its kind and expiry
/// (the 64-bit number as the JSON text of a string), `create-push-request` its email and
/// `submit` flag, each to its own method, and each prints the answer and exits 0.
#[test]
fn the_calls_with_flags_post_exactly_their_fields_to_their_method() {
    for (words, method, body, answer) in [
        (
            &[
                "mdm",
                "put-setting",
                "--name",
                "secret_grace_days",
                "--value",
                "60,14",
            ][..],
            "PutSetting",
            "{\"name\":\"secret_grace_days\",\"value\":\"60,14\"}",
            "{\"name\":\"secret_grace_days\",\"value\":\"60,14\"}",
        ),
        (
            &[
                "mdm",
                "put-secret",
                "--kind",
                "abm_token",
                "--not-after-unix",
                "1800000000",
            ][..],
            "PutSecret",
            "{\"kind\":\"abm_token\",\"notAfterUnix\":\"1800000000\"}",
            "{\"kind\":\"abm_token\",\"version\":2}",
        ),
        (
            &[
                "mdm",
                "create-push-request",
                "--email",
                "ops@example.com",
                "--submit",
                "true",
            ][..],
            "CreatePushRequest",
            "{\"email\":\"ops@example.com\",\"submit\":true}",
            "{\"csrPem\":\"c\",\"encryptionCertificatePem\":\"e\",\"submitted\":true}",
        ),
    ] {
        let rig = Rig::new("mdm-flags");
        rig.reply(200, answer);
        let ran = rig.run(Script::Real, &call(&rig, words), b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{method}");
        assert_eq!(ran.stdout.trim_end(), answer, "{method}");
        assert_eq!(posted(&rig), (url(method), body.to_owned()), "{method}");
    }
}

/// Proves: the bytes fields `material`, `reply`, `certificate_pem` and `key_pem`, which have no
/// flag, reach the gateway through `--request` as the base64 text the file holds, and the
/// answers print on stdout.
#[test]
fn the_calls_with_bytes_take_them_through_the_request_file() {
    for (method, word, request, answer) in [
        (
            "PutSecret",
            "put-secret",
            "{\"kind\":\"push_key\",\"material\":\"aGk=\"}",
            "{\"kind\":\"push_key\",\"version\":1}",
        ),
        (
            "PutPushReply",
            "put-push-reply",
            "{\"reply\":\"AAEC\"}",
            "{\"signedRequest\":\"AAEC\"}",
        ),
        (
            "PutPushCertificate",
            "put-push-certificate",
            "{\"certificatePem\":\"Yw==\",\"keyPem\":\"aw==\"}",
            "{\"topic\":\"com.apple.mgmt.External.x\",\"renewal\":false}",
        ),
    ] {
        let rig = Rig::new("mdm-bytes");
        rig.reply(200, answer);
        let file = rig.put("request.json", request.as_bytes(), 0o600);
        let ran = rig.run(
            Script::Real,
            &call(&rig, &["mdm", word, "--request", &file]),
            b"",
            &[],
        );
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{method}");
        assert_eq!(ran.stdout.trim_end(), answer, "{method}");
        let (target, body) = posted(&rig);
        assert_eq!(target, url(method));
        assert_eq!(body.replace(' ', ""), request, "{method}");
    }
}

/// Proves: a bearer token's call of each session-only write is refused by the gateway with
/// `PERMISSION_DENIED` and the reason `session_required`, which `hkctl` prints on a `refused:`
/// line and exits 3 for, with nothing on stdout.
#[test]
fn a_session_only_write_refused_session_required_exits_3_and_prints_the_reason() {
    for word in [
        "put-secret",
        "put-setting",
        "create-push-request",
        "put-push-reply",
        "put-push-certificate",
    ] {
        let rig = Rig::new("mdm-session");
        rig.reply(
            403,
            "{\"code\":\"PERMISSION_DENIED\",\"reason\":\"session_required\"}",
        );
        let request = rig.put("request.json", b"{}", 0o600);
        let ran = rig.run(
            Script::Real,
            &call(&rig, &["mdm", word, "--request", &request]),
            b"",
            &[],
        );
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (
                3,
                "",
                "hkctl: the coordinator answered PERMISSION_DENIED\nrefused: session_required\n"
            ),
            "{word}"
        );
    }
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
        &call_at(
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
        &call_at(
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
        &call_at(&rig, gateway.port, &["mdm", "list-settings"]),
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
            ("installs", "[]", true),
            ("proxy_wait_s", "10", true),
            ("proxy_redirects", "3", true),
            ("proxy_index_ttl_s", "300", true),
            ("proxy_cache_max_bytes", "21474836480", true),
            ("proxy_fetches", "8", true),
            ("abm_sync_s", "300", true),
            ("abm_request_timeout_s", "30", true),
            ("abm_retries", "3", true),
            ("ade_profile_name", "Komira MDM", true),
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
        &call_at(&rig, gateway.port, &["mdm", "list-secrets"]),
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
