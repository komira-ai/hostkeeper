//! `dep get-cert`, `upload-token` and `assign`, and the checks every `dep` verb makes first,
//! on the fake host and the fake API.

mod common;
mod dep_support;

use common::{FakeApi, SVC, changes};
use dep_support::{KEY, NANODEP_ENV, UUID, UUID_FILE, certificate, dep_host, go};
use hostkeeper_mdm::api::Method;
use hostkeeper_mdm::dep::{Dep, DepError, Serial};
use hostkeeper_mdm::{MdmError, secret};
use hostkeeper_mdm_core::profile::ProfileError;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{DRY_RUN_NOTE, Host, Mode};

const CERT_PATH: &str = "/v1/tokenpki/example?cn=hostkeeper-mdm&validity_days=7";
const OUT: &str = "/ops/nanodep.crt";
const TOKEN: &str = "/ops/token.p7m";
const NEXT: &str = "upload it in Apple Business Manager (your MDM server, Upload Public Key), then download the server token (.p7m)";

fn get_cert() -> Dep {
    Dep::GetCert {
        out: OUT.to_owned(),
    }
}

fn upload() -> Dep {
    Dep::UploadToken {
        token: TOKEN.to_owned(),
    }
}

fn assign(serials: &[&str]) -> Dep {
    Dep::Assign {
        serials: serials.iter().map(|s| Serial::new(s).unwrap()).collect(),
    }
}

fn ops_host() -> FakeHost {
    let host = dep_host();
    host.put_dir("/ops", 0o755, (1000, 1000));
    host
}

fn refused(result: Result<bool, MdmError>) -> MdmError {
    result.expect_err("the verb refuses")
}

/// Catches a dry run of get-cert that calls NanoDEP or writes: it prints the call and the
/// write, makes no call, and leaves the host as it was.
#[test]
fn get_cert_dry_run_asks_nothing_and_writes_nothing() {
    let host = ops_host();
    let api = FakeApi::new();
    let before = host.paths();
    let (result, shown) = go(&host, &api, &get_cert(), Mode::DryRun);
    assert!(result.unwrap());
    assert_eq!(
        shown,
        format!(
            "DRY-RUN: GET http://127.0.0.1:9001{CERT_PATH} (NanoDEP API)\n\
             DRY-RUN: write {OUT} (0644, the certificate NanoDEP answers)\n\
             {DRY_RUN_NOTE}\n"
        )
    );
    assert!(api.sent().is_empty());
    assert_eq!(host.paths(), before);
}

/// Catches a get-cert that does not write what NanoDEP answers: with the answer a PEM
/// certificate the file is root's 0644 with those bytes, the call carries the key, and the
/// next step is printed.
#[test]
fn get_cert_apply_writes_the_certificate_root_0644() {
    let host = ops_host();
    let api = FakeApi::new();
    let pem = certificate();
    api.reply(Method::Get, CERT_PATH, 200, pem.as_bytes());
    let (result, shown) = go(&host, &api, &get_cert(), Mode::Apply);
    assert!(result.unwrap());
    let meta = host.stat(OUT).unwrap().unwrap();
    assert_eq!((meta.mode, meta.uid, meta.gid), (0o644, 0, 0));
    assert_eq!(host.read(OUT).unwrap().unwrap(), pem.as_bytes());
    let sent = api.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (
            sent[0].service,
            sent[0].port,
            sent[0].user,
            sent[0].key.as_str()
        ),
        ("NanoDEP", 9001, "depserver", KEY)
    );
    assert_eq!(
        shown,
        format!(
            "+ GET http://127.0.0.1:9001{CERT_PATH} (NanoDEP API)\n\
             + write {OUT} mode 0644 owner root:root ({} bytes)\n\
             {OUT}: {NEXT}\n",
            pem.len()
        )
    );
}

/// Catches a get-cert that asks NanoDEP for a new key pair when a certificate is already
/// there (the uploaded certificate must be the last one made): the file is kept byte for
/// byte, no call is made, in a dry run and with apply.
#[test]
fn get_cert_keeps_an_existing_certificate_and_asks_nothing() {
    let host = ops_host();
    let pem = certificate();
    host.put_file(OUT, 0o600, (1000, 1000), pem.as_bytes());
    for mode in [Mode::DryRun, Mode::Apply] {
        let api = FakeApi::new();
        let (result, shown) = go(&host, &api, &get_cert(), mode);
        assert!(result.unwrap());
        assert_eq!(shown, format!("{OUT}: holds a certificate, kept; {NEXT}\n"));
        assert!(api.sent().is_empty());
        assert_eq!(host.read(OUT).unwrap().unwrap(), pem.as_bytes());
        assert_eq!(host.stat(OUT).unwrap().unwrap().mode, 0o600);
    }
}

/// Catches a get-cert that overwrites or accepts a file that is not a certificate: text,
/// a PEM block that is not a certificate and a directory are each refused with exit 2, and
/// nothing is asked.
#[test]
fn get_cert_refuses_a_file_that_is_not_a_certificate() {
    let not_certs: [&[u8]; 2] = [
        b"my notes\n",
        b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ];
    for bytes in not_certs {
        let host = ops_host();
        host.put_file(OUT, 0o644, (1000, 1000), bytes);
        let api = FakeApi::new();
        let (result, shown) = go(&host, &api, &get_cert(), Mode::Apply);
        let e = refused(result);
        assert_eq!(e.exit_code(), 2);
        assert_eq!(
            e.to_string(),
            format!(
                "{OUT} is there and is not a PEM certificate: move it aside, and run get-cert again to make a new one"
            )
        );
        assert_eq!((shown.as_str(), api.sent().len()), ("", 0));
        assert_eq!(host.read(OUT).unwrap().unwrap(), bytes);
    }
    let host = ops_host();
    host.put_dir(OUT, 0o755, (1000, 1000));
    let (result, _) = go(&host, &FakeApi::new(), &get_cert(), Mode::Apply);
    assert_eq!(refused(result).exit_code(), 2);
}

/// Catches a get-cert, or upload-token, that works inside a git work tree: a `.git` in the
/// file's directory or in one above refuses with exit 2 naming the `.git`, before any call.
#[test]
fn files_inside_a_git_work_tree_are_refused() {
    for git in ["/ops/.git", "/.git"] {
        let host = ops_host();
        host.put_file(TOKEN, 0o600, (1000, 1000), b"token");
        host.put_dir(git, 0o755, (1000, 1000));
        for d in [get_cert(), upload()] {
            let api = FakeApi::new();
            let (result, shown) = go(&host, &api, &d, Mode::Apply);
            let e = refused(result);
            assert_eq!(e.exit_code(), 2);
            let file = match d {
                Dep::GetCert { .. } => OUT,
                _ => TOKEN,
            };
            assert_eq!(
                e.to_string(),
                format!(
                    "{file} is inside a git work tree ({git} exists): keep keys, tokens and certificates outside every repository"
                )
            );
            assert_eq!((shown.as_str(), api.sent().len()), ("", 0));
        }
    }
}

/// Catches a get-cert that writes whatever NanoDEP answers: an answer that is not a PEM
/// certificate fails with exit 1, naming what was wrong, and no file is written.
#[test]
fn get_cert_with_a_non_certificate_answer_fails_and_writes_nothing() {
    let host = ops_host();
    let api = FakeApi::new();
    api.reply(Method::Get, CERT_PATH, 200, b"<html>login</html>");
    let before = host.paths();
    let (result, _) = go(&host, &api, &get_cert(), Mode::Apply);
    let e = refused(result);
    assert_eq!(e.exit_code(), 1);
    assert_eq!(
        e.to_string(),
        "the token certificate: not a PEM certificate"
    );
    assert_eq!(host.paths(), before);
}

/// Catches a get-cert that hides a refusal of NanoDEP: an answer of 500 fails with exit 1
/// and the status, and nothing is written.
#[test]
fn get_cert_with_a_refused_call_fails() {
    let host = ops_host();
    let api = FakeApi::new();
    api.reply(Method::Get, CERT_PATH, 500, b"broken");
    let before = host.paths();
    let (result, _) = go(&host, &api, &get_cert(), Mode::Apply);
    let e = refused(result);
    assert_eq!(e.exit_code(), 1);
    assert!(e.to_string().ends_with("answered 500: broken"), "{e}");
    assert_eq!(host.paths(), before);
}

/// Catches an upload-token dry run that sends the token: it prints both calls with the size
/// of the body (never the bytes), makes none, and changes nothing.
#[test]
fn upload_token_dry_run_calls_nothing() {
    let host = ops_host();
    host.put_file(TOKEN, 0o600, (1000, 1000), b"SECRET-TOKEN-BYTES");
    let api = FakeApi::new();
    let before = host.paths();
    let (result, shown) = go(&host, &api, &upload(), Mode::DryRun);
    assert!(result.unwrap());
    assert_eq!(
        shown,
        format!(
            "DRY-RUN: PUT http://127.0.0.1:9001/v1/tokenpki/example (NanoDEP API, 18 bytes)\n\
             DRY-RUN: GET http://127.0.0.1:9001/proxy/example/account (NanoDEP API)\n\
             {DRY_RUN_NOTE}\n"
        )
    );
    assert!(api.sent().is_empty());
    assert_eq!(host.paths(), before);
}

/// Catches an upload-token that sends other bytes than the file's or does not show the
/// proof: the token is put whole, then the account is read, and the organisation and the
/// MDM server are printed with the note that the file may be deleted.
#[test]
fn upload_token_puts_the_token_and_prints_the_account() {
    let host = ops_host();
    host.put_file(TOKEN, 0o600, (1000, 1000), b"SECRET-TOKEN-BYTES");
    let api = FakeApi::new();
    api.reply(Method::Put, "/v1/tokenpki/example", 200, b"");
    api.reply(
        Method::Get,
        "/proxy/example/account",
        200,
        br#"{"org_name":"Example Org","server_name":"hostkeeper-mdm"}"#,
    );
    let (result, shown) = go(&host, &api, &upload(), Mode::Apply);
    assert!(result.unwrap());
    let sent = api.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].call.method, Method::Put);
    assert_eq!(sent[0].call.path, "/v1/tokenpki/example");
    assert_eq!(sent[0].call.body, b"SECRET-TOKEN-BYTES");
    assert_eq!(sent[1].call.path, "/proxy/example/account");
    assert_eq!(
        shown,
        format!(
            "+ PUT http://127.0.0.1:9001/v1/tokenpki/example (NanoDEP API, 18 bytes)\n\
             + GET http://127.0.0.1:9001/proxy/example/account (NanoDEP API)\n\
             ABM: org Example Org, MDM server hostkeeper-mdm\n\
             the token is stored; you may delete {TOKEN} (NanoDEP keeps it)\n"
        )
    );
}

/// Catches an upload-token that fails on an account answer with a field missing: each missing
/// field shows as `?`.
#[test]
fn upload_token_shows_a_question_mark_for_a_missing_field() {
    for (answer, line) in [
        (
            &br#"{"org_name":"Example Org"}"#[..],
            "ABM: org Example Org, MDM server ?",
        ),
        (&b"{}"[..], "ABM: org ?, MDM server ?"),
    ] {
        let host = ops_host();
        host.put_file(TOKEN, 0o600, (1000, 1000), b"t");
        let api = FakeApi::new();
        api.reply(Method::Put, "/v1/tokenpki/example", 200, b"");
        api.reply(Method::Get, "/proxy/example/account", 200, answer);
        let (result, shown) = go(&host, &api, &upload(), Mode::Apply);
        assert!(result.unwrap());
        assert!(shown.lines().any(|l| l == line), "{shown}");
    }
}

/// Catches an upload-token that goes on without a token: a missing file is refused with
/// exit 2 naming it, before any call.
#[test]
fn upload_token_refuses_a_missing_token() {
    let host = ops_host();
    let api = FakeApi::new();
    let (result, shown) = go(&host, &api, &upload(), Mode::Apply);
    let e = refused(result);
    assert_eq!(e.exit_code(), 2);
    assert_eq!(e.to_string(), format!("{TOKEN} is not there"));
    assert_eq!((shown.as_str(), api.sent().len()), ("", 0));
}

/// Catches an upload-token that goes on when NanoDEP rejects the token: the failed PUT is
/// exit 1 and the account is not asked.
#[test]
fn upload_token_stops_when_the_put_fails() {
    let host = ops_host();
    host.put_file(TOKEN, 0o600, (1000, 1000), b"t");
    let api = FakeApi::new();
    api.reply(Method::Put, "/v1/tokenpki/example", 400, b"bad token");
    let (result, _) = go(&host, &api, &upload(), Mode::Apply);
    assert_eq!(refused(result).exit_code(), 1);
    assert_eq!(api.sent().len(), 1);
}

fn with_profile(host: &FakeHost) {
    host.put_file(UUID_FILE, 0o644, (0, 0), format!("{UUID}\n").as_bytes());
}

/// Catches an assign dry run that assigns: it prints the call with the size of its body,
/// returns true, calls nothing and changes nothing.
#[test]
fn assign_dry_run_calls_nothing_and_succeeds() {
    let host = ops_host();
    with_profile(&host);
    let api = FakeApi::new();
    let before = host.paths();
    let (result, shown) = go(&host, &api, &assign(&["C02ABCDEF1"]), Mode::DryRun);
    assert!(result.unwrap());
    let body = format!(r#"{{"devices":["C02ABCDEF1"],"profile_uuid":"{UUID}"}}"#);
    assert_eq!(
        shown,
        format!(
            "DRY-RUN: POST http://127.0.0.1:9001/proxy/example/profile/devices (NanoDEP API, {} bytes)\n\
             {DRY_RUN_NOTE}\n",
            body.len()
        )
    );
    assert!(api.sent().is_empty());
    assert_eq!(host.paths(), before);
}

/// Catches an assign that posts the wrong profile or devices, or reports success wrongly:
/// the recorded uuid and every serial are in the body, each status is printed, and the run
/// is true only when every status is SUCCESS.
#[test]
fn assign_reports_each_device_and_succeeds_only_when_all_are_assigned() {
    let host = ops_host();
    with_profile(&host);
    let path = "/proxy/example/profile/devices";
    let cases: [(&[u8], bool, &str); 3] = [
        (
            br#"{"devices":{"C02ABCDEF1":"SUCCESS","C02ABCDEF2":"SUCCESS"}}"#,
            true,
            "C02ABCDEF1: SUCCESS\nC02ABCDEF2: SUCCESS\n",
        ),
        (
            br#"{"devices":{"C02ABCDEF1":"SUCCESS","C02ABCDEF2":"NOT_ACCESSIBLE"}}"#,
            false,
            "C02ABCDEF1: SUCCESS\nC02ABCDEF2: NOT_ACCESSIBLE\n",
        ),
        (
            br#"{"devices":{"C02ABCDEF1":"SUCCESS"}}"#,
            false,
            "C02ABCDEF1: SUCCESS\nC02ABCDEF2: no answer\n",
        ),
    ];
    for (answer, all, lines) in cases {
        let api = FakeApi::new();
        api.reply(Method::Post, path, 200, answer);
        let (result, shown) = go(
            &host,
            &api,
            &assign(&["C02ABCDEF1", "C02ABCDEF2"]),
            Mode::Apply,
        );
        assert_eq!(result.unwrap(), all, "{shown}");
        assert!(shown.ends_with(lines), "{shown}");
        let sent = api.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].call.json);
        let body: serde_json::Value = serde_json::from_slice(&sent[0].call.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"profile_uuid": UUID, "devices": ["C02ABCDEF1", "C02ABCDEF2"]})
        );
    }
    // An answer that is not JSON assigns no device.
    let api = FakeApi::new();
    api.reply(Method::Post, path, 200, b"oops");
    let (result, shown) = go(&host, &api, &assign(&["C02ABCDEF1"]), Mode::Apply);
    assert!(!result.unwrap());
    assert!(shown.ends_with("C02ABCDEF1: no answer\n"), "{shown}");
}

/// Catches an assign that runs with no recorded profile, or an empty record: it is refused
/// with exit 2 naming the file and the verb to run first, and calls nothing.
#[test]
fn assign_without_a_recorded_profile_is_refused() {
    for record in [None, Some(&b"\n"[..])] {
        let host = ops_host();
        if let Some(bytes) = record {
            host.put_file(UUID_FILE, 0o644, (0, 0), bytes);
        }
        let api = FakeApi::new();
        let (result, shown) = go(&host, &api, &assign(&["C02ABCDEF1"]), Mode::Apply);
        let e = refused(result);
        assert_eq!(e.exit_code(), 2);
        assert_eq!(
            e.to_string(),
            format!(
                "{UUID_FILE} is not there: run hostkeeper-mdm dep define-profile --apply first"
            )
        );
        assert_eq!((shown.as_str(), api.sent().len()), ("", 0));
    }
}

/// Catches a dep verb that works for someone who is not root, even in a dry run: each is
/// refused with exit 2 naming the effective uid, before the key file is read or any call.
#[test]
fn a_user_who_is_not_root_is_refused_even_for_a_dry_run() {
    let host = FakeHost::new(1000);
    for d in [
        get_cert(),
        upload(),
        Dep::DefineProfile,
        assign(&["C02ABCDEF1"]),
    ] {
        for mode in [Mode::DryRun, Mode::Apply] {
            let api = FakeApi::new();
            let (result, shown) = go(&host, &api, &d, mode);
            let e = refused(result);
            assert_eq!(e.exit_code(), 2);
            assert_eq!(
                e.to_string(),
                "dep reads root-only files: run it as root, even for a dry run (effective uid is 1000)"
            );
            assert_eq!((shown.as_str(), api.sent().len()), ("", 0));
        }
    }
}

/// Catches a dep verb that reads an API key from a file others can read, or from one that is
/// missing: each is refused (exit 2) before any call, in the dry run too.
#[test]
fn a_key_file_that_is_not_0600_root_or_not_there_is_refused_before_any_call() {
    for (mode_bits, owner) in [(0o644, 0), (0o600, SVC)] {
        let host = ops_host();
        host.put_file(
            NANODEP_ENV,
            mode_bits,
            (owner, owner),
            format!("NANODEP_API={KEY}\n").as_bytes(),
        );
        for d in [get_cert(), Dep::DefineProfile] {
            let api = FakeApi::new();
            let (result, shown) = go(&host, &api, &d, Mode::DryRun);
            let e = refused(result);
            assert_eq!(e.exit_code(), 2);
            assert!(matches!(e, MdmError::SecretMode { .. }), "{e}");
            assert_eq!((shown.as_str(), api.sent().len()), ("", 0));
        }
    }
    let host = FakeHost::new(0);
    let (result, _) = go(&host, &FakeApi::new(), &Dep::DefineProfile, Mode::DryRun);
    assert!(matches!(refused(result), MdmError::NoSecret { .. }));
    // The reader the verbs use is the shared one.
    assert!(secret::read(&ops_host(), NANODEP_ENV, "NANODEP_API").is_ok());
}

/// Catches changes the dry runs make to the host through commands: none of the verbs runs
/// anything but queries.
#[test]
fn get_cert_dry_run_runs_no_command() {
    let host = ops_host();
    let (result, _) = go(&host, &FakeApi::new(), &get_cert(), Mode::DryRun);
    assert!(result.unwrap());
    assert!(changes(&host).is_empty());
}

/// Catches a serial check that is off by one or too loose: 7 characters are refused, 8 and
/// 14 accepted, 15 refused; lower case, a space, an empty text and non-ASCII are refused
/// with the text quoted.
#[test]
fn serial_numbers_are_8_to_14_upper_case_letters_and_digits() {
    for ok in ["ABCDEFG1", "ABCDEFGHIJKLM1", "C02ABCDEF1", "12345678"] {
        assert_eq!(Serial::new(ok).unwrap().as_str(), ok);
    }
    for bad in [
        "ABCDEF1",
        "ABCDEFGHIJKLMN1",
        "abcdefgh1",
        "ABCDEFG ",
        "",
        "ABCDEFGH\u{c9}",
        "ABCDEFG-1",
    ] {
        assert_eq!(
            Serial::new(bad).unwrap_err(),
            format!("{bad:?} is not a serial number (8 to 14 upper-case letters and digits)")
        );
    }
}

/// Catches an exit code that is wrong: a certificate or profile record that is not there
/// is a refusal (2), an answer that is not usable and a profile that cannot be written are
/// failures (1).
#[test]
fn the_dep_errors_have_their_exit_codes() {
    assert_eq!(DepError::NotCertificate("f".to_owned()).exit_code(), 2);
    assert_eq!(DepError::NoProfile("f".to_owned()).exit_code(), 2);
    let answer = DepError::Answer {
        what: "x",
        why: "y".to_owned(),
    };
    assert_eq!(answer.exit_code(), 1);
    let profile = DepError::from(ProfileError::Json("z".to_owned()));
    assert_eq!(profile.exit_code(), 1);
    assert_eq!(MdmError::from(profile).exit_code(), 1);
}

/// Catches an account check that passes on an answer it cannot read: an answer that is not a
/// JSON object fails (exit 1) quoting the answer, after the token was put.
#[test]
fn upload_token_fails_on_an_account_answer_that_is_not_json() {
    for answer in [&b"not json"[..], &b"[1]"[..]] {
        let host = ops_host();
        host.put_file(TOKEN, 0o600, (1000, 1000), b"t");
        let api = FakeApi::new();
        api.reply(Method::Put, "/v1/tokenpki/example", 200, b"");
        api.reply(Method::Get, "/proxy/example/account", 200, answer);
        let (result, _) = go(&host, &api, &upload(), Mode::Apply);
        let e = result.unwrap_err();
        assert_eq!(e.exit_code(), 1);
        assert_eq!(
            e.to_string(),
            format!(
                "the account check: not a JSON object: {}",
                String::from_utf8_lossy(answer)
            )
        );
        assert_eq!(api.sent().len(), 2);
    }
}

/// Catches an upload-token that sends an empty token, or reads a file of any size whole: an
/// empty file and one of 64 KiB and a byte are refused (exit 2) by name before any call, and one
/// of exactly 64 KiB is sent.
#[test]
fn upload_token_refuses_an_empty_or_oversized_file() {
    for (size, refused) in [
        (0, Some("empty")),
        (64 * 1024 + 1, Some("larger than 64 KiB")),
        (64 * 1024, None),
    ] {
        let host = ops_host();
        host.put_file(TOKEN, 0o600, (1000, 1000), &vec![b'x'; size]);
        let api = FakeApi::new();
        api.reply(Method::Put, "/v1/tokenpki/example", 200, b"");
        api.reply(Method::Get, "/proxy/example/account", 200, b"{}");
        let (result, _) = go(&host, &api, &upload(), Mode::Apply);
        match refused {
            Some(why) => {
                let e = result.unwrap_err();
                assert_eq!(e.exit_code(), 2);
                assert_eq!(
                    e.to_string(),
                    format!(
                        "{TOKEN} is {why}: give the server token (.p7m) Apple Business Manager gave"
                    )
                );
                assert!(api.sent().is_empty());
            }
            None => {
                assert!(result.unwrap());
                assert_eq!(api.sent()[0].call.body.len(), size);
            }
        }
    }
}

/// Catches a token that cannot be read taken for a missing one, or sent anyway: a read the
/// system denies fails the run (exit 1) before any call.
#[test]
fn upload_token_fails_when_the_token_cannot_be_read() {
    let host = ops_host();
    host.put_file(TOKEN, 0o600, (1000, 1000), b"t");
    host.deny_read(TOKEN);
    let api = FakeApi::new();
    let (result, _) = go(&host, &api, &upload(), Mode::Apply);
    let e = result.unwrap_err();
    assert_eq!(e.exit_code(), 1);
    assert!(api.sent().is_empty());
}
