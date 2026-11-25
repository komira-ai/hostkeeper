//! `hk ssh`, `ssh-revoke`, `ssh-status` and `ssh-known-host`.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

use hostkeeper_hk::ssh::{Verb, known_host};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::fake::{
    Call, Closed, Double, EXE, HOST, NOW, PIN, PUBLIC, Rig, TOKEN, cert_type, invoke, invoke_as,
    invoke_into, os,
};

fn ssh(rig: &Rig, double: &Double, extra: &[&str]) -> crate::fake::Outcome {
    let mut rest = vec![HOST];
    rest.extend_from_slice(extra);
    invoke(rig, Verb::Ssh, &rest, double, NOW, Vec::new())
}

/// Proves: `ssh` keeps the pins and the certificate where ssh reads them, runs ssh with the
/// certificate, the proxy and the known-hosts command (each a quoted command line of this
/// program), strict host key checking and no password, to the login user the coordinator named,
/// then the arguments after `--`, and exits with ssh's code. The token is in no argument.
#[test]
fn ssh_runs_ssh_with_the_certificate_the_proxy_and_the_pins() {
    let rig = Rig::start();
    let double = Double {
        ssh_code: 7,
        ..Double::new()
    };
    let outcome = ssh(&rig, &double, &["--", "uname", "-a"]);
    assert_eq!(outcome.code, 7);
    let calls = double.calls();
    assert_eq!(calls.len(), 1);
    let args = &calls[0];
    let state = rig.state_dir();
    let option = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no -o {name} in {args:?}"))
            .to_owned()
    };
    for (name, value) in [
        ("IdentitiesOnly", "yes"),
        ("PasswordAuthentication", "no"),
        ("KbdInteractiveAuthentication", "no"),
        ("StrictHostKeyChecking", "yes"),
        ("UserKnownHostsFile", "/dev/null"),
        ("GlobalKnownHostsFile", "/dev/null"),
    ] {
        assert_eq!(option(name), value);
    }
    assert_eq!(
        option("CertificateFile"),
        state
            .join("hosts")
            .join(format!("{HOST}-cert.pub"))
            .display()
            .to_string()
    );
    let coordinator = rig.coordinator();
    assert_eq!(
        option("ProxyCommand"),
        format!(
            "'{EXE}' --endpoint '{}' --ca-file '{}' --token-file '{}' --timeout-secs '30' \
             ssh-proxy '{HOST}'",
            coordinator.endpoint,
            coordinator.ca_file.display(),
            coordinator.token_file.display()
        )
    );
    assert_eq!(
        option("KnownHostsCommand"),
        format!(
            "'{EXE}' ssh-known-host '{}' %H %t %K %f",
            state.join("hosts").join(format!("{HOST}.pins")).display()
        )
    );
    let tail = &args[args.len() - 3..];
    assert_eq!(
        tail,
        [format!("root@{HOST}"), "uname".to_owned(), "-a".to_owned()]
    );
    assert!(args.iter().all(|a| !a.contains(TOKEN)));
    let identity = args.iter().position(|a| a == "-i").expect("-i");
    assert_eq!(
        args[identity + 1],
        state.join("id_ed25519").display().to_string()
    );
    // The pins file and the certificate are where the options say.
    assert_eq!(
        std::fs::read_to_string(state.join("hosts").join(format!("{HOST}.pins"))).expect("pins"),
        format!("{PIN}\n")
    );
    assert_eq!(
        std::fs::read_to_string(state.join("hosts").join(format!("{HOST}-cert.pub")))
            .expect("cert"),
        format!("{} AAAAcertbytes\n", cert_type())
    );
    assert_eq!(rig.seen(|s| s.signs[0].reason.clone()), "hk ssh");
    assert_eq!(rig.seen(|s| s.signs[0].public_key.clone()), PUBLIC);
}

/// Proves: a host with no pin is refused before any certificate is asked for; a host whose keys
/// changed is connected to with the pinned set and a warning; any other state is refused.
#[test]
fn ssh_needs_a_pin_and_says_when_the_keys_changed() {
    let rig = Rig::start();
    *rig.script.keys.lock().expect("keys") = Ok(proto::HostKeysResponse {
        fingerprints: Vec::new(),
        state: "no_pin".into(),
    });
    let double = Double::new();
    let outcome = ssh(&rig, &double, &[]);
    let error = outcome.error();
    assert!(
        error.starts_with(&format!("no host key is pinned for {HOST}")),
        "{error}"
    );
    assert_eq!(outcome.code, 1);
    assert_eq!(rig.seen(|s| s.signs.len()), 0);
    assert!(double.calls().is_empty());

    *rig.script.keys.lock().expect("keys") = Ok(proto::HostKeysResponse {
        fingerprints: vec![PIN.into()],
        state: "changed".into(),
    });
    let outcome = ssh(&rig, &double, &[]);
    assert_eq!(outcome.ok(), 0);
    assert_eq!(
        outcome.diag,
        format!(
            "hk: the host keys of {HOST} differ from the pinned ones; connecting with the pinned set\n"
        )
    );

    for (state, fingerprints) in [("pinned", vec![]), ("surprise", vec![PIN.to_owned()])] {
        *rig.script.keys.lock().expect("keys") = Ok(proto::HostKeysResponse {
            fingerprints,
            state: state.into(),
        });
        assert_eq!(
            ssh(&rig, &double, &[]).error(),
            format!("the coordinator gave no usable pins for {HOST} (state {state:?})")
        );
    }
}

/// Proves: the key is chosen as the design says: `--identity` first (its `.pub` is read and the
/// private key is what ssh is given), else the agent's first key (ssh is given the public key
/// file so that the agent signs), else the key of the state directory, made once and reused.
#[test]
fn the_key_is_the_identity_then_the_agent_then_the_state_key() {
    let rig = Rig::start();
    let private = rig.dir().join("mine");
    std::fs::write(rig.dir().join("mine.pub"), format!("{PUBLIC} comment\n")).expect("pub");
    let double = Double::new();
    let outcome = ssh(
        &rig,
        &double,
        &["--identity", private.to_str().expect("utf-8")],
    );
    assert_eq!(outcome.ok(), 0);
    let args = double.calls()[0].clone();
    let at = args.iter().position(|a| a == "-i").expect("-i");
    assert_eq!(args[at + 1], private.display().to_string());
    assert!(args.iter().any(|a| a.starts_with("ProxyCommand=")
        && a.ends_with(&format!(
            " ssh-proxy '{HOST}' --identity '{}'",
            private.display()
        ))));
    assert!(double.generated.lock().expect("generated").is_empty());

    let rig = Rig::start();
    let double = Double {
        agent: Some("ssh-ed25519 AAAAagent".into()),
        ..Double::new()
    };
    ssh(&rig, &double, &[]).ok();
    let args = double.calls()[0].clone();
    let at = args.iter().position(|a| a == "-i").expect("-i");
    assert_eq!(
        args[at + 1],
        rig.state_dir().join("agent.pub").display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(rig.state_dir().join("agent.pub")).expect("agent.pub"),
        "ssh-ed25519 AAAAagent\n"
    );
    assert!(double.generated.lock().expect("generated").is_empty());

    let rig = Rig::start();
    let double = Double::new();
    ssh(&rig, &double, &[]).ok();
    ssh(&rig, &double, &[]).ok();
    assert_eq!(
        *double.generated.lock().expect("generated"),
        vec![rig.state_dir().join("id_ed25519")]
    );
}

/// Proves: an identity whose `.pub` is missing, or holds no key, is an error that names the
/// file, and nothing is sent to the coordinator for it.
#[test]
fn an_identity_without_a_public_key_is_refused() {
    let rig = Rig::start();
    let private = rig.dir().join("none");
    let error = ssh(
        &rig,
        &Double::new(),
        &["--identity", private.to_str().expect("utf-8")],
    )
    .error();
    assert!(
        error.starts_with("cannot read a public key from"),
        "{error}"
    );
    std::fs::write(rig.dir().join("none.pub"), "not a key\n").expect("pub");
    let error = ssh(
        &rig,
        &Double::new(),
        &["--identity", private.to_str().expect("utf-8")],
    )
    .error();
    assert!(error.ends_with("holds no public key"), "{error}");
    assert_eq!(rig.seen(|s| s.signs.len()), 0);
}

/// Proves: `ssh-revoke` sends the operator, the serial, or the serial of the certificate held for
/// a host, whichever was given, and prints how many were revoked; a host with no certificate held
/// is an error; the coordinator's refusal is the error.
#[test]
fn revoke_names_an_operator_a_serial_or_the_certificate_held_for_a_host() {
    let rig = Rig::start();
    let double = Double::new();
    let revoke = |rest: &[&str]| invoke(&rig, Verb::Revoke, rest, &double, NOW, Vec::new());
    let outcome = revoke(&["--operator", "alice"]);
    assert_eq!(
        (outcome.ok(), outcome.text()),
        (0, "revoked: 1\n".to_owned())
    );
    revoke(&["--serial", "9"]).ok();
    assert_eq!(
        revoke(&["--host", HOST]).error(),
        format!("no certificate is held for {HOST}")
    );
    ssh(&rig, &double, &[]).ok();
    revoke(&["--host", HOST]).ok();
    let sent = rig.seen(|s| {
        s.revokes
            .iter()
            .map(|r| (r.operator.clone(), r.serial))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        sent,
        [
            ("alice".to_owned(), 0),
            (String::new(), 9),
            (String::new(), 41)
        ]
    );
    *rig.script.revoke.lock().expect("revoke") = Err(tonic::Status::not_found("unknown_cert"));
    assert_eq!(revoke(&["--serial", "1"]).error(), "refused: unknown_cert");
    let outcome = revoke(&["--host", "../x"]);
    assert_eq!(outcome.code, 2);
    assert!(outcome.error().contains("is not a host id"));
}

/// Proves: `ssh-status` prints the pin state and fingerprints of the host and the certificate
/// held: none, valid, or expired by the clock it is given.
#[test]
fn status_prints_the_pins_and_the_certificate_held() {
    let rig = Rig::start();
    let double = Double::new();
    let status = |now: u64| invoke(&rig, Verb::Status, &[HOST], &double, now, Vec::new());
    assert_eq!(
        status(NOW).text(),
        format!("host: {HOST}\nhost keys: pinned\nfingerprint: {PIN}\ncertificate: none held\n")
    );
    ssh(&rig, &double, &[]).ok();
    let held = format!("certificate: serial 41 for root, expires {} ", NOW + 3600);
    assert!(status(NOW).text().ends_with(&format!("{held}(valid)\n")));
    assert!(
        status(NOW + 3601)
            .text()
            .ends_with(&format!("{held}(expired)\n"))
    );
    *rig.script.keys.lock().expect("keys") = Err(tonic::Status::not_found("unknown_host"));
    assert_eq!(status(NOW).error(), "refused: unknown_host");
}

fn known(args: &[OsString]) -> (u8, Vec<u8>) {
    let mut stdout = Vec::new();
    let code = known_host(args, &mut stdout);
    (code, stdout)
}

/// Proves: the known-hosts command prints a line for the key ssh presents only when its
/// fingerprint is pinned, and prints nothing (still exit 0) when it is not, when ssh gave no key
/// (it only asks which key types to prefer), when an argument is not text, or when there is no
/// pins file.
#[test]
fn known_host_vouches_only_for_a_pinned_fingerprint() {
    let rig = Rig::start();
    let pins = rig.dir().join("pins");
    std::fs::write(&pins, format!("SHA256:other\n{PIN}\n")).expect("pins");
    let run = |rest: &[&str]| {
        let mut all = vec![pins.clone().into_os_string()];
        all.extend(os(rest));
        known(&all)
    };
    assert_eq!(
        run(&[HOST, "ssh-ed25519", "AAAAkey", PIN]),
        (0, format!("{HOST} ssh-ed25519 AAAAkey\n").into_bytes())
    );
    let others = [
        vec![HOST, "ssh-ed25519", "AAAAkey", "SHA256:unpinned"],
        vec![HOST],
        vec![],
    ];
    for rest in others {
        assert_eq!(run(&rest), (0, Vec::new()), "{rest:?}");
    }
    let mut not_text = vec![pins.clone().into_os_string()];
    not_text.extend(os(&[HOST, "ssh-ed25519"]));
    not_text.push(OsString::from_vec(b"AA\xffAA".to_vec()));
    not_text.push(OsString::from(PIN));
    assert_eq!(known(&not_text), (0, Vec::new()));
    assert_eq!(known(&[]), (0, Vec::new()));
    std::fs::remove_file(&pins).expect("remove");
    assert_eq!(run(&[HOST, "ssh-ed25519", "AAAAkey", PIN]), (0, Vec::new()));
}

/// Proves: the tail of `hk ssh` is translated into ssh's arguments as the OS gave it: the
/// login and host come first, then every argument after `--` in order, including one that is not
/// UTF-8, and a tail without `--` adds none.
#[test]
fn the_tail_after_the_dashes_reaches_ssh_byte_for_byte() {
    let rig = Rig::start();
    let double = Double::new();
    let raw = OsString::from_vec(b"echo \xff\xfe".to_vec());
    let mut tail = os(&[HOST, "--"]);
    tail.extend([OsString::from("-v"), raw.clone(), OsString::from("a b")]);
    let call = Call {
        name: "hk",
        verb: Verb::Ssh,
        coordinator: &rig.coordinator(),
        state_dir: &rig.state_dir(),
        args: tail,
    };
    let (code, diag) = invoke_as(&call, &double, NOW, Vec::new(), Vec::new());
    assert_eq!((code, diag.as_str()), (0, ""));
    let sent = double.ssh_calls.lock().expect("calls")[0].clone();
    assert_eq!(
        sent[sent.len() - 4..],
        [
            OsString::from(format!("root@{HOST}")),
            OsString::from("-v"),
            raw,
            OsString::from("a b")
        ]
    );
    ssh(&rig, &double, &[]).ok();
    let plain = double.calls()[1].clone();
    assert_eq!(plain.last(), Some(&format!("root@{HOST}")));
}

/// Proves: the lines ssh runs start with the path of the running executable whatever name the
/// program was called by, and the diagnostics carry that name: run as `tool`, the proxy and
/// known-hosts commands still start with the executable's path, and the warning about changed
/// host keys starts `tool:`.
#[test]
fn the_lines_ssh_runs_name_the_executable_not_the_program_name() {
    let rig = Rig::start();
    *rig.script.keys.lock().expect("keys") = Ok(proto::HostKeysResponse {
        fingerprints: vec![PIN.into()],
        state: "changed".into(),
    });
    let double = Double::new();
    let call = Call {
        name: "tool",
        verb: Verb::Ssh,
        coordinator: &rig.coordinator(),
        state_dir: &rig.state_dir(),
        args: os(&[HOST, "--identity", "/nonexistent/key"]),
    };
    let (code, diag) = invoke_as(&call, &double, NOW, Vec::new(), Vec::new());
    assert_eq!(code, 1);
    assert_eq!(
        diag,
        format!(
            "tool: the host keys of {HOST} differ from the pinned ones; connecting with the pinned set\n\
             tool: cannot read a public key from /nonexistent/key.pub: No such file or directory (os error 2)\n"
        )
    );
    let call = Call {
        args: os(&[HOST]),
        ..call
    };
    let (code, _) = invoke_as(&call, &double, NOW, Vec::new(), Vec::new());
    assert_eq!(code, 0);
    let args = double.calls()[0].clone();
    let line = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("{name}=")))
            .expect(name)
            .to_owned()
    };
    assert!(line("ProxyCommand").starts_with(&format!("'{EXE}' --endpoint ")));
    assert!(line("KnownHostsCommand").starts_with(&format!("'{EXE}' ssh-known-host ")));
    assert_eq!(rig.seen(|s| s.signs[0].reason.clone()), "hk ssh");
}

/// Proves: text that cannot be written to standard output is an error that says so, for the
/// verbs that print (`ssh-status` and `ssh-revoke`), not a success.
#[test]
fn text_that_cannot_be_written_to_standard_output_is_an_error() {
    let rig = Rig::start();
    let double = Double::new();
    for (verb, rest) in [
        (Verb::Status, vec![HOST]),
        (Verb::Revoke, vec!["--serial", "3"]),
    ] {
        let (code, diag) = invoke_into(&rig, verb, &rest, &double, NOW, Vec::new(), Closed);
        assert_eq!(code, 1);
        assert!(
            diag.starts_with("hk: cannot write standard output"),
            "{diag}"
        );
    }
}

/// Proves: help of a verb is printed to standard output with exit 0 and a tail that does not
/// parse is clap's message on the diagnostics with exit 2, naming the verb.
#[test]
fn a_tail_that_does_not_parse_is_a_usage_error() {
    let rig = Rig::start();
    let double = Double::new();
    let help = invoke(&rig, Verb::Ssh, &["--help"], &double, NOW, Vec::new());
    assert_eq!(help.ok(), 0);
    assert!(help.text().starts_with("Get a certificate for a host"));
    assert!(help.text().contains("Usage: hk ssh "));
    for (verb, tail) in [
        (Verb::Ssh, vec![]),
        (Verb::Proxy, vec![]),
        (Verb::Status, vec![HOST, "extra"]),
        (Verb::Revoke, vec![]),
        (Verb::Revoke, vec!["--operator", "a", "--serial", "1"]),
        (Verb::Revoke, vec!["--serial", "1", "--host", "h"]),
        (Verb::Revoke, vec!["--operator", "a", "--host", "h"]),
        (Verb::Revoke, vec!["--serial", "x"]),
    ] {
        let outcome = invoke(&rig, verb, &tail, &double, NOW, Vec::new());
        assert_eq!(outcome.code, 2, "{verb:?} {tail:?}");
        assert!(outcome.diag.starts_with("error: "), "{}", outcome.diag);
        assert!(outcome.stdout.is_empty());
    }
    assert!(
        invoke(&rig, Verb::Proxy, &[], &double, NOW, Vec::new())
            .diag
            .contains("Usage: hk ssh-proxy ")
    );
}
