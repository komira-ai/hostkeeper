//! `push-cert`: the check of the pair, the upload to NanoHUB and the enrollment profile, on
//! the fake host.

mod common;
mod push_cert_support;

use common::{FakeApi, SVC, config, random_hex};
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm::api::Method;
use hostkeeper_mdm::push_cert::{With, check_pair};
use hostkeeper_mdm_core::profile::{self, Challenge};
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{GETENT, Host, Mode};
use push_cert_support::*;
use sha2::{Digest, Sha256};

/// NanoHUB's answer that stores `topic`.
fn stored(topic: &str) -> FakeApi {
    let api = FakeApi::new();
    api.reply(
        Method::Put,
        PUSHCERT,
        200,
        format!("{{\"topic\":\"{topic}\"}}").as_bytes(),
    );
    api
}

/// The refusal `result` is, as (exit code, text).
fn refused(result: Result<(), MdmError>) -> (u8, String) {
    let e = result.expect_err("a refusal");
    (e.exit_code(), e.to_string())
}

/// The enrollment profile core makes for [`TOPIC`] and the test challenge.
fn expected_profile() -> Vec<u8> {
    let challenge = Challenge::new(&random_hex()).unwrap();
    profile::enrollment(&config(), &topic(), &challenge).unwrap()
}

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn profile_line() -> String {
    format!(
        "profile: ServerURL https://mdm.example.com/mdm, SCEP https://mdm.example.com/scep, topic {TOPIC}; served at https://mdm.example.com/enroll (the challenge is in it, not shown)\n"
    )
}

fn pair_line() -> String {
    format!(
        "push certificate: topic {TOPIC}, valid until Mar 17 17:46:40 2030 +00:00 (renew it before then at the Apple Push Certificates Portal, with the same Apple Account)\n"
    )
}

/// Catches a profile that the proxy cannot read or that anyone can: the profile is written
/// 0640, owned by root with the service group, its bytes are core's enrollment profile with
/// the SCEP challenge from scep.env, and the whole output shows neither the challenge nor a key.
#[test]
fn the_enrollment_profile_is_served_mode_0640() {
    let p = push_pair();
    let host = host(&p);
    let api = FakeApi::new();
    let (result, out) = go(
        &host,
        &api,
        NOW,
        With::ProfileOnly(Some(topic())),
        Mode::Apply,
    );
    result.unwrap();
    let bytes = expected_profile();
    let want = format!(
        "{}+ write {PROFILE} mode 0640 owner root:hostkeeper-mdm ({} bytes)\n",
        profile_line(),
        bytes.len()
    );
    assert_eq!(out, want);
    let node = host.node(PROFILE).unwrap();
    assert_eq!((node.mode, node.uid, node.gid), (0o640, 0, SVC));
    assert_eq!(node.bytes, bytes);
    assert!(
        String::from_utf8(node.bytes)
            .unwrap()
            .contains(&random_hex())
    );
    assert!(api.sent().is_empty());
    assert!(host.node(TOPIC_FILE).is_none());
}

/// Catches `push-cert` run by someone else: it reads root-only files, so even a dry run is
/// refused at once, with the effective uid.
#[test]
fn a_user_other_than_root_is_refused_even_for_a_dry_run() {
    let host = FakeHost::new(1000);
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::DryRun).0);
    assert_eq!(code, 2);
    assert_eq!(
        text,
        "push-cert reads root-only files: run it as root, even for a dry run (effective uid is 1000)"
    );
}

/// Catches a run before install: without MDM_STATE/www as a directory (missing, or a file) it
/// refuses and says to run install first.
#[test]
fn a_missing_www_directory_is_refused() {
    let p = push_pair();
    let host = host(&p);
    let www = format!("{STATE}/www");
    host.remove(&www).unwrap();
    let want = format!("{www} is not a directory: run hostkeeper-mdm install --apply first");
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!((code, text.as_str()), (2, want.as_str()));
    host.put_file(&www, 0o644, (0, 0), b"");
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!((code, text.as_str()), (2, want.as_str()));
}

/// Catches a key or certificate kept in a repository: a `.git` above either file (the
/// certificate's directory, or the key's far above it) is refused before the file is read,
/// naming the file and the `.git`.
#[test]
fn a_file_inside_a_git_work_tree_is_refused() {
    let p = push_pair();
    let host = host(&p);
    host.put_dir("/srv/keys/.git", 0o755, (1000, 1000));
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 2);
    assert_eq!(
        text,
        format!(
            "{CERT} is inside a git work tree (/srv/keys/.git exists): keep keys, tokens and certificates outside every repository"
        )
    );
    host.remove("/srv/keys/.git").unwrap();
    host.put_dir("/work/.git", 0o755, (1000, 1000));
    host.put_file("/work/a/b/k.key", 0o600, (1000, 1000), p.key.as_bytes());
    let with = With::Pair {
        cert: CERT.to_owned(),
        key: "/work/a/b/k.key".to_owned(),
    };
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with, Mode::Apply).0);
    assert_eq!(code, 2);
    assert!(
        text.starts_with("/work/a/b/k.key is inside a git work tree (/work/.git exists)"),
        "{text}"
    );
    // A file directly under the root: the root itself is looked at too.
    host.put_dir("/.git", 0o755, (0, 0));
    host.put_file("/k.key", 0o600, (1000, 1000), p.key.as_bytes());
    let with = With::Pair {
        cert: CERT.to_owned(),
        key: "/k.key".to_owned(),
    };
    let (_, text) = refused(go(&host, &FakeApi::new(), NOW, with, Mode::Apply).0);
    assert!(text.contains("(/.git exists)"), "{text}");
}

/// Catches a missing certificate or key file passing as empty: each is refused by name.
#[test]
fn a_missing_certificate_or_key_file_is_refused() {
    let p = push_pair();
    let host = host(&p);
    host.remove(KEY).unwrap();
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!((code, text), (2, format!("{KEY} is not there")));
    host.remove(CERT).unwrap();
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!((code, text), (2, format!("{CERT} is not there")));
}

/// Catches a key file readable by others (0640 or 0644, whoever owns it) being given to
/// NanoHUB: each is refused with its mode, and the certificate file's mode does not matter.
#[test]
fn a_key_file_that_is_not_mode_0600_is_refused() {
    let p = push_pair();
    let host = host(&p);
    for (mode, owner) in [
        (0o644, (1000, 1000)),
        (0o640, (0, 0)),
        (0o400, (1000, 1000)),
    ] {
        host.put_file(KEY, mode, owner, p.key.as_bytes());
        let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
        assert_eq!(code, 2);
        assert_eq!(
            text,
            format!("{KEY} is mode {mode:04o}, not 0600: chmod 0600 it (it holds a private key)")
        );
    }
}

/// Catches a certificate file that is not a PEM X.509 certificate being read as one: plain
/// text, an empty file and a certificate-shaped PEM of garbage are each refused by name.
#[test]
fn a_file_that_is_not_a_certificate_is_refused() {
    let p = push_pair();
    let host = host(&p);
    let junk = "-----BEGIN CERTIFICATE-----\nAAAAAAAA\n-----END CERTIFICATE-----\n";
    for text in ["not a certificate\n", "", junk] {
        host.put_file(CERT, 0o644, (1000, 1000), text.as_bytes());
        let (code, said) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
        assert_eq!(
            (code, said),
            (2, format!("{CERT} is not a PEM certificate"))
        );
    }
}

/// Catches a certificate that is not a push certificate being uploaded: one with no subject
/// UID and one whose UID is not a topic are refused, naming the UID found.
#[test]
fn a_certificate_without_a_push_topic_is_refused() {
    for (uid, shown) in [(None, ""), (Some("not-a-topic"), "not-a-topic")] {
        let p = pair(uid, NOT_AFTER);
        let host = host(&p);
        let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
        assert_eq!(code, 2);
        assert_eq!(
            text,
            format!(
                "{CERT} is not an MDM push certificate: its subject UID is {shown:?}, not com.apple.mgmt.External.<uuid>"
            )
        );
    }
}

/// Catches an off-by-one in the expiry: at one second before `not_after` the certificate
/// passes, at `not_after` exactly it is expired, and after it is expired; a date before 1970 is
/// expired too. The clock is the injected one.
#[test]
fn the_certificate_expires_exactly_at_not_after() {
    let p = push_pair();
    let host = host(&p);
    let end = u64::try_from(NOT_AFTER).unwrap();
    go(&host, &stored(TOPIC), end - 1, with_pair(), Mode::DryRun)
        .0
        .unwrap();
    for now in [end, end + 1] {
        let (code, text) = refused(go(&host, &FakeApi::new(), now, with_pair(), Mode::DryRun).0);
        assert_eq!(code, 2);
        assert_eq!(
            text,
            format!(
                "{CERT} expired at Mar 17 17:46:40 2030 +00:00: renew it at the Apple Push Certificates Portal"
            )
        );
    }
    let old = pair(Some(TOPIC), -86_400);
    let host = self::host(&old);
    let (_, text) = refused(go(&host, &FakeApi::new(), 0, with_pair(), Mode::DryRun).0);
    assert!(text.contains("expired at"), "{text}");
}

/// Catches a key NanoHUB cannot use being sent: an encrypted key, a file that is no key at
/// all and a private-key PEM of garbage are each refused by name.
#[test]
fn an_encrypted_or_unreadable_key_is_refused() {
    let p = push_pair();
    let host = host(&p);
    let encrypted =
        "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAAAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n";
    let junk = "-----BEGIN PRIVATE KEY-----\nAAAAAAAA\n-----END PRIVATE KEY-----\n";
    for text in [encrypted, "not a key\n", junk] {
        host.put_file(KEY, 0o600, (1000, 1000), text.as_bytes());
        let (code, said) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
        assert_eq!(code, 2);
        assert_eq!(
            said,
            format!("{KEY} is encrypted or not a PEM private key (NanoHUB needs it unencrypted)")
        );
    }
}

/// Catches a key of another certificate being accepted: the pair must share their public key.
#[test]
fn a_key_of_another_certificate_is_refused() {
    let p = push_pair();
    let host = host(&p);
    host.put_file(KEY, 0o600, (1000, 1000), push_pair().key.as_bytes());
    let api = FakeApi::new();
    let (code, text) = refused(go(&host, &api, NOW, with_pair(), Mode::Apply).0);
    assert_eq!((code, text), (2, format!("{KEY} is not the key of {CERT}")));
    assert!(api.sent().is_empty());
    assert!(host.node(TOPIC_FILE).is_none());
}

/// Catches a check_pair that prints or returns the wrong facts: the topic, the expiry text and
/// the sha256 of the certificate's PEM bytes.
#[test]
fn check_pair_names_the_topic_the_expiry_and_the_certificate_digest() {
    let p = push_pair();
    let checked = check_pair(CERT, p.cert.as_bytes(), KEY, p.key.as_bytes(), NOW).unwrap();
    assert_eq!(checked.topic, topic());
    assert_eq!(checked.not_after, "Mar 17 17:46:40 2030 +00:00");
    assert_eq!(checked.sha256, sha(&p.cert));
}

/// Catches a dry run that acts: it prints the call and the plan, makes no call, writes nothing
/// and runs nothing.
#[test]
fn a_dry_run_makes_no_call_and_changes_nothing() {
    let p = push_pair();
    let host = host(&p);
    let before = host.paths();
    let api = stored(TOPIC);
    let (result, out) = go(&host, &api, NOW, with_pair(), Mode::DryRun);
    result.unwrap();
    assert!(api.sent().is_empty());
    assert_eq!(host.paths(), before);
    let bytes = expected_profile();
    let body = p.cert.len() + p.key.len();
    let want = format!(
        "{}{}DRY-RUN: PUT http://127.0.0.1:9004{PUSHCERT} (NanoHUB API, {body} bytes)\nDRY-RUN: write {TOPIC_FILE} mode 0644 owner root:root ({} bytes)\nDRY-RUN: write {SUM_FILE} mode 0644 owner root:root ({} bytes)\nDRY-RUN: write {PROFILE} mode 0640 owner root:hostkeeper-mdm ({} bytes)\ndry run: nothing changed; re-run with --apply to carry it out\n",
        pair_line(),
        profile_line(),
        TOPIC.len() + 1,
        65,
        bytes.len()
    );
    assert_eq!(out, want);
}

/// Catches an upload that is repeated or a rerun that plans: apply sends the certificate then
/// the key once, to NanoHUB's push-certificate path with its API key, writes the records and the
/// profile after the answer, and a rerun sends nothing and plans nothing.
#[test]
fn apply_uploads_once_and_a_rerun_uploads_nothing_and_plans_nothing() {
    let p = push_pair();
    let host = host(&p);
    let api = stored(TOPIC);
    let (result, out) = go(&host, &api, NOW, with_pair(), Mode::Apply);
    result.unwrap();
    let sent = api.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0].service, sent[0].port, sent[0].user),
        ("NanoHUB", 9004, "nanohub")
    );
    assert_eq!(sent[0].key, NANOHUB_KEY);
    assert_eq!(sent[0].call.method, Method::Put);
    assert_eq!(sent[0].call.path, PUSHCERT);
    assert_eq!(
        sent[0].call.body,
        format!("{}{}", p.cert, p.key).into_bytes()
    );
    assert!(out.contains(&format!("NanoHUB: push certificate stored for {TOPIC}\n")));
    let record = |path: &str| host.node(path).map(|n| (n.mode, n.uid, n.gid, n.bytes));
    assert_eq!(
        record(TOPIC_FILE),
        Some((0o644, 0, 0, format!("{TOPIC}\n").into_bytes()))
    );
    assert_eq!(
        record(SUM_FILE),
        Some((0o644, 0, 0, format!("{}\n", sha(&p.cert)).into_bytes()))
    );
    let (result, again) = go(&host, &api, NOW, with_pair(), Mode::Apply);
    result.unwrap();
    assert_eq!(api.sent().len(), 1);
    let want = format!(
        "{}NanoHUB: has this certificate already (push-cert.sha256)\n{}nothing to do\n",
        pair_line(),
        profile_line()
    );
    assert_eq!(again, want);
}

/// Catches a rerun that skips a changed input: a renewed certificate of the same topic, a
/// record that was lost, and a record of another certificate each upload again, and a profile
/// left 0600 is the only step when the records are right.
#[test]
fn a_changed_certificate_or_record_uploads_again_and_a_stale_profile_is_rewritten() {
    let p = push_pair();
    let host = host(&p);
    let api = stored(TOPIC);
    go(&host, &api, NOW, with_pair(), Mode::Apply).0.unwrap();
    assert_eq!(api.sent().len(), 1);
    // The sum record lost: the topic record alone is not enough.
    host.remove(SUM_FILE).unwrap();
    go(&host, &api, NOW, with_pair(), Mode::Apply).0.unwrap();
    assert_eq!(api.sent().len(), 2);
    // The topic record of another topic.
    host.put_file(TOPIC_FILE, 0o644, (0, 0), b"other\n");
    go(&host, &api, NOW, with_pair(), Mode::Apply).0.unwrap();
    assert_eq!(api.sent().len(), 3);
    // A renewed certificate: the same topic, another digest.
    let renewed = pair(Some(TOPIC), NOT_AFTER + 86_400);
    host.put_file(CERT, 0o644, (1000, 1000), renewed.cert.as_bytes());
    host.put_file(KEY, 0o600, (1000, 1000), renewed.key.as_bytes());
    go(&host, &api, NOW, with_pair(), Mode::Apply).0.unwrap();
    assert_eq!(api.sent().len(), 4);
    assert_eq!(
        host.node(SUM_FILE).unwrap().bytes,
        format!("{}\n", sha(&renewed.cert)).into_bytes()
    );
    // Records right, profile stale: one write, no call.
    host.put_file(PROFILE, 0o600, (SVC, SVC), b"old");
    let (result, out) = go(&host, &api, NOW, with_pair(), Mode::Apply);
    result.unwrap();
    assert_eq!(api.sent().len(), 4);
    assert_eq!(out.matches("+ write ").count(), 1, "{out}");
    let node = host.node(PROFILE).unwrap();
    assert_eq!((node.mode, node.uid, node.gid), (0o640, 0, SVC));
}

/// Catches records written for a certificate NanoHUB stored under another topic: an answer
/// naming another topic, one that is not JSON and one with no topic each fail (exit 1) with both
/// topics, and neither record nor the profile is written.
#[test]
fn nanohub_answering_another_topic_writes_no_record() {
    let other = "com.apple.mgmt.External.ffffffff-ffff-ffff-ffff-ffffffffffff";
    for (body, got) in [
        (format!("{{\"topic\":\"{other}\"}}"), other),
        ("stored".to_owned(), ""),
        ("{\"id\":1}".to_owned(), ""),
        ("{\"topic\":5}".to_owned(), ""),
    ] {
        let p = push_pair();
        let host = host(&p);
        let api = FakeApi::new();
        api.reply(Method::Put, PUSHCERT, 200, body.as_bytes());
        let (code, text) = refused(go(&host, &api, NOW, with_pair(), Mode::Apply).0);
        assert_eq!(code, 1);
        assert_eq!(
            text,
            format!(
                "NanoHUB answered topic {got:?} for a certificate whose topic is {TOPIC}: the records were not written"
            )
        );
        assert!(host.node(TOPIC_FILE).is_none());
        assert!(host.node(SUM_FILE).is_none());
        assert!(host.node(PROFILE).is_none());
    }
}

/// Catches records written although NanoHUB refused the certificate: a refusal (status 500)
/// and a call that did not complete each fail (exit 1) and leave the host as it was.
#[test]
fn nanohub_refusing_the_certificate_writes_no_record() {
    let p = push_pair();
    let host = host(&p);
    let before = host.paths();
    let api = FakeApi::new();
    api.reply(Method::Put, PUSHCERT, 500, b"bad certificate\n");
    let (code, text) = refused(go(&host, &api, NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    assert_eq!(
        text,
        format!(
            "PUT http://127.0.0.1:9004{PUSHCERT} (NanoHUB API, {} bytes): answered 500: bad certificate",
            p.cert.len() + p.key.len()
        )
    );
    assert_eq!(host.paths(), before);
    let api = FakeApi::new();
    api.fail(Method::Put, PUSHCERT, "connection refused");
    let (code, text) = refused(go(&host, &api, NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    assert!(text.ends_with(": connection refused"), "{text}");
    assert_eq!(host.paths(), before);
}

/// Catches an upload attempted with no NanoHUB key: the nanohub.env that is missing refuses
/// the run (exit 2) before any call.
#[test]
fn an_upload_needs_the_nanohub_api_key() {
    let p = push_pair();
    let host = host(&p);
    host.remove(&format!("{ETC}/nanohub.env")).unwrap();
    let api = stored(TOPIC);
    let (code, text) = refused(go(&host, &api, NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 2);
    assert_eq!(
        text,
        format!("{ETC}/nanohub.env has no NANOHUB_API_KEY= line with a value")
    );
    assert!(api.sent().is_empty());
}

/// Catches `--profile-only` that invents a topic: with no `--topic` and no record it is refused
/// (exit 2) and says to run push-cert with the pair first; a recorded topic (trimmed) is used,
/// and a recorded text that is not a topic is refused.
#[test]
fn profile_only_takes_the_recorded_topic_and_refuses_when_there_is_none() {
    let p = push_pair();
    let host = host(&p);
    let (code, text) = refused(
        go(
            &host,
            &FakeApi::new(),
            NOW,
            With::ProfileOnly(None),
            Mode::Apply,
        )
        .0,
    );
    assert_eq!(code, 2);
    assert_eq!(
        text,
        format!(
            "{TOPIC_FILE} is not there: run push-cert --cert PEM --key PEM first, or give --topic with --profile-only"
        )
    );
    assert!(host.node(PROFILE).is_none());
    host.put_file(TOPIC_FILE, 0o644, (0, 0), b"  not a topic \n");
    let (code, text) = refused(
        go(
            &host,
            &FakeApi::new(),
            NOW,
            With::ProfileOnly(None),
            Mode::Apply,
        )
        .0,
    );
    assert_eq!(code, 2);
    assert_eq!(
        text,
        "topic \"not a topic\" is not com.apple.mgmt.External.<uuid>"
    );
    host.put_file(TOPIC_FILE, 0o644, (0, 0), format!("{TOPIC}\n").as_bytes());
    let api = FakeApi::new();
    let (result, out) = go(&host, &api, NOW, With::ProfileOnly(None), Mode::Apply);
    result.unwrap();
    assert!(out.starts_with(&profile_line()), "{out}");
    assert_eq!(host.node(PROFILE).unwrap().bytes, expected_profile());
    assert!(api.sent().is_empty());
}

/// Catches `--topic` ignored in favour of the record: the profile names the given topic.
#[test]
fn profile_only_topic_wins_over_the_record() {
    let p = push_pair();
    let host = host(&p);
    host.put_file(TOPIC_FILE, 0o644, (0, 0), b"not a topic\n");
    let (result, _) = go(
        &host,
        &FakeApi::new(),
        NOW,
        With::ProfileOnly(Some(topic())),
        Mode::Apply,
    );
    result.unwrap();
    assert_eq!(host.node(PROFILE).unwrap().bytes, expected_profile());
}

/// Catches a profile made without a usable challenge or service user: a missing scep.env, a
/// challenge with a character the profile refuses and a missing service user are each refused
/// (exit 2) before any change.
#[test]
fn the_profile_needs_the_challenge_and_the_service_user() {
    let p = push_pair();
    let host = host(&p);
    let scep = format!("{ETC}/scep.env");
    let with = || With::ProfileOnly(Some(topic()));
    host.put_file(&scep, 0o600, (0, 0), b"SCEP_CHALLENGE_PASSWORD=a b!\n");
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with(), Mode::Apply).0);
    assert_eq!(code, 2);
    assert_eq!(
        text,
        "the SCEP challenge is empty or holds a character other than letters, digits, _ and -"
    );
    host.remove(&scep).unwrap();
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with(), Mode::Apply).0);
    assert_eq!(
        (code, text),
        (
            2,
            format!("{scep} has no SCEP_CHALLENGE_PASSWORD= line with a value")
        )
    );
    host.put_file(&scep, 0o600, (0, 0), b"SCEP_CHALLENGE_PASSWORD=abc\n");
    host.reply(&[GETENT, "passwd", "hostkeeper-mdm"], 2, "");
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, with(), Mode::Apply).0);
    assert_eq!(
        (code, text),
        (
            2,
            "the user and group hostkeeper-mdm are not there: run hostkeeper-mdm install --apply first"
                .to_owned()
        )
    );
    assert!(host.node(PROFILE).is_none());
}

/// Catches a host that cannot be looked at being read as empty: a www directory that cannot be
/// examined, a certificate that cannot be read and a failing key stat fail the run (exit 1) with
/// the system's error rather than going on.
#[test]
fn a_host_that_cannot_be_read_fails_the_run() {
    let p = push_pair();
    let host = host(&p);
    host.deny_stat(&format!("{STATE}/www"));
    let (code, _) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    let host = self::host(&p);
    host.deny_read(CERT);
    let (code, _) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    let host = self::host(&p);
    host.deny_read(KEY);
    let (code, _) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    let host = self::host(&p);
    host.deny_stat(KEY);
    let (code, _) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    let host = self::host(&p);
    host.deny_stat("/srv/keys/.git");
    let (code, _) = refused(go(&host, &FakeApi::new(), NOW, with_pair(), Mode::Apply).0);
    assert_eq!(code, 1);
    let host = self::host(&p);
    host.deny_read(&format!("{STATE}/push-topic"));
    let (code, _) = refused(
        go(
            &host,
            &FakeApi::new(),
            NOW,
            With::ProfileOnly(None),
            Mode::Apply,
        )
        .0,
    );
    assert_eq!(code, 1);
}

/// Catches a git-tree check that a relative path or a link gets past: a relative `--key` is
/// refused as not absolute before anything is read, and a key reached through a link into a
/// work tree is refused with the `.git` of the link's target, the path named as given.
#[test]
fn a_relative_path_or_a_link_into_a_work_tree_is_refused() {
    let p = push_pair();
    let host = host(&p);
    let relative = With::Pair {
        cert: CERT.to_owned(),
        key: "keys/push.key".to_owned(),
    };
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, relative, Mode::DryRun).0);
    assert_eq!(code, 2);
    assert!(text.contains("keys/push.key"), "{text}");
    host.put_dir("/repo/.git", 0o755, (1000, 1000));
    host.put_dir("/repo/keys", 0o755, (1000, 1000));
    host.put_file("/repo/keys/push.key", 0o600, (1000, 1000), p.key.as_bytes());
    host.put_symlink("/srv/linked", "/repo/keys");
    let linked = With::Pair {
        cert: CERT.to_owned(),
        key: "/srv/linked/push.key".to_owned(),
    };
    let (code, text) = refused(go(&host, &FakeApi::new(), NOW, linked, Mode::DryRun).0);
    assert_eq!(code, 2);
    assert_eq!(
        text,
        "/srv/linked/push.key is inside a git work tree (/repo/.git exists): keep keys, tokens and certificates outside every repository"
    );
}
