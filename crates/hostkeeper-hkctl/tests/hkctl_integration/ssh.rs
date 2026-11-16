//! `hkctl ssh`, `ssh-proxy`, `ssh-revoke` and `ssh-status` run `hkssh` and nothing else: the
//! flags the script took, the state directory under `$HOME`, the path of the token file and never
//! the token, and the byte stream of `ssh-proxy` is the caller's own stdin and stdout.

use crate::rig::{Call, Rig, Script, TOKEN, args};

/// The runs of `hkssh` the fake recorded.
fn hkssh(rig: &Rig) -> Vec<Call> {
    rig.calls()
        .into_iter()
        .filter(|c| c.program == "hkssh")
        .collect()
}

/// A rig with the fake as `hkssh` on PATH and a home directory; the environment of a run.
fn rig(name: &str) -> (Rig, String) {
    let rig = Rig::new(name);
    rig.put_hkssh(false);
    let home = rig.path("home");
    std::fs::create_dir(&home).expect("the home directory");
    (rig, home.display().to_string())
}

/// What `hkssh` is given before its command, for the globals of `rig` and this home.
fn leading(rig: &Rig, home: &str) -> Vec<String> {
    vec![
        "--endpoint".to_owned(),
        "https://127.0.0.1:4443".to_owned(),
        "--ca-file".to_owned(),
        rig.path("ca.pem").display().to_string(),
        "--token-file".to_owned(),
        rig.path("token").display().to_string(),
        "--state-dir".to_owned(),
        format!("{home}/.hostkeeper/ssh"),
        "--timeout-secs".to_owned(),
        "30".to_owned(),
    ]
}

/// Proves: each of the four commands execs `hkssh` once with the global flags it took, the
/// state directory `$HOME/.hostkeeper/ssh`, the command's name in hkssh's spelling and the rest of
/// its words in order (a `--` and the ssh arguments after it included); flags of hkssh, and the
/// globals given after the command, are accepted; no gateway call is made and the token is in no
/// argument and no environment variable.
#[test]
fn each_command_execs_hkssh_with_the_flags_it_took() {
    let (rig, home) = rig("ssh-exec");
    let globals = rig.globals(4443);
    let cases: [(&[&str], &[&str]); 5] = [
        (
            &["ssh", "host-7", "--", "uname", "-a"],
            &["ssh", "host-7", "--", "uname", "-a"],
        ),
        (
            &["ssh", "host-7", "--identity", "/k/id", "--reason", "fix"],
            &["ssh", "host-7", "--identity", "/k/id", "--reason", "fix"],
        ),
        (&["ssh-proxy", "host-7"], &["proxy", "host-7"]),
        (
            &["ssh-revoke", "--serial", "5"],
            &["revoke", "--serial", "5"],
        ),
        (&["ssh-status", "host-7"], &["status", "host-7"]),
    ];
    for (words, expected) in cases {
        rig.reset();
        let mut line = globals.clone();
        line.extend(args(words));
        let ran = rig.run(Script::Real, &line, b"", &[("HOME", &home)]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        let calls = hkssh(&rig);
        assert_eq!(calls.len(), 1, "{words:?}");
        let mut want = leading(&rig, &home);
        want.extend(args(expected));
        assert_eq!(calls[0].argv, want, "{words:?}");
        assert!(
            rig.calls().iter().all(|c| c.program == "hkssh"),
            "{words:?}"
        );
        for call in rig.calls() {
            assert!(call.argv.iter().all(|a| !a.contains(TOKEN)));
            assert!(call.environ.iter().all(|e| !e.contains(TOKEN)));
        }
    }
    rig.reset();
    let mut line = args(&["ssh-status", "--timeout-secs", "9", "host-7"]);
    line.extend(globals.clone());
    rig.run(Script::Real, &line, b"", &[("HOME", &home)]);
    let mut want = leading(&rig, &home);
    want[9] = "9".to_owned();
    want.extend(args(&["status", "host-7"]));
    assert_eq!(hkssh(&rig)[0].argv, want);
}

/// Proves: a word after `ssh` that is a method of the service (`revoke`, `host-keys`,
/// `sign-user-cert`) is still the gateway call and does not run hkssh; only another word is a
/// host id.
#[test]
fn the_methods_of_the_service_ssh_stay_gateway_calls() {
    let (rig, home) = rig("ssh-methods");
    rig.reply(200, "{\"revoked\":\"1\"}");
    let mut line = rig.globals(4443);
    line.extend(args(&["ssh", "revoke", "--serial", "5"]));
    let ran = rig.run(Script::Real, &line, b"", &[("HOME", &home)]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "{\"revoked\":\"1\"}\n")
    );
    assert!(hkssh(&rig).is_empty());
    assert_eq!(rig.curls().len(), 1);
}

/// Proves: the usage errors: a credential other than a token file, a missing global flag, a bad
/// endpoint and a missing `$HOME` are each refused before hkssh runs, and `--help` prints what
/// the four commands are.
#[test]
fn the_commands_refuse_what_hkssh_cannot_take() {
    let (rig, home) = rig("ssh-usage");
    let run = |line: &[String], env: &[(&str, &str)]| rig.run(Script::Real, line, b"", env);
    let with = |extra: &[&str]| {
        let mut line = rig.globals(4443);
        line.extend(args(extra));
        line
    };
    for flag in [
        "--token-stdin",
        "--response-file=/x",
        "--github-oidc-audience=a",
    ] {
        let mut line = rig.globals(4443);
        line.truncate(4);
        line.extend(args(&[flag, "ssh-status", "h"]));
        let ran = run(&line, &[("HOME", &home)]);
        assert_eq!(ran.code, 2, "{flag}: {}", ran.stderr);
        assert!(
            ran.stderr.contains("takes --token-file only"),
            "{}",
            ran.stderr
        );
    }
    let ran = run(&args(&["ssh-status", "h"]), &[("HOME", &home)]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hkctl: hkctl ssh-status needs --endpoint, --ca-file and --token-file\n"
        )
    );
    let mut line = with(&["ssh-status", "h"]);
    line[1] = "http://127.0.0.1:4443".to_owned();
    let ran = run(&line, &[("HOME", &home)]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hkctl: --endpoint takes https://<host>[:<port>]\n")
    );
    let ran = run(&with(&["ssh-status", "h"]), &[]);
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.contains("which is not set"), "{}", ran.stderr);
    assert!(hkssh(&rig).is_empty());
    let ran = run(&args(&["ssh-proxy", "--help"]), &[]);
    assert_eq!(ran.code, 0);
    assert!(
        ran.stdout.contains("hkctl ssh-proxy <host-id>"),
        "{}",
        ran.stdout
    );
}

/// Proves: the hkssh beside the script is the one that runs when there is one, before the one on
/// the PATH.
#[test]
fn the_hkssh_beside_the_script_runs_before_the_one_on_the_path() {
    let (rig, home) = rig("ssh-beside");
    rig.put_hkssh(true);
    let mut line = rig.globals(4443);
    line.extend(args(&["ssh-status", "h"]));
    let ran = rig.run(Script::Real, &line, b"", &[("HOME", &home)]);
    assert_eq!(ran.code, 0);
    assert!(hkssh(&rig).is_empty(), "the one on the PATH did not run");
    assert!(rig.path("calls").is_dir(), "the one beside the script did");
}
