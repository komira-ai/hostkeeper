//! `hkctl ssh pin-host-keys` and `hkctl ssh host-keys`, as the real script sends them: the path
//! each posts to, the request body curl reads, and how an answer and a refusal end.

use crate::rig::{Ran, Rig, Script, args};

/// The paths of the calls on the coordinator the rig's global flags name.
const PIN: &str = "https://127.0.0.1:1/hostkeeper.operator.v1.Ssh/PinHostKeys";
const READ: &str = "https://127.0.0.1:1/hostkeeper.operator.v1.Ssh/HostKeys";

/// Two fingerprints of the form the coordinator takes: `SHA256:` and 43 base64 characters.
const ED25519: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const ECDSA: &str = "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

/// Runs the real script with `ssh <verb>` and `flags` after the global flags of a call to a
/// coordinator at a port nothing listens on, the fake curl answering `status` with `body`: what
/// it ended with, and the target and body of the one call it posted.
fn ssh(name: &str, verb: &str, flags: &[&str], status: u16, body: &str) -> (Ran, String, String) {
    let rig = Rig::new(name);
    rig.reply(status, body);
    let mut all = rig.globals(1);
    all.extend(args(&["ssh", verb]));
    all.extend(args(flags));
    let ran = rig.run(Script::Real, &all, b"", &[]);
    let curls = rig.curls();
    assert_eq!(curls.len(), 1, "{curls:#?}");
    let url = curls[0]
        .argv
        .iter()
        .skip_while(|a| *a != "--url")
        .nth(1)
        .expect("a url")
        .clone();
    let sent = curls[0]
        .read_after("--data-binary", 0)
        .expect("the body is read from a pipe");
    (ran, url, sent)
}

/// Proves: `ssh pin-host-keys --host-id H --fingerprints A --fingerprints B` posts to
/// `Ssh/PinHostKeys` a body holding exactly the host id and both fingerprints, in the order
/// given, under their JSON names, exits 0 and prints the coordinator's answer (the pinned set
/// and its state) on standard output.
///
/// Catches: a repeated flag that keeps only its last value, a flag that does not reach its
/// field, or a call posted to another method of `Ssh`.
#[test]
fn pin_host_keys_posts_the_host_and_every_fingerprint() {
    let answer = format!("{{\"fingerprints\":[\"{ED25519}\",\"{ECDSA}\"],\"state\":\"pinned\"}}");
    let (ran, url, sent) = ssh(
        "ssh-pin-posted",
        "pin-host-keys",
        &[
            "--host-id",
            "node-4",
            "--fingerprints",
            ED25519,
            "--fingerprints",
            ECDSA,
        ],
        200,
        &answer,
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(url, PIN);
    assert_eq!(
        sent,
        format!("{{\"hostId\":\"node-4\",\"fingerprints\":[\"{ED25519}\",\"{ECDSA}\"]}}")
    );
}

/// Proves: the refusal `not_reported` (HTTP 400, `FAILED_PRECONDITION`, as the coordinator
/// answers a set that is not what the host's last heartbeat reported) makes `hkctl` exit 2 with
/// the code and the refusal on standard error, whole, and nothing on standard output; the one
/// fingerprint given is sent as a list of one.
///
/// Catches: a refused pin that exits 0 or whose reason is lost, or a single repeated value sent
/// as a bare string.
#[test]
fn a_refused_pin_exits_2_and_names_not_reported() {
    let (ran, url, sent) = ssh(
        "ssh-pin-refused",
        "pin-host-keys",
        &["--host-id", "node-4", "--fingerprints", ED25519],
        400,
        "{\"code\":\"FAILED_PRECONDITION\",\"reason\":\"not_reported\"}",
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            2,
            "",
            "hkctl: the coordinator answered FAILED_PRECONDITION\nrefused: not_reported\n"
        )
    );
    assert_eq!(url, PIN);
    assert_eq!(
        sent,
        format!("{{\"hostId\":\"node-4\",\"fingerprints\":[\"{ED25519}\"]}}")
    );
}

/// Proves: `ssh host-keys --host-id H` posts to `Ssh/HostKeys` a body holding exactly the host
/// id, exits 0 and prints the answer, here a host with no pin.
///
/// Catches: the read posted to the pin method, or the answer not printed.
#[test]
fn host_keys_posts_the_host_and_prints_its_state() {
    let answer = "{\"state\":\"no_pin\"}";
    let (ran, url, sent) = ssh(
        "ssh-host-keys",
        "host-keys",
        &["--host-id", "node-4"],
        200,
        answer,
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(url, READ);
    assert_eq!(sent, "{\"hostId\":\"node-4\"}");
}
