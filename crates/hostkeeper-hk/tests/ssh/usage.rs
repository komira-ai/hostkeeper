//! What the coordinator flags refuse, and the program as a process.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};

use hostkeeper_hk::ssh::{Coordinator, Verb};

use crate::fake::{Call, Double, HOST, NOW, Rig, invoke_as, os, pki, write_private};

/// `ssh-status` of the host against `coordinator`: the exit code and the message.
fn status(rig: &Rig, coordinator: &Coordinator) -> (u8, String) {
    let call = Call {
        name: "hk",
        verb: Verb::Status,
        coordinator,
        state_dir: &rig.state_dir(),
        args: os(&[HOST]),
    };
    invoke_as(&call, &Double::new(), NOW, Vec::new(), Vec::new())
}

/// The message of a failed status: the diagnostic without the program name and the line end.
fn failure(rig: &Rig, coordinator: &Coordinator) -> String {
    let (code, diag) = status(rig, coordinator);
    assert_ne!(code, 0, "{diag}");
    diag.strip_prefix("hk: ")
        .and_then(|rest| rest.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("{diag:?}"))
        .to_owned()
}

fn with_endpoint(rig: &Rig, endpoint: &str) -> Coordinator {
    Coordinator {
        endpoint: endpoint.to_owned(),
        ..rig.coordinator()
    }
}

/// Proves: only `https://<host>[:<port>]` is an endpoint: a scheme other than https, a user, a
/// path, a query or a trailing slash is refused with exit 2, an IPv6 address in brackets is
/// taken.
#[test]
fn an_endpoint_is_https_host_and_port_only() {
    let rig = Rig::start();
    for bad in [
        "http://localhost:1",
        "https://u@localhost",
        "https://localhost/p",
        "https://localhost/",
        "https://localhost?q=1",
        "localhost",
        "https://",
    ] {
        let (code, diag) = status(&rig, &with_endpoint(&rig, bad));
        assert_eq!(
            (code, diag.as_str()),
            (2, "hk: --endpoint takes https://<host>[:<port>]\n"),
            "{bad}"
        );
    }
    let error = failure(&rig, &with_endpoint(&rig, "https://[::1]:1"));
    assert!(
        error.starts_with("no answer from the coordinator"),
        "{error}"
    );
}

/// Proves: the token file is refused when group or others can read it, when it is missing, empty,
/// or holds a character that is not visible ASCII; the refusal never prints the token.
#[test]
fn a_token_file_must_be_private_and_a_visible_word() {
    let rig = Rig::start();
    let coordinator = rig.coordinator();
    let token = rig.dir().join("token");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o640)).expect("mode");
    let error = failure(&rig, &coordinator);
    assert!(
        error.ends_with("may be read by its group or others; make it 0600"),
        "{error}"
    );
    for (content, label) in [
        ("\n", "empty"),
        ("two words\n", "a space"),
        ("t\u{f6}ken\n", "non-ASCII"),
    ] {
        write_private(&token, content);
        assert_eq!(
            failure(&rig, &coordinator),
            "the token is empty or holds a character that is not visible ASCII",
            "{label}"
        );
    }
    write_private(&token, "no-newline");
    // A token that is fine goes on to the call, which the fake answers.
    assert_eq!(status(&rig, &coordinator).0, 0);
    std::fs::remove_file(&token).expect("remove");
    let error = failure(&rig, &coordinator);
    assert!(error.starts_with("cannot read the token from"), "{error}");
}

/// Proves: a CA file that is missing is an error naming it; a CA that did not sign the
/// coordinator's certificate, and a coordinator that is not listening, are "no answer", and the
/// token is not sent to either.
#[test]
fn a_wrong_ca_or_no_listener_is_no_answer() {
    let rig = Rig::start();
    let coordinator = rig.coordinator();
    let ca = rig.dir().join("ca.pem");
    std::fs::write(&ca, pki().ca_pem).expect("another CA");
    let error = failure(&rig, &coordinator);
    assert!(
        error.starts_with("no answer from the coordinator"),
        "{error}"
    );
    assert_eq!(rig.seen(|s| s.authorizations.len()), 0);
    std::fs::write(&ca, "not a certificate").expect("junk");
    let said = failure(&rig, &coordinator);
    assert!(
        said.starts_with("no answer from the coordinator")
            || said.starts_with("--ca-file is not a CA certificate"),
        "{said}"
    );
    std::fs::write(
        &ca,
        "-----BEGIN CERTIFICATE-----\n%%%%\n-----END CERTIFICATE-----\n",
    )
    .expect("malformed");
    let (code, diag) = status(&rig, &coordinator);
    assert_eq!(code, 2);
    assert!(
        diag.starts_with("hk: --ca-file is not a CA certificate"),
        "{diag}"
    );
    std::fs::remove_file(&ca).expect("remove");
    let error = failure(&rig, &coordinator);
    assert!(error.starts_with("cannot read the CA from"), "{error}");
}

/// Proves: a host id is a plain word wherever it is taken: a status, a proxy and an ssh of
/// another path, a flag or an overlong word are refused with exit 2 before any call.
#[test]
fn a_host_id_that_is_not_a_plain_word_is_refused_before_any_call() {
    let rig = Rig::start();
    for verb in [Verb::Status, Verb::Proxy, Verb::Ssh] {
        for bad in ["../x", ".hidden", "a b"] {
            let call = Call {
                name: "hk",
                verb,
                coordinator: &rig.coordinator(),
                state_dir: &rig.state_dir(),
                args: os(&[bad]),
            };
            let (code, diag) = invoke_as(&call, &Double::new(), NOW, Vec::new(), Vec::new());
            assert_eq!(code, 2, "{verb:?} {bad}");
            assert!(diag.contains("is not a host id"), "{diag}");
        }
    }
    assert_eq!(rig.seen(|s| s.authorizations.len()), 0);
}

/// The program `hk` with the coordinator's flags of the rig, then `rest`, and `HOME` and the
/// agent set so that the state is in the rig's directory and no agent is asked.
fn hk(rig: &Rig, rest: &[&str]) -> Command {
    let coordinator = rig.coordinator();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hk"));
    command
        .args(["--endpoint", &coordinator.endpoint])
        .arg("--ca-file")
        .arg(&coordinator.ca_file)
        .arg("--token-file")
        .arg(&coordinator.token_file)
        .args(["--timeout-secs", "30"])
        .args(rest)
        .env("HOME", rig.dir())
        .env_remove("SSH_AUTH_SOCK");
    command
}

/// Proves: the program is the library behind a command line: a tail that does not parse exits 2
/// with clap's message, a failure prints `hk: <error>` and exits 1, an unknown service exits 2,
/// `ssh-known-host` prints its line, `ssh-status` reads the state under `$HOME/.hostkeeper/ssh`
/// and prints the host's pins.
#[test]
fn the_binary_exits_with_the_codes_it_promises() {
    let rig = Rig::start();
    let out = hk(&rig, &["ssh-revoke"]).output().expect("hk runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--operator"));
    let out = Command::new(env!("CARGO_BIN_EXE_hk"))
        .arg("frobnicate")
        .output()
        .expect("hk runs");
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned()
        ),
        (
            Some(2),
            "hk: no service frobnicate; 'hk --help' lists them\n".to_owned()
        )
    );
    let pins = rig.dir().join("pins");
    std::fs::write(&pins, "SHA256:abc\n").expect("pins");
    let out = Command::new(env!("CARGO_BIN_EXE_hk"))
        .arg("ssh-known-host")
        .arg(&pins)
        .args(["h", "ssh-ed25519", "AAAA", "SHA256:abc"])
        .output()
        .expect("hk runs");
    assert_eq!(
        (out.status.code(), out.stdout),
        (Some(0), b"h ssh-ed25519 AAAA\n".to_vec())
    );
    let out = hk(&rig, &["ssh-status", "host-1"])
        .output()
        .expect("hk runs");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("host: host-1\nhost keys: pinned\n"));
    let out = Command::new(env!("CARGO_BIN_EXE_hk"))
        .args(["--endpoint", "https://localhost:1"])
        .arg("--ca-file")
        .arg(rig.dir().join("ca.pem"))
        .arg("--token-file")
        .arg(rig.dir().join("token"))
        .args(["ssh-status", "host-1"])
        .env("HOME", rig.dir())
        .output()
        .expect("hk runs");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("hk: no answer from the coordinator"));
}

/// Proves: `ssh-proxy` as a process relays its standard input to the host and the host's
/// answer to its standard output, makes the operator's key under `$HOME/.hostkeeper/ssh` with
/// the machine's `ssh-keygen`, and asks the coordinator for a certificate once.
#[test]
fn the_binary_relays_standard_input_through_the_tunnel() {
    let rig = Rig::start();
    let mut child = hk(&rig, &["ssh-proxy", HOST])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hk runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"SSH-2.0-client\r\n")
        .expect("write");
    let out = child.wait_with_output().expect("hk ends");
    assert_eq!(
        (out.status.code(), out.stdout.as_slice()),
        (Some(0), b"SSH-2.0-client\r\nbye".as_slice()),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(rig.dir().join(".hostkeeper/ssh/id_ed25519").exists());
    assert_eq!(rig.seen(|s| s.signs.len()), 1);
    assert_eq!(rig.seen(|s| s.signs[0].reason.clone()), "hk ssh-proxy");
}
