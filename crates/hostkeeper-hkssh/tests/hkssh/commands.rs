//! `hkssh ssh`, `revoke`, `status` and `known-host`.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::fake::{
    Closed, Double, HOST, NOW, PIN, PUBLIC, Rig, TOKEN, cert_type, invoke, invoke_into,
};

fn ssh(rig: &Rig, double: &Double, extra: &[&str]) -> crate::fake::Outcome {
    let mut rest = vec!["ssh", HOST];
    rest.extend_from_slice(extra);
    invoke(&rig.args(&rest), double, NOW, Vec::new())
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
    assert_eq!(outcome.result.expect("ssh ran"), 7);
    let calls = double.ssh_calls.lock().expect("calls").clone();
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
    let proxy = option("ProxyCommand");
    assert!(
        proxy.starts_with(&format!(
            "'/opt/hkssh' proxy '{HOST}' '--endpoint' 'https://localhost:"
        )),
        "{proxy}"
    );
    assert!(proxy.contains(&format!("'--state-dir' '{}'", state.display())));
    assert!(proxy.contains("'--timeout-secs' '30' '--keepalive-secs' '30'"));
    assert_eq!(
        option("KnownHostsCommand"),
        format!(
            "'/opt/hkssh' known-host '{}' %H %t %K %f",
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
    assert_eq!(rig.seen(|s| s.signs[0].reason.clone()), "hkctl ssh");
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
    let error = ssh(&rig, &double, &[]).result.expect_err("no pin");
    assert!(
        error
            .to_string()
            .starts_with(&format!("no host key is pinned for {HOST}")),
        "{error}"
    );
    assert_eq!(rig.seen(|s| s.signs.len()), 0);
    assert!(double.ssh_calls.lock().expect("calls").is_empty());

    *rig.script.keys.lock().expect("keys") = Ok(proto::HostKeysResponse {
        fingerprints: vec![PIN.into()],
        state: "changed".into(),
    });
    let outcome = ssh(&rig, &double, &[]);
    assert_eq!(outcome.result.expect("connected"), 0);
    assert_eq!(
        outcome.diag,
        format!(
            "hkssh: the host keys of {HOST} differ from the pinned ones; connecting with the pinned set\n"
        )
    );

    for (state, fingerprints) in [("pinned", vec![]), ("surprise", vec![PIN.to_owned()])] {
        *rig.script.keys.lock().expect("keys") = Ok(proto::HostKeysResponse {
            fingerprints,
            state: state.into(),
        });
        let error = ssh(&rig, &double, &[]).result.expect_err(state);
        assert_eq!(
            error.to_string(),
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
    assert_eq!(outcome.result.expect("ran"), 0);
    let args = double.ssh_calls.lock().expect("calls")[0].clone();
    let at = args.iter().position(|a| a == "-i").expect("-i");
    assert_eq!(args[at + 1], private.display().to_string());
    assert!(args.iter().any(|a| a.starts_with("ProxyCommand=")
        && a.contains(&format!("'--identity' '{}'", private.display()))));
    assert!(double.generated.lock().expect("generated").is_empty());

    let rig = Rig::start();
    let double = Double {
        agent: Some("ssh-ed25519 AAAAagent".into()),
        ..Double::new()
    };
    ssh(&rig, &double, &[]).result.expect("ran");
    let args = double.ssh_calls.lock().expect("calls")[0].clone();
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
    ssh(&rig, &double, &[]).result.expect("made");
    ssh(&rig, &double, &[]).result.expect("reused");
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
    .result
    .expect_err("missing");
    assert!(
        error
            .to_string()
            .starts_with("cannot read a public key from"),
        "{error}"
    );
    std::fs::write(rig.dir().join("none.pub"), "not a key\n").expect("pub");
    let error = ssh(
        &rig,
        &Double::new(),
        &["--identity", private.to_str().expect("utf-8")],
    )
    .result
    .expect_err("junk");
    assert!(
        error.to_string().ends_with("holds no public key"),
        "{error}"
    );
    assert_eq!(rig.seen(|s| s.signs.len()), 0);
}

/// Proves: `revoke` sends the operator, the serial, or the serial of the certificate held for a
/// host, whichever was given, and prints how many were revoked; a host with no certificate held
/// is an error; the coordinator's refusal is the error.
#[test]
fn revoke_names_an_operator_a_serial_or_the_certificate_held_for_a_host() {
    let rig = Rig::start();
    let double = Double::new();
    let revoke = |rest: &[&str]| {
        let mut all = vec!["revoke"];
        all.extend_from_slice(rest);
        invoke(&rig.args(&all), &double, NOW, Vec::new())
    };
    let outcome = revoke(&["--operator", "alice"]);
    let text = outcome.text();
    assert_eq!(
        (outcome.result.expect("ok"), text),
        (0, "revoked: 1\n".to_owned())
    );
    revoke(&["--serial", "9"]).result.expect("ok");
    let error = revoke(&["--host", HOST]).result.expect_err("none held");
    assert_eq!(
        error.to_string(),
        format!("no certificate is held for {HOST}")
    );
    ssh(&rig, &double, &[])
        .result
        .expect("a certificate is made");
    revoke(&["--host", HOST]).result.expect("ok");
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
    assert_eq!(
        revoke(&["--serial", "1"])
            .result
            .expect_err("refused")
            .to_string(),
        "refused: unknown_cert"
    );
    let error = revoke(&["--host", "../x"]).result.expect_err("bad id");
    assert_eq!(error.exit_code(), 2);
}

/// Proves: `status` prints the pin state and fingerprints of the host and the certificate held:
/// none, valid, or expired by the clock it is given.
#[test]
fn status_prints_the_pins_and_the_certificate_held() {
    let rig = Rig::start();
    let double = Double::new();
    let status = |now: u64| invoke(&rig.args(&["status", HOST]), &double, now, Vec::new()).text();
    assert_eq!(
        status(NOW),
        format!("host: {HOST}\nhost keys: pinned\nfingerprint: {PIN}\ncertificate: none held\n")
    );
    ssh(&rig, &double, &[])
        .result
        .expect("a certificate is made");
    let held = format!("certificate: serial 41 for root, expires {} ", NOW + 3600);
    assert!(status(NOW).ends_with(&format!("{held}(valid)\n")));
    assert!(status(NOW + 3601).ends_with(&format!("{held}(expired)\n")));
    *rig.script.keys.lock().expect("keys") = Err(tonic::Status::not_found("unknown_host"));
    let error = invoke(&rig.args(&["status", HOST]), &double, NOW, Vec::new())
        .result
        .expect_err("refused");
    assert_eq!(error.to_string(), "refused: unknown_host");
}

/// Proves: the known-hosts command prints a line for the key ssh presents only when its
/// fingerprint is pinned, and prints nothing when it is not, when ssh gave no key (it only asks
/// which key types to prefer), or when there is no pins file.
#[test]
fn known_host_vouches_only_for_a_pinned_fingerprint() {
    let rig = Rig::start();
    let pins = rig.dir().join("pins");
    std::fs::write(&pins, format!("SHA256:other\n{PIN}\n")).expect("pins");
    let run = |rest: &[&str]| {
        let mut all = vec!["known-host", pins.to_str().expect("utf-8")];
        all.extend_from_slice(rest);
        let line: Vec<String> = ["hkssh"].into_iter().chain(all).map(String::from).collect();
        invoke(&line, &Double::new(), NOW, Vec::new())
    };
    let outcome = run(&[HOST, "ssh-ed25519", "AAAAkey", PIN]);
    let text = outcome.text();
    assert_eq!(
        (outcome.result.expect("ok"), text),
        (0, format!("{HOST} ssh-ed25519 AAAAkey\n"))
    );
    let others = [
        vec![HOST, "ssh-ed25519", "AAAAkey", "SHA256:unpinned"],
        vec![HOST],
        vec![],
    ];
    for rest in others {
        let outcome = run(&rest);
        assert_eq!(
            (outcome.result.expect("ok"), outcome.stdout),
            (0, Vec::new()),
            "{rest:?}"
        );
    }
    std::fs::remove_file(&pins).expect("remove");
    assert!(
        run(&[HOST, "ssh-ed25519", "AAAAkey", PIN])
            .stdout
            .is_empty()
    );
}

/// Proves: text that cannot be written to standard output is an error that says so, for the
/// commands that print (`status` and `revoke`), not a success.
#[test]
fn an_output_that_cannot_be_written_is_an_error() {
    let rig = Rig::start();
    let double = Double::new();
    for rest in [vec!["status", HOST], vec!["revoke", "--serial", "3"]] {
        let (result, _) = invoke_into(&rig.args(&rest), &double, NOW, Vec::new(), Closed);
        let error = result.expect_err("closed");
        assert!(
            error
                .to_string()
                .starts_with("cannot write standard output"),
            "{error}"
        );
    }
}
