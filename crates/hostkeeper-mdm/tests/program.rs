//! `mdm`, the program of the `mdm` package, through its command line on the fake host: its
//! verbs, and `push-cert` and `dep` re-pointed from `hostkeeper-mdm` at the package's layout.

mod common;
mod hook_support;
mod push_cert_support;

use std::ffi::OsString;

use common::{FakeApi, FakeMachine, FakeNet, SVC, random_hex, with_system_api};
use hook_support::{ETC, STATE, package_example};
use hostkeeper_mdm::api::Method;
use hostkeeper_mdm_step::DRY_RUN_NOTE;
use hostkeeper_mdm_step::fake::FakeHost;
use push_cert_support::{CERT, KEY, NANOHUB_KEY, PUSHCERT, TOPIC, push_pair};

const SETTINGS: &str = "/etc/mdm/settings";
const PROFILE: &str = "/var/lib/mdm/www/enroll.mobileconfig";
const TOPIC_FILE: &str = "/var/lib/mdm/push-topic";
const DEP_KEY: &str = "dep-api-key-1234";
const CERT_PATH: &str = "/v1/tokenpki/example?cn=hostkeeper-mdm&validity_days=7";

/// Runs `mdm <words>` on `host` with `api` answering: the exit code, stdout and stderr.
fn mdm(host: &FakeHost, api: &FakeApi, words: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = std::iter::once("mdm")
        .chain(words.iter().copied())
        .map(OsString::from)
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let machine = FakeMachine::new(host);
    let net = FakeNet::new(Default::default());
    let code = with_system_api(&machine, &net, api, |system| {
        hostkeeper_mdm::program::run(&args, system, &mut out, &mut err)
    });
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// A root host on which the package's `prepare` has run: the user, the state with `www`, the
/// settings and secrets under `/etc/mdm`, and the operator's push certificate and key in a
/// directory of their own.
fn installed() -> FakeHost {
    let host = FakeHost::new(0);
    host.add_user("mdm", SVC);
    host.put_dir(STATE, 0o700, (SVC, SVC));
    host.put_dir(&format!("{STATE}/www"), 0o700, (SVC, SVC));
    host.put_dir(ETC, 0o750, (0, SVC));
    host.put_file(SETTINGS, 0o600, (0, 0), package_example().as_bytes());
    let scep = format!("SCEP_CHALLENGE_PASSWORD={}\n", random_hex());
    host.put_file(&format!("{ETC}/scep.env"), 0o600, (0, 0), scep.as_bytes());
    let hub = format!("NANOHUB_API_KEY={NANOHUB_KEY}\n");
    host.put_file(&format!("{ETC}/nanohub.env"), 0o600, (0, 0), hub.as_bytes());
    let dep = format!("NANODEP_API={DEP_KEY}\n");
    host.put_file(&format!("{ETC}/nanodep.env"), 0o600, (0, 0), dep.as_bytes());
    let pair = push_pair();
    host.put_dir("/srv/keys", 0o755, (1000, 1000));
    host.put_file(CERT, 0o644, (1000, 1000), pair.cert.as_bytes());
    host.put_file(KEY, 0o600, (1000, 1000), pair.key.as_bytes());
    host.put_dir("/ops", 0o755, (1000, 1000));
    host
}

/// Catches a program that is not the package's: `--version` names `mdm` and the workspace's
/// version, and the help lists exactly the hook and the Apple-facing verbs, none of the kit's
/// install, fetch, tls, verify or migrate.
#[test]
fn the_program_names_itself_mdm_and_has_only_the_hook_and_the_apple_verbs() {
    let host = FakeHost::new(0);
    let (code, out, err) = mdm(&host, &FakeApi::new(), &["--version"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        out.starts_with(&format!("mdm {}", env!("CARGO_PKG_VERSION"))),
        "{out}"
    );
    let (code, out, _) = mdm(&host, &FakeApi::new(), &["--help"]);
    assert_eq!(code, 0);
    let verbs: Vec<&str> = out
        .split("Commands:\n")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(verbs, ["update-hook", "push-cert", "dep", "help"]);
    for words in [&[][..], &["install"], &["fetch"], &["verify"], &["migrate"]] {
        let (code, out, err) = mdm(&host, &FakeApi::new(), words);
        assert_eq!((code, out.as_str()), (2, ""), "{words:?}");
        assert!(err.contains("Usage: mdm"), "{words:?}: {err}");
    }
    assert!(host.calls().is_empty());
}

/// Catches `mdm push-cert` reading another layout than the package's, or not wired to the
/// kit's verb: the dry run prints the call to NanoHUB on the package's port and changes
/// nothing; `--apply` uploads with the key of `/etc/mdm/nanohub.env` and writes the topic and
/// the enrollment profile under `/var/lib/mdm`; `--profile-only` then rewrites nothing.
#[test]
fn push_cert_runs_on_the_packages_layout() {
    let host = installed();
    let api = FakeApi::new();
    api.reply(
        Method::Put,
        PUSHCERT,
        200,
        format!("{{\"topic\":\"{TOPIC}\"}}").as_bytes(),
    );
    let words = |apply: bool| {
        let mut w = vec![
            "push-cert",
            "--settings",
            SETTINGS,
            "--cert",
            CERT,
            "--key",
            KEY,
        ];
        if apply {
            w.push("--apply");
        }
        w
    };
    let before = host.paths();
    let (code, out, err) = mdm(&host, &api, &words(false));
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.contains("DRY-RUN: PUT http://127.0.0.1:9004"), "{out}");
    assert!(out.ends_with(&format!("{DRY_RUN_NOTE}\n")), "{out}");
    assert!(api.sent().is_empty());
    assert_eq!(host.paths(), before);

    let (code, out, err) = mdm(&host, &api, &words(true));
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    let sent = api.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].port, sent[0].key.as_str()), (9004, NANOHUB_KEY));
    assert_eq!(
        host.node(TOPIC_FILE).unwrap().bytes,
        format!("{TOPIC}\n").into_bytes()
    );
    let profile = host.node(PROFILE).unwrap();
    assert_eq!((profile.mode, profile.uid, profile.gid), (0o640, 0, SVC));

    let (code, out, err) = mdm(
        &host,
        &api,
        &[
            "push-cert",
            "--settings",
            SETTINGS,
            "--profile-only",
            "--apply",
        ],
    );
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.ends_with("nothing to do\n"), "{out}");
}

/// Catches `mdm dep` reading another layout than the package's, or not wired to the kit's
/// verbs: `get-cert` asks NanoDEP on the package's port with the key of `/etc/mdm/nanodep.env`
/// and writes the certificate; `assign` without a recorded profile is refused (exit 2) naming
/// the package's record file.
#[test]
fn dep_runs_on_the_packages_layout() {
    let host = installed();
    let api = FakeApi::new();
    let (code, out, err) = mdm(
        &host,
        &api,
        &[
            "dep",
            "get-cert",
            "--settings",
            SETTINGS,
            "--out",
            "/ops/nanodep.crt",
        ],
    );
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(
        out.starts_with(&format!(
            "DRY-RUN: GET http://127.0.0.1:9001{CERT_PATH} (NanoDEP API)\n"
        )),
        "{out}"
    );
    assert!(api.sent().is_empty());

    let (code, out, err) = mdm(
        &host,
        &api,
        &[
            "dep",
            "assign",
            "--settings",
            SETTINGS,
            "--serial",
            "C02ABCDEF1",
        ],
    );
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(
        err.starts_with("REFUSED: ") && err.contains("/var/lib/mdm/dep-profile-uuid"),
        "{err}"
    );
    assert!(api.sent().is_empty());
}

/// Catches a settings file the package cannot run being read anyway: one that gives a key the
/// package fixes another value (the kit's state directory) is refused by every verb with exit
/// 2, naming the key, both values and what to do, before any call; one that gives the package's
/// own value is read.
#[test]
fn a_settings_file_that_moves_the_packages_layout_is_refused() {
    let host = installed();
    let api = FakeApi::new();
    let moved = format!("{}MDM_STATE=/var/lib/hostkeeper-mdm\n", package_example());
    host.put_file(SETTINGS, 0o600, (0, 0), moved.as_bytes());
    let refusal = "REFUSED: MDM_STATE is \"/var/lib/hostkeeper-mdm\", but the mdm package fixes it to \"/var/lib/mdm\": leave it out or set that value\n";
    for words in [
        &["push-cert", "--settings", SETTINGS, "--profile-only"][..],
        &["dep", "define-profile", "--settings", SETTINGS],
    ] {
        let (code, out, err) = mdm(&host, &api, words);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (2, "", refusal),
            "{words:?}"
        );
    }
    let same = format!(
        "{}MDM_STATE=/var/lib/mdm\nMDM_USER=\"mdm\"\n",
        package_example()
    );
    host.put_file(SETTINGS, 0o600, (0, 0), same.as_bytes());
    let (code, _, err) = mdm(
        &host,
        &api,
        &[
            "dep",
            "get-cert",
            "--settings",
            SETTINGS,
            "--out",
            "/ops/nanodep.crt",
        ],
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(api.sent().is_empty());
}
