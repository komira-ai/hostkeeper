//! The built `hk` as a process: its `main`, the names it answers to, the environment it reads
//! (the runner's two variables, `HOME`) and the programs it does not need.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Output};

use crate::rig::{Rig, Server, TOKEN, closed_port};

/// What the coverage tool sets for a child, so that the child's coverage is counted; nothing
/// else of the test's environment reaches the program.
#[expect(
    clippy::disallowed_methods,
    reason = "the tests of a process pass on the variable the coverage tool names, and nothing else"
)]
fn coverage_env() -> Vec<(String, std::ffi::OsString)> {
    std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            name.starts_with("LLVM_PROFILE").then_some((name, value))
        })
        .collect()
}

/// The built program, run with an empty environment but what `env` names, and a `PATH` that is
/// an empty directory: a gateway call needs no other program.
fn hk(rig: &Rig, program: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    hk_os(rig, program, &args, env)
}

/// [`hk`] with arguments that need not be text.
fn hk_os(rig: &Rig, program: &std::path::Path, args: &[OsString], env: &[(&str, &str)]) -> Output {
    let empty = rig.path("empty-path");
    let _ = std::fs::create_dir(&empty);
    Command::new(program)
        .args(args)
        .env_clear()
        .envs(coverage_env())
        .env("PATH", &empty)
        .envs(env.iter().copied())
        .current_dir(rig.path(""))
        .output()
        .expect("hk runs")
}

/// Proves: `main` prints the stamp of `--version` after the name the program is called by (`hk`,
/// and `hkctl` through a link of that name), and a diagnostic starts with the name too; usage
/// errors exit 2 and a call exits with the gateway's code.
#[test]
fn the_program_answers_to_hk_and_to_its_old_name_hkctl() {
    let rig = Rig::new("process-names");
    let hk_path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hk"));
    let link = rig.path("hkctl");
    std::os::unix::fs::symlink(&hk_path, &link).expect("a link");
    let version = hostkeeper_hk::stamp::version();
    for (program, name) in [(hk_path.as_path(), "hk"), (link.as_path(), "hkctl")] {
        let out = hk(&rig, program, &["--version"], &[]);
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned()
            ),
            (Some(0), format!("{name} {version}\n")),
            "{name}"
        );
        let out = hk(&rig, program, &["nosuch"], &[]);
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).into_owned()
            ),
            (
                Some(2),
                format!("{name}: no service nosuch; '{name} --help' lists them\n")
            ),
            "{name}"
        );
        let out = hk(&rig, program, &[], &[]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .starts_with(&format!("hk {version}: call one method"))
        );
        let out = hk(&rig, program, &["--help"], &[]);
        assert_eq!(out.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&out.stdout).contains("\nServices:\n  alerts\n"));
    }
}

/// Proves: a call by the program is a gateway call with no other program: with an empty `PATH`
/// it posts the request over TLS to the endpoint, trusting the CA file, and prints the answer; the
/// token read from stdin is the bearer; and a refusal exits with its code.
#[test]
fn the_program_makes_a_gateway_call_with_no_other_program() {
    let rig = Rig::new("process-call");
    let program = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hk"));
    let server = Server::canned(&rig.pki, 200, "{\"hosts\":[]}");
    let endpoint = format!("https://127.0.0.1:{}", server.port);
    let ca = rig.path("ca.pem").display().to_string();
    let token = rig.path("token").display().to_string();
    let out = hk(
        &rig,
        &program,
        &[
            "--endpoint",
            &endpoint,
            "--ca-file",
            &ca,
            "--token-file",
            &token,
            "hosts",
            "list",
        ],
        &[],
    );
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.stderr
        ),
        (Some(0), "{\"hosts\":[]}\n".to_owned(), Vec::new())
    );
    let seen = server.seen();
    assert_eq!(seen[0].target, "/hostkeeper.operator.v1.Hosts/List");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );

    let refused = Server::canned(
        &rig.pki,
        403,
        "{\"code\":\"PERMISSION_DENIED\",\"reason\":\"no\"}",
    );
    let endpoint = format!("https://127.0.0.1:{}", refused.port);
    let out = hk(
        &rig,
        &program,
        &[
            "--endpoint",
            &endpoint,
            "--ca-file",
            &ca,
            "--token-file",
            &token,
            "hosts",
            "list",
        ],
        &[],
    );
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned()
        ),
        (
            Some(3),
            "hk: the coordinator answered PERMISSION_DENIED\nrefused: no\n".to_owned()
        )
    );
}

/// Proves: `main` reads the runner's two variables and asks the runner for a token with the
/// system's roots: a runner whose address nothing answers is a failure of that request, said, with
/// neither variable's value in the message.
#[test]
fn the_program_reads_the_runners_variables() {
    let rig = Rig::new("process-oidc");
    let program = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hk"));
    let ca = rig.path("ca.pem").display().to_string();
    let url = format!("https://127.0.0.1:{}/token", closed_port());
    let out = hk(
        &rig,
        &program,
        &[
            "--endpoint",
            "https://127.0.0.1:1",
            "--ca-file",
            &ca,
            "--github-oidc-audience",
            "aud",
            "hosts",
            "list",
        ],
        &[
            ("ACTIONS_ID_TOKEN_REQUEST_URL", &url),
            (
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
                "runner-request-token-of-the-test",
            ),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("hk: the runner's token request failed ("),
        "{stderr}"
    );
    assert!(!stderr.contains("runner-request-token-of-the-test"));
    assert!(out.stdout.is_empty());
}

/// Proves: a diagnostic carries the bytes of an argument or a path as they are, not as U+FFFD: an
/// unknown service, an unknown method of a service, and a token file of such a name that is not
/// there are each said with the bytes of the name.
#[test]
fn a_diagnostic_carries_the_bytes_of_a_name_as_given() {
    let rig = Rig::new("process-bytes");
    let program = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hk"));
    let odd = OsString::from_vec(b"od\xffd".to_vec());
    let said = |args: Vec<OsString>| {
        let out = hk_os(&rig, &program, &args, &[]);
        (out.status.code(), out.stderr)
    };
    assert_eq!(
        said(vec![odd.clone()]),
        (
            Some(2),
            b"hk: no service od\xffd; 'hk --help' lists them\n".to_vec()
        )
    );
    assert_eq!(
        said(vec![OsString::from("hosts"), odd.clone()]),
        (
            Some(2),
            b"hk: the service hosts has no method od\xffd; 'hk hosts --help' lists them\n".to_vec()
        )
    );
    let ca = rig.path("ca.pem");
    let mut args: Vec<OsString> = ["--endpoint", "https://127.0.0.1:1", "--ca-file"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.extend([ca.into_os_string(), OsString::from("--token-file"), odd]);
    args.extend(["hosts", "list"].map(OsString::from));
    assert_eq!(
        said(args),
        (
            Some(2),
            b"hk: cannot read the mode of --token-file od\xffd\n".to_vec()
        )
    );
}

/// Proves: a relative token file is looked at as `./<path>`, so an empty `--token-file=` is the
/// current directory (here readable by its group, so refused as such) and no path that cannot
/// be found.
#[test]
fn an_empty_token_file_is_the_current_directory() {
    let rig = Rig::new("process-empty-token");
    let program = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hk"));
    std::fs::set_permissions(rig.path(""), std::fs::Permissions::from_mode(0o755)).expect("a mode");
    let ca = rig.path("ca.pem").display().to_string();
    let out = hk(
        &rig,
        &program,
        &[
            "--endpoint",
            "https://127.0.0.1:1",
            "--ca-file",
            &ca,
            "--token-file=",
            "hosts",
            "list",
        ],
        &[],
    );
    assert_eq!(
        (out.status.code(), out.stderr),
        (
            Some(2),
            b"hk: --token-file  may be read by its group or others; make it 0600\n".to_vec()
        )
    );
}

/// Proves: the runner's two token variables reach no program `hk` starts, while the rest of the
/// environment does: `ssh-proxy` makes a key with `ssh-keygen` and asks the agent with `ssh-add`,
/// and each of the two, run with both variables set, records an environment without them and with
/// the variable `HK_PROBE` that was set beside them.
#[test]
fn the_runners_token_variables_reach_no_program_hk_starts() {
    let rig = Rig::new("process-env");
    let program = std::path::PathBuf::from(env!("CARGO_BIN_EXE_hk"));
    let bin = rig.path("bin");
    std::fs::create_dir(&bin).expect("a PATH directory");
    for name in ["ssh-keygen", "ssh-add"] {
        std::fs::copy(env!("CARGO_BIN_EXE_hk-fake-openssh"), bin.join(name)).expect("a copy");
    }
    let ca = rig.path("ca.pem").display().to_string();
    let token = rig.path("token").display().to_string();
    let home = rig.path("home").display().to_string();
    let out = Command::new(&program)
        .args([
            "--endpoint",
            "https://127.0.0.1:1",
            "--ca-file",
            &ca,
            "--token-file",
            &token,
            "ssh-proxy",
            "h1",
        ])
        .env_clear()
        .envs(coverage_env())
        .env("PATH", &bin)
        .env("HOME", &home)
        .env("HK_PROBE", "1")
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runner-secret")
        .env("ACTIONS_ID_TOKEN_REQUEST_URL", "https://127.0.0.1:1/t")
        .output()
        .expect("hk runs");
    assert!(
        !out.stderr.is_empty(),
        "the proxy cannot go on without a key"
    );
    for name in ["ssh-keygen", "ssh-add"] {
        let seen = std::fs::read_to_string(bin.join(format!("{name}.env"))).expect("a record");
        let names: Vec<&str> = seen.lines().collect();
        assert!(names.contains(&"HK_PROBE"), "{name}: {names:?}");
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("ACTIONS_ID_TOKEN_REQUEST")),
            "{name}: {names:?}"
        );
    }
}
