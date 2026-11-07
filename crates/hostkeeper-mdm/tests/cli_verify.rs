//! `verify` through the command line: how the ACME settings are read, and which directories
//! each verb trusts, on the fake host.

mod common;

use common::{FakeMachine, FakeNet, example, kit, ready_host, with_system};
use hostkeeper_mdm::{Grant, run_with};
use hostkeeper_mdm_step::Trustee;
use hostkeeper_mdm_step::fake::FakeHost;

/// Runs the command line `words` on `host`; the exit code, standard output, standard error and
/// the grants each host the verb asked the machine for was made with.
fn cli(host: &FakeHost, words: &[&str]) -> (u8, String, String, Vec<Vec<Grant>>) {
    let kit = kit();
    let net = FakeNet::new(Default::default());
    let args: Vec<String> = std::iter::once("hostkeeper-mdm")
        .chain(words.iter().copied())
        .map(str::to_owned)
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let machine = FakeMachine::new(host);
    let code = with_system(&machine, &net, |system| {
        run_with(&args, system, &kit.pins, &mut out, &mut err)
    });
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        machine.grants.into_inner(),
    )
}

/// The example settings with the line of `key` replaced by `with` (dropped when `None`).
fn example_with(key: &str, with: Option<&str>) -> String {
    let prefix = format!("{key}=");
    let mut found = false;
    let text: String = example()
        .lines()
        .filter_map(|l| match l.starts_with(&prefix) {
            true => {
                found = true;
                with.map(|w| format!("{key}={w}\n"))
            }
            false => Some(format!("{l}\n")),
        })
        .collect();
    assert!(found, "{key} is in the example");
    text
}

fn machine_with_settings(text: &str) -> FakeHost {
    let host = ready_host(&kit());
    host.put_file("/cfg", 0o644, (0, 0), text.as_bytes());
    host
}

const ACME_KEYS: [&str; 5] = [
    "MDM_ACME_EMAIL",
    "MDM_ACME_SERVER",
    "MDM_ACME_DNS_PROVIDER",
    "MDM_ACME_RESOLVERS",
    "MDM_ACME_PROPAGATION_WAIT",
];

/// A host with no service user yet and the example settings in `/cfg`.
fn host_without_the_user() -> FakeHost {
    let host = FakeHost::new(0);
    host.reply(&["/usr/bin/getent", "passwd", "hostkeeper-mdm"], 2, "");
    host.reply(&["/usr/bin/getent", "group", "hostkeeper-mdm"], 2, "");
    host.put_file("/cfg", 0o644, (0, 0), example().as_bytes());
    host
}

/// Catches ACME settings that are present but malformed being read as absent: verify refuses
/// (exit 2) with the setting's message and prints no check, so the certificate job's units
/// cannot be silently left out of the comparison.
#[test]
fn verify_refuses_a_malformed_acme_setting() {
    let host = machine_with_settings(&example_with("MDM_ACME_PROPAGATION_WAIT", Some("soon")));
    let (code, out, err, _) = cli(&host, &["verify", "--settings", "/cfg"]);
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "REFUSED: MDM_ACME_PROPAGATION_WAIT=soon: not a number of seconds above 0 with an s, like 300s\n"
    );
}

/// Catches ACME settings with a key left out being read as absent: with the others set, the
/// missing key is named in the refusal, for each of the five keys.
#[test]
fn verify_refuses_acme_settings_with_a_key_missing() {
    for key in ACME_KEYS {
        let host = machine_with_settings(&example_with(key, None));
        let (code, out, err, _) = cli(&host, &["verify", "--settings", "/cfg"]);
        assert_eq!((code, out.as_str()), (2, ""), "{key}");
        assert_eq!(err, format!("REFUSED: {key} is not set (settings file)\n"));
    }
}

/// Catches a key set empty being taken for a key not set: an ACME key given as `KEY=` counts as
/// set, so the ACME settings are read and refused.
#[test]
fn verify_refuses_an_acme_setting_that_is_set_empty() {
    let host = machine_with_settings(&example_with("MDM_ACME_EMAIL", Some("")));
    let (code, out, err, _) = cli(&host, &["verify", "--settings", "/cfg"]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert_eq!(err, "REFUSED: MDM_ACME_EMAIL is not set (settings file)\n");
}

/// Catches the certificate job's units dropped when the ACME settings are whole: with all five
/// keys valid the units are compared, not skipped.
#[test]
fn verify_compares_the_certificate_units_when_the_acme_settings_are_whole() {
    let host = machine_with_settings(&example());
    let (code, out, err, _) = cli(&host, &["verify", "--settings", "/cfg"]);
    assert_eq!(err, "");
    assert_eq!(code, 1, "{out}");
    assert!(
        !out.contains("skipped  the certificate job's units"),
        "{out}"
    );
    assert!(
        out.contains("missing    /etc/systemd/system/hostkeeper-mdm-cert.service"),
        "{out}"
    );
}

/// Catches ACME settings demanded when none is set: with no MDM_ACME_ key the certificate
/// job's units are skipped, said so, and the run is not refused.
#[test]
fn verify_skips_the_certificate_units_when_no_acme_key_is_set() {
    let mut text = example();
    for key in ACME_KEYS {
        text = text
            .lines()
            .filter(|l| !l.starts_with(&format!("{key}=")))
            .map(|l| format!("{l}\n"))
            .collect();
    }
    let host = machine_with_settings(&text);
    let (code, out, err, _) = cli(&host, &["verify", "--settings", "/cfg"]);
    assert_eq!((code, err.as_str()), (1, ""));
    assert!(
        out.contains("skipped  the certificate job's units: the settings have no ACME keys\n"),
        "{out}"
    );
}

fn grant(dir: &str, uid: u32) -> Grant {
    Grant {
        dir: dir.to_owned(),
        who: Trustee::Uid(uid),
    }
}

/// The grant of the state directory to the service user, by name.
fn state() -> Grant {
    Grant {
        dir: "/var/lib/hostkeeper-mdm".to_owned(),
        who: Trustee::User("hostkeeper-mdm".to_owned()),
    }
}

/// Catches the state directory trusted for the wrong user or not at all: install grants
/// MDM_STATE to the service user by name and `--bin-dir` to the uid that owns it, the grants
/// the verb's host is made with (after the grant-free host that reads the settings).
#[test]
fn install_trusts_the_state_for_the_service_user_and_the_bin_dir_for_its_owner() {
    let host = machine_with_settings(&example());
    host.put_dir("/srv/fetch", 0o755, (1000, 1000));
    let (code, _, err, grants) = cli(
        &host,
        &["install", "--settings", "/cfg", "--bin-dir", "/srv/fetch"],
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        grants,
        vec![vec![], vec![state(), grant("/srv/fetch", 1000)]]
    );
}

/// Catches a bin-dir trusted when there is nothing to trust, or a state grant that waits for
/// the service user to exist before the run (the plan makes that user, then writes below its
/// state): with no service user the state is still granted to it by name (a name that trusts
/// nobody until the user exists), and a `--bin-dir` that is not there is granted to no one.
#[test]
fn install_grants_the_state_by_name_and_nothing_for_a_bin_dir_that_does_not_exist() {
    let host = host_without_the_user();
    host.put_dir("/srv/op", 0o755, (1000, 1000));
    let install = |dir: &str| cli(&host, &["install", "--settings", "/cfg", "--bin-dir", dir]).3;
    assert_eq!(
        install("/srv/op"),
        vec![vec![], vec![state(), grant("/srv/op", 1000)]]
    );
    assert_eq!(install("/srv/none"), vec![vec![], vec![state()]]);
}

/// Catches uninstall, tls and verify trusting more or less than the state directory: each makes
/// its host with MDM_STATE granted to the service user by name and nothing else, whether or
/// not that user exists yet.
#[test]
fn uninstall_tls_and_verify_trust_only_the_state_for_the_service_user() {
    let verbs: [&[&str]; 3] = [
        &["install", "--settings", "/cfg", "--uninstall"],
        &["tls", "--settings", "/cfg"],
        &["verify", "--settings", "/cfg"],
    ];
    let host = machine_with_settings(&example());
    for words in verbs {
        let (_, _, _, grants) = cli(&host, words);
        assert_eq!(grants, vec![vec![], vec![state()]], "{words:?}");
    }
    let bare = host_without_the_user();
    for words in verbs {
        let (_, _, _, grants) = cli(&bare, words);
        assert_eq!(grants, vec![vec![], vec![state()]], "{words:?}");
    }
}
