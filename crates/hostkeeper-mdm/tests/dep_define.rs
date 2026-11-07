//! `dep define-profile` on the fake host and API, and the `dep` verbs through the command
//! line.

mod common;
mod dep_support;

use common::{FakeApi, FakeMachine, FakeNet, changes, config, example, with_system_api};
use dep_support::{SUM_FILE, UUID, UUID_FILE, certificate, dep_host, go, go_failing, go_with};
use hostkeeper_mdm::api::Method;
use hostkeeper_mdm::dep::{Dep, Serial};
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::profile::AdeProfile;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{DRY_RUN_NOTE, Host, Mode, SYSTEMCTL};
use sha2::{Digest, Sha256};

const UNIT: &str = "hostkeeper-mdm-depsyncer.service";
const DEFINE: &str = "/proxy/example/profile";
const ASSIGNER: &str = "/v1/assigner/example?profile_uuid=0123456789abcdef0123456789abcdef";

fn json_of(c: &MdmConfig) -> String {
    String::from_utf8(AdeProfile::from_config(c).to_json().unwrap()).unwrap()
}

fn sum_of(c: &MdmConfig) -> String {
    format!("{}\n", hex::encode(Sha256::digest(json_of(c))))
}

fn answered() -> FakeApi {
    let api = FakeApi::new();
    api.reply(
        Method::Post,
        DEFINE,
        200,
        format!(r#"{{"profile_uuid":"{UUID}"}}"#).as_bytes(),
    );
    api.reply(Method::Put, ASSIGNER, 200, b"");
    api
}

fn language(code: &str) -> MdmConfig {
    MdmConfig::parse(&example().replace("\nMDM_LANGUAGE=en\n", &format!("\nMDM_LANGUAGE={code}\n")))
        .unwrap()
}

fn read(host: &FakeHost, path: &str) -> Option<String> {
    host.read(path)
        .unwrap()
        .map(|b| String::from_utf8(b).unwrap())
}

/// Catches a define-profile dry run that defines the profile or records it: it prints the
/// three steps and the profile's JSON, indented, in full, then the plan that records and
/// starts depsyncer; no call is made and the host is as it was.
#[test]
fn define_profile_dry_run_calls_nothing_and_records_nothing() {
    let host = dep_host();
    let api = FakeApi::new();
    let c = config();
    let before = host.paths();
    let (result, shown) = go(&host, &api, &Dep::DefineProfile, Mode::DryRun);
    assert!(result.unwrap());
    let json = json_of(&c);
    let size = json.len();
    let indented: String = json.lines().map(|l| format!("    | {l}\n")).collect();
    let sum = sum_of(&c);
    assert_eq!(
        shown,
        format!(
            "DRY-RUN: POST http://127.0.0.1:9001{DEFINE} (NanoDEP API, {size} bytes)\n\
             DRY-RUN: PUT /v1/assigner/example?profile_uuid=<the profile_uuid Apple answers>\n\
             DRY-RUN: write {UUID_FILE} (that uuid)\n\
             {indented}\
             DRY-RUN: write {SUM_FILE} mode 0644 owner root:root ({} bytes)\n\
             DRY-RUN: systemctl enable -- {UNIT}\n\
             DRY-RUN: systemctl start -- {UNIT}\n\
             {DRY_RUN_NOTE}\n",
            sum.len()
        )
    );
    assert!(api.sent().is_empty());
    assert_eq!(host.paths(), before);
    assert!(changes(&host).is_empty());
}

/// Catches a define-profile that records a profile it did not define, or forgets depsyncer:
/// it posts the profile's JSON, makes the answered uuid the assigner's, then records the
/// uuid and the profile's sha256 (root, 0644) and enables and starts depsyncer.
#[test]
fn define_profile_defines_once_records_and_starts_depsyncer() {
    let host = dep_host();
    let api = answered();
    let c = config();
    let (result, shown) = go(&host, &api, &Dep::DefineProfile, Mode::Apply);
    assert!(result.unwrap());
    let sent = api.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].call.method, Method::Post);
    assert!(sent[0].call.json);
    assert_eq!(sent[0].call.body, json_of(&c).as_bytes());
    assert_eq!(
        (sent[1].call.method, sent[1].call.path.as_str()),
        (Method::Put, ASSIGNER)
    );
    assert!(sent[1].call.body.is_empty());
    assert_eq!(read(&host, UUID_FILE).unwrap(), format!("{UUID}\n"));
    assert_eq!(read(&host, SUM_FILE).unwrap(), sum_of(&c));
    for file in [UUID_FILE, SUM_FILE] {
        let meta = host.stat(file).unwrap().unwrap();
        assert_eq!((meta.mode, meta.uid, meta.gid), (0o644, 0, 0));
    }
    assert!(shown.contains(&format!(
        "ADE profile: defined, uuid {UUID}; the assigner gives it to new devices\n"
    )));
    assert_eq!(
        changes(&host),
        [
            format!("{SYSTEMCTL} enable -- {UNIT}"),
            format!("{SYSTEMCTL} start -- {UNIT}"),
        ]
    );
}

/// Catches a define-profile that defines the same profile again: a rerun with the recorded
/// sha256 calls nothing, says so with the recorded uuid, and starts nothing when depsyncer
/// runs.
#[test]
fn define_profile_rerun_with_the_same_profile_calls_nothing() {
    let host = dep_host();
    let (result, _) = go(&host, &answered(), &Dep::DefineProfile, Mode::Apply);
    assert!(result.unwrap());
    host.reply_prefix(&[SYSTEMCTL, "is-enabled"], 0, "enabled\n");
    host.reply_prefix(&[SYSTEMCTL, "is-active"], 0, "active\n");
    let api = FakeApi::new();
    let calls_before = host.calls().len();
    for mode in [Mode::DryRun, Mode::Apply] {
        let (result, shown) = go(&host, &api, &Dep::DefineProfile, mode);
        assert!(result.unwrap());
        assert_eq!(
            shown,
            format!("ADE profile: unchanged (uuid {UUID})\nnothing to do\n")
        );
    }
    assert!(api.sent().is_empty());
    assert!(
        host.calls()[calls_before..]
            .iter()
            .all(|c| c[1].starts_with("is-"))
    );
}

/// Catches a define-profile that keeps an old record after a setting changes: with another
/// MDM_LANGUAGE the profile differs, so it is defined again and the record moves to the new
/// uuid and sum.
#[test]
fn a_changed_setting_defines_the_profile_again() {
    let host = dep_host();
    let (result, _) = go(&host, &answered(), &Dep::DefineProfile, Mode::Apply);
    assert!(result.unwrap());
    let fr = language("fr");
    assert_ne!(sum_of(&fr), sum_of(&config()));
    let other = "fedcba9876543210fedcba9876543210";
    let api = FakeApi::new();
    api.reply(
        Method::Post,
        DEFINE,
        200,
        format!(r#"{{"profile_uuid":"{other}"}}"#).as_bytes(),
    );
    api.reply(
        Method::Put,
        &format!("/v1/assigner/example?profile_uuid={other}"),
        200,
        b"",
    );
    let (result, _) = go_with(&host, &api, &fr, &Dep::DefineProfile, Mode::Apply);
    assert!(result.unwrap());
    assert_eq!(api.sent().len(), 2);
    assert!(String::from_utf8_lossy(&api.sent()[0].call.body).contains("\"fr\""));
    assert_eq!(read(&host, UUID_FILE).unwrap(), format!("{other}\n"));
    assert_eq!(read(&host, SUM_FILE).unwrap(), sum_of(&fr));
}

/// Catches a define-profile that records an answer it cannot use: no `profile_uuid`, a
/// short one, a long one, one with a non-hex digit, one that is not a string, or a body that
/// is not JSON fails with exit 1 naming the answer, makes no assigner call and records
/// nothing.
#[test]
fn an_answer_without_a_valid_profile_uuid_fails_and_records_nothing() {
    let long = format!(r#"{{"profile_uuid":"{}"}}"#, "a".repeat(65));
    let answers = [
        r#"{"error":"no"}"#.to_owned(),
        r#"{"profile_uuid":"abc123"}"#.to_owned(),
        long,
        r#"{"profile_uuid":"0123456789abcdeg0123456789abcdef"}"#.to_owned(),
        r#"{"profile_uuid":12345678901234567890}"#.to_owned(),
        "not json".to_owned(),
    ];
    for answer in answers {
        let host = dep_host();
        let api = FakeApi::new();
        api.reply(Method::Post, DEFINE, 200, answer.as_bytes());
        let before = host.paths();
        let (result, _) = go(&host, &api, &Dep::DefineProfile, Mode::Apply);
        let e = result.expect_err("refused");
        assert_eq!(e.exit_code(), 1, "{answer}");
        assert_eq!(
            e.to_string(),
            format!("the profile definition: no profile_uuid of 16 to 64 hex digits in {answer}")
        );
        assert_eq!(api.sent().len(), 1);
        assert_eq!(host.paths(), before);
        assert!(changes(&host).is_empty());
    }
}

/// Catches a define-profile that records a profile that did not become the assigner's: the
/// assigner call refused leaves no uuid, no sum and depsyncer untouched, exit 1.
#[test]
fn a_failing_assigner_call_records_nothing() {
    let host = dep_host();
    let api = FakeApi::new();
    api.reply(
        Method::Post,
        DEFINE,
        200,
        format!(r#"{{"profile_uuid":"{UUID}"}}"#).as_bytes(),
    );
    api.reply(Method::Put, ASSIGNER, 500, b"nope");
    let before = host.paths();
    let (result, _) = go(&host, &api, &Dep::DefineProfile, Mode::Apply);
    let e = result.expect_err("fails");
    assert_eq!(e.exit_code(), 1);
    assert!(e.to_string().ends_with("answered 500: nope"), "{e}");
    assert_eq!(host.paths(), before);
    assert!(changes(&host).is_empty());
}

/// Catches a record that is trusted when it is not a regular file: with a directory where the
/// sum belongs, the dry run plans the definition, and the apply posts the profile, makes it the
/// assigner's and records the uuid and the sum, never saying "unchanged".
#[test]
fn a_directory_where_the_sum_belongs_is_not_a_record() {
    let host = dep_host();
    host.put_dir(SUM_FILE, 0o755, (0, 0));
    let api = answered();
    let (result, shown) = go(&host, &api, &Dep::DefineProfile, Mode::DryRun);
    assert!(result.unwrap());
    assert!(api.sent().is_empty());
    assert!(
        shown.starts_with("DRY-RUN: POST http://127.0.0.1:9001/proxy/example/profile "),
        "{shown}"
    );
    let (result, shown) = go(&host, &api, &Dep::DefineProfile, Mode::Apply);
    assert!(result.unwrap());
    assert!(!shown.contains("unchanged"), "{shown}");
    let sent = api.sent();
    let calls: Vec<(Method, &str)> = sent
        .iter()
        .map(|s| (s.call.method, s.call.path.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            (Method::Post, "/proxy/example/profile"),
            (Method::Put, ASSIGNER)
        ]
    );
    assert_eq!(read(&host, UUID_FILE).unwrap(), format!("{UUID}\n"));
    assert_eq!(read(&host, SUM_FILE).unwrap(), sum_of(&config()));
}

fn args(words: &[&str]) -> Vec<String> {
    std::iter::once("hostkeeper-mdm")
        .chain(words.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Runs the command line `words` on `host`: exit code, standard output, standard error.
fn cli(host: &FakeHost, api: &FakeApi, words: &[&str]) -> (u8, String, String) {
    let net = FakeNet::new(Default::default());
    let machine = FakeMachine::new(host);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = with_system_api(&machine, &net, api, |system| {
        hostkeeper_mdm::run(&args(words), system, &mut out, &mut err)
    });
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn cli_host() -> FakeHost {
    let host = dep_host();
    host.put_file("/cfg", 0o644, (0, 0), example().as_bytes());
    with_profile_record(&host);
    host
}

fn with_profile_record(host: &FakeHost) {
    host.put_file(UUID_FILE, 0o644, (0, 0), format!("{UUID}\n").as_bytes());
}

/// Catches an assign command line that is accepted without a device or with a bad serial:
/// no `--serial` and an invalid serial each exit 2 with the reason on stderr, nothing on
/// stdout and no call.
#[test]
fn dep_assign_needs_valid_serials() {
    let host = cli_host();
    let api = FakeApi::new();
    let (code, out, err) = cli(&host, &api, &["dep", "assign", "--settings", "/cfg"]);
    assert_eq!((code, out.as_str()), (2, ""), "{err}");
    assert!(err.contains("--serial"), "{err}");
    let (code, out, err) = cli(
        &host,
        &api,
        &["dep", "assign", "--settings", "/cfg", "--serial", "abc"],
    );
    assert_eq!((code, out.as_str()), (2, ""), "{err}");
    assert!(err.contains("is not a serial number"), "{err}");
    assert!(api.sent().is_empty());
}

/// Catches an assign command line that exits 0 when Apple did not assign every device: all
/// SUCCESS exits 0, a NOT_ACCESSIBLE exits 1, and a dry run exits 0 with no call.
#[test]
fn dep_assign_exits_1_unless_every_device_is_assigned() {
    let host = cli_host();
    let path = "/proxy/example/profile/devices";
    let words = |apply: bool| {
        let mut w = vec![
            "dep",
            "assign",
            "--settings",
            "/cfg",
            "--serial",
            "C02ABCDEF1",
            "--serial",
            "C02ABCDEF2",
        ];
        if apply {
            w.push("--apply");
        }
        w
    };
    let api = FakeApi::new();
    api.reply(
        Method::Post,
        path,
        200,
        br#"{"devices":{"C02ABCDEF1":"SUCCESS","C02ABCDEF2":"SUCCESS"}}"#,
    );
    let (code, out, err) = cli(&host, &api, &words(true));
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        out.ends_with("C02ABCDEF1: SUCCESS\nC02ABCDEF2: SUCCESS\n"),
        "{out}"
    );
    let api = FakeApi::new();
    api.reply(
        Method::Post,
        path,
        200,
        br#"{"devices":{"C02ABCDEF1":"SUCCESS","C02ABCDEF2":"NOT_ACCESSIBLE"}}"#,
    );
    let (code, out, _) = cli(&host, &api, &words(true));
    assert_eq!(code, 1);
    assert!(out.ends_with("C02ABCDEF2: NOT_ACCESSIBLE\n"), "{out}");
    let api = FakeApi::new();
    let (code, _, _) = cli(&host, &api, &words(false));
    assert_eq!(code, 0);
    assert!(api.sent().is_empty());
}

/// Catches dep verbs not wired to the command line: get-cert and upload-token reach their
/// verbs with the operator's file, define-profile with the settings, and a dep verb run by
/// someone who is not root is refused with exit 2.
#[test]
fn the_other_dep_verbs_run_through_the_command_line() {
    let host = cli_host();
    host.put_dir("/ops", 0o755, (1000, 1000));
    host.put_file("/ops/token.p7m", 0o600, (1000, 1000), b"tok");
    let api = FakeApi::new();
    let (code, out, _) = cli(
        &host,
        &api,
        &[
            "dep",
            "get-cert",
            "--settings",
            "/cfg",
            "--out",
            "/ops/n.crt",
        ],
    );
    assert_eq!(code, 0);
    assert!(out.starts_with("DRY-RUN: GET "), "{out}");
    let (code, out, _) = cli(
        &host,
        &api,
        &[
            "dep",
            "upload-token",
            "--settings",
            "/cfg",
            "--token",
            "/ops/token.p7m",
        ],
    );
    assert_eq!(code, 0);
    assert!(out.starts_with("DRY-RUN: PUT "), "{out}");
    let (code, out, _) = cli(
        &host,
        &api,
        &["dep", "define-profile", "--settings", "/cfg"],
    );
    assert_eq!(code, 0);
    assert!(out.starts_with("DRY-RUN: POST "), "{out}");
    assert!(api.sent().is_empty());

    let user = FakeHost::new(1000);
    user.add_user("hostkeeper-mdm", common::SVC);
    user.put_file("/cfg", 0o644, (0, 0), example().as_bytes());
    let (code, out, err) = cli(
        &user,
        &api,
        &["dep", "define-profile", "--settings", "/cfg"],
    );
    assert_eq!((code, out.as_str()), (2, ""), "{err}");
    assert!(
        err.starts_with("REFUSED: dep reads root-only files"),
        "{err}"
    );
}

fn every_run() -> Vec<(Dep, Mode)> {
    let get = |file: &str| Dep::GetCert {
        out: file.to_owned(),
    };
    let upload = || Dep::UploadToken {
        token: "/ops/t.p7m".to_owned(),
    };
    let assign = || Dep::Assign {
        serials: vec![Serial::new("C02ABCDEF1").unwrap()],
    };
    vec![
        (get("/ops/n.crt"), Mode::DryRun),
        (get("/ops/n.crt"), Mode::Apply),
        (get("/ops/held.crt"), Mode::Apply),
        (upload(), Mode::DryRun),
        (upload(), Mode::Apply),
        (Dep::DefineProfile, Mode::DryRun),
        (Dep::DefineProfile, Mode::Apply),
        (assign(), Mode::DryRun),
        (assign(), Mode::Apply),
    ]
}

/// Catches a verb that carries on, or reports success, when its output cannot be written:
/// whichever write of the run fails (the calls, the notes, the plan), the run ends with an
/// error of exit 1, for every verb, in a dry run and with apply.
#[test]
fn a_closed_output_fails_the_run_at_whichever_write_it_closes() {
    let cert = certificate();
    for (d, mode) in every_run() {
        let mut closed_after = 0;
        loop {
            let host = dep_host();
            host.put_dir("/ops", 0o755, (1000, 1000));
            host.put_file("/ops/t.p7m", 0o600, (1000, 1000), b"t");
            host.put_file("/ops/held.crt", 0o644, (1000, 1000), cert.as_bytes());
            host.put_file(UUID_FILE, 0o644, (0, 0), format!("{UUID}\n").as_bytes());
            let api = answered();
            let cert_path = "/v1/tokenpki/example?cn=hostkeeper-mdm&validity_days=7";
            api.reply(Method::Get, cert_path, 200, cert.as_bytes());
            api.reply(Method::Put, "/v1/tokenpki/example", 200, b"");
            api.reply(Method::Get, "/proxy/example/account", 200, b"{}");
            api.reply(Method::Post, "/proxy/example/profile/devices", 200, b"{}");
            // The recorded uuid stands for the assign runs; define-profile has no sum yet.
            match go_failing(&host, &api, &d, mode, closed_after) {
                Ok(_) => break,
                Err(e) => assert!(
                    e.to_string().contains("output is closed"),
                    "after {closed_after} writes: {e}"
                ),
            }
            closed_after += 1;
            assert!(
                closed_after < 5000,
                "{mode:?}: a run of fewer than 5000 writes"
            );
        }
        assert!(closed_after > 0);
    }
}

/// Catches a define-profile that trusts the digest record alone: with the sha256 recorded but
/// the uuid record gone, empty or not a uuid, `assign` would have no profile, so the profile is
/// defined again and both records are written.
#[test]
fn a_matching_digest_without_a_uuid_record_defines_again() {
    for uuid in [None, Some(""), Some("not-a-uuid\n")] {
        let host = dep_host();
        let (result, _) = go(&host, &answered(), &Dep::DefineProfile, Mode::Apply);
        assert!(result.unwrap());
        match uuid {
            None => host.remove(UUID_FILE).unwrap(),
            Some(text) => host.put_file(UUID_FILE, 0o644, (0, 0), text.as_bytes()),
        }
        let api = answered();
        let (result, shown) = go(&host, &api, &Dep::DefineProfile, Mode::Apply);
        assert!(result.unwrap());
        assert_eq!(api.sent().len(), 2, "{uuid:?}");
        assert!(shown.contains("ADE profile: defined"), "{shown}");
        assert_eq!(read(&host, UUID_FILE).unwrap(), format!("{UUID}\n"));
    }
}
