//! The `Mdm` setup and push-certificate calls, as the real script sends them: the path each
//! posts to and the body curl reads, the exit code and the answer printed, and the refusal
//! `session_required` the gateway answers a bearer token's write with, printed as any refusal.

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
