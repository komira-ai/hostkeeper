//! The OpenSSH programs `hkssh` runs, here the real ones: `ssh-keygen` and `ssh-agent` of the
//! machine (the tests need `openssh-client`, which every runner of the PR checks has), and
//! `true` and `false` for the exit codes.

use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_hkssh::{Programs, System, normalize_public_key};
use hostkeeper_testkit::Scratch;

fn scratch() -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "hkprog").expect("a scratch directory")
}

fn system(keygen: &str, add: &str, ssh: &str) -> System {
    System {
        ssh: ssh.into(),
        keygen: keygen.into(),
        add: add.into(),
        agent_socket: None,
    }
}

/// Proves: `ssh-keygen` makes an Ed25519 key with no passphrase, its public half is read back as
/// `<type> <base64>`, and a keygen that fails, or is not there, is an error naming it.
#[test]
fn a_key_is_made_by_ssh_keygen_and_a_failure_names_the_program() {
    let scratch = scratch();
    let key = scratch.path().join("id");
    let real = system("ssh-keygen", "ssh-add", "ssh");
    real.generate_key(&key).expect("a key");
    let public = std::fs::read_to_string(scratch.path().join("id.pub")).expect("the public key");
    assert!(
        normalize_public_key(&public)
            .expect("a key")
            .starts_with("ssh-ed25519 ")
    );
    let error = real
        .generate_key(&key)
        .expect_err("not over an existing key");
    assert_eq!(error.to_string().split(':').next(), Some("ssh-keygen"));
    let error = system("false", "ssh-add", "ssh")
        .generate_key(&key)
        .expect_err("fails");
    assert!(error.to_string().starts_with("false: exited"), "{error}");
    let error = system("/nonexistent/keygen", "x", "x")
        .generate_key(&key)
        .expect_err("absent");
    assert!(
        error.to_string().starts_with("/nonexistent/keygen: "),
        "{error}"
    );
}

/// Proves: ssh's exit code is returned (0 and 1 here, from `true` and `false`), and a program
/// that is not there is an error naming it.
#[test]
fn ssh_is_run_and_its_exit_code_returned() {
    let args = [];
    assert_eq!(system("k", "a", "true").run_ssh(&args).expect("ran"), 0);
    assert_eq!(system("k", "a", "false").run_ssh(&args).expect("ran"), 1);
    let error = system("k", "a", "/nonexistent/ssh")
        .run_ssh(&args)
        .expect_err("absent");
    assert!(
        error.to_string().starts_with("/nonexistent/ssh: "),
        "{error}"
    );
}

fn agent(scratch: &Scratch) -> (PathBuf, String) {
    let socket = scratch.path().join("agent");
    let output = Command::new("ssh-agent")
        .arg("-a")
        .arg(&socket)
        .output()
        .expect("ssh-agent");
    let text = String::from_utf8(output.stdout).expect("text");
    let pid = text
        .split("SSH_AGENT_PID=")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("a pid")
        .to_owned();
    (socket, pid)
}

/// Proves: the agent's first key is read through `ssh-add -L` on the socket given: none while the
/// agent is empty, the key once one is added; with no agent at all, or an `ssh-add` that is not
/// there, there is none.
#[test]
fn the_agents_first_key_is_read_with_ssh_add() {
    let scratch = scratch();
    let (socket, pid) = agent(&scratch);
    let mut real = system("ssh-keygen", "ssh-add", "ssh");
    real.agent_socket = Some(socket.clone());
    assert_eq!(real.agent_key(), None, "an empty agent");
    let key = scratch.path().join("k");
    real.generate_key(&key).expect("a key");
    let added = Command::new("ssh-add")
        .arg(&key)
        .env("SSH_AUTH_SOCK", &socket)
        .output()
        .expect("ssh-add");
    assert!(added.status.success());
    let public = std::fs::read_to_string(scratch.path().join("k.pub")).expect("pub");
    assert_eq!(real.agent_key(), normalize_public_key(&public));
    let killed = Command::new("kill").arg(&pid).status().expect("kill");
    assert!(killed.success());
    assert_eq!(real.agent_key(), None, "an agent that is gone");
    assert_eq!(system("k", "/nonexistent/add", "s").agent_key(), None);
    assert_eq!(
        system("k", "true", "s").agent_key(),
        None,
        "success with no key"
    );
}

/// Proves: only a line of a plain public key is a key: `<type> <base64>` of a line with a
/// comment, nothing for a certificate line, a short line, or text that is not a key.
#[test]
fn only_a_plain_public_key_line_is_a_key() {
    assert_eq!(
        normalize_public_key("ssh-ed25519 AAAA me@host").as_deref(),
        Some("ssh-ed25519 AAAA")
    );
    assert_eq!(
        normalize_public_key("ecdsa-sha2-nistp256 BBBB").as_deref(),
        Some("ecdsa-sha2-nistp256 BBBB")
    );
    for line in [
        "ssh-ed25519-cert-v01@openssh.com AAAA",
        "ssh-ed25519",
        "",
        "hello world",
        "The agent has no identities.",
    ] {
        assert_eq!(normalize_public_key(line), None, "{line:?}");
    }
}
