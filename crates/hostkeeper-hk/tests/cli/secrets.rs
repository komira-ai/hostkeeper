//! The token and a secret reach the request and nothing else: not stdout or stderr, not a file
//! `hk` writes, not the probe of the coordinator's version. And a token file others can read, or
//! that holds no token, is refused.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use crate::gateway;
use crate::rig::{Opts, Rig, Server, TOKEN, args};

/// The paths under `dir`, as text, sorted.
fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("a directory").flatten() {
            let path = entry.path();
            // The coordinator keeps its own state there.
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("coordinator"))
            {
                continue;
            }
            if path.is_dir() {
                pending.push(path.clone());
            }
            out.push(path.display().to_string());
        }
    }
    out.sort();
    out
}

/// Proves: the token is in the `Authorization` header of the call and in no other request (the
/// probe after `invalid_request` has none), in no output of a call (answered, refused or not
/// answered), and a secret is in the body of its call alone; the token from stdin and from a
/// file are the same; and a run writes no file.
#[test]
fn the_token_and_a_secret_reach_the_request_and_nothing_else() {
    let rig = Rig::new("token-absent");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    let before = tree(&rig.path(""));

    // From the token file, through the real gateway, answered and refused.
    let mut globals = rig.globals(gateway.port);
    globals.extend(args(&["hosts", "list"]));
    let ran = rig.hk(&globals);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert!(!ran.stdout.contains(TOKEN));
    let request = rig.put("bad.json", b"{\"noSuchField\":1}", 0o644);
    globals.extend(args(&["--request", &request]));
    let ran = rig.hk(&globals);
    // The refusal is followed by the probe of the coordinator's version, which carries no token.
    let seen = gateway.seen();
    let probe = seen.last().expect("the probe");
    assert_eq!(probe.target, "/v1/version?at_least=1.2.3");
    assert_eq!((probe.status, probe.header("authorization")), (409, None));
    assert!(!probe.text().contains(TOKEN));
    let said: serde_json::Value = serde_json::from_slice(&probe.answer).expect("a version");
    let coordinator = said["version"].as_str().expect("a version").to_owned();
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: the coordinator answered INVALID_ARGUMENT\nhk: the coordinator is older than this hk (1.2.3): it runs {coordinator}; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
    // From stdin.
    let stdin_globals = args(&[
        "--endpoint",
        &format!("https://127.0.0.1:{}", gateway.port),
        "--ca-file",
        &rig.path("ca.pem").display().to_string(),
        "--token-stdin",
        "hosts",
        "list",
    ]);
    let ran = rig.run(
        Opts::real().stdin(format!("{TOKEN}\n").as_bytes()),
        &stdin_globals,
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));

    for seen in gateway.seen() {
        let expect = if seen.target.starts_with("/v1/version") {
            None
        } else {
            Some(format!("Bearer {TOKEN}"))
        };
        assert_eq!(
            seen.header("authorization").map(str::to_owned),
            expect,
            "{}",
            seen.target
        );
        assert!(!seen.text().contains(TOKEN), "{}", seen.target);
    }

    // A secret field of the fixture, from its file and from stdin, refused or not.
    let password = "the-password-of-the-test";
    let file = rig.put("password", format!("{password}\n").as_bytes(), 0o600);
    let ok = Server::canned(&rig.pki, 200, "{}");
    let mut words = rig.globals(ok.port);
    words.extend(args(&["widgets", "login", "--password-file", &file]));
    let ran = rig.fx(&words);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let refused = Server::canned(
        &rig.pki,
        403,
        "{\"code\":\"PERMISSION_DENIED\",\"reason\":\"no\"}",
    );
    let mut words = rig.globals(refused.port);
    words.extend(args(&["widgets", "login", "--password-stdin"]));
    let ran = rig.run(Opts::fixture().stdin(password.as_bytes()), &words);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            3,
            "",
            "hk: the coordinator answered PERMISSION_DENIED\nrefused: no\n"
        )
    );
    for server in [&ok, &refused] {
        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].text(), format!("{{\"password\":\"{password}\"}}"));
        assert_eq!(
            seen[0].header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }

    // Nothing was written: the token and the secret are in memory only.
    assert_eq!(tree(&rig.path("")), {
        let mut after = before;
        after.extend(
            ["bad.json", "password"]
                .iter()
                .map(|n| rig.path(n).display().to_string()),
        );
        after.sort();
        after
    });
}

/// Proves: a call that gets no answer says why without the token or the secret in the message,
/// whatever the transport's error holds.
#[test]
fn a_call_with_no_answer_does_not_say_the_token_or_a_secret() {
    let rig = Rig::new("no-leak");
    let password = rig.put("password", b"the-password-of-the-test\n", 0o600);
    let port = crate::rig::closed_port();
    let mut words = rig.globals(port);
    words.extend(args(&["widgets", "login", "--password-file", &password]));
    let ran = rig.fx(&words);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""));
    assert!(
        ran.stderr
            .starts_with(&format!("hk: no answer from https://127.0.0.1:{port} (")),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.ends_with(")\n") && ran.stderr.lines().count() == 1);
    assert!(!ran.stderr.contains(TOKEN));
    assert!(!ran.stderr.contains("the-password-of-the-test"));
}

/// Proves: a token file that group or others can read is refused (mode bits 0o044 only: others'
/// and the group's read), one that only its owner can read is used, a link is judged by its
/// target, and an unreadable, empty or malformed token is refused with the reason said.
#[test]
fn a_token_file_its_group_or_others_may_read_is_refused() {
    let rig = Rig::new("token-mode");
    let server = Server::canned(&rig.pki, 200, "{}");
    let token = rig.path("token").display().to_string();
    let mut globals = rig.globals(server.port);
    globals.extend(args(&["gadgets", "list"]));
    for mode in [0o640, 0o604, 0o644, 0o660, 0o606, 0o440] {
        rig.put("token", format!("{TOKEN}\n").as_bytes(), mode);
        let ran = rig.fx(&globals);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                2,
                "",
                format!(
                    "hk: --token-file {token} may be read by its group or others; make it 0600\n"
                )
            ),
            "{mode:o}"
        );
    }
    assert!(server.seen().is_empty());
    for mode in [0o600, 0o400, 0o700] {
        rig.put("token", format!("{TOKEN}\n").as_bytes(), mode);
        let ran = rig.fx(&globals);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{mode:o}");
    }
    // A link to a private file is the file's mode, not the link's.
    rig.put("token", format!("{TOKEN}\n").as_bytes(), 0o600);
    let link = rig.path("link").display().to_string();
    std::os::unix::fs::symlink(rig.path("token"), rig.path("link")).expect("a link");
    let at = globals
        .iter()
        .position(|a| a == &token)
        .expect("the token flag");
    let mut linked = globals.clone();
    linked[at] = link;
    let ran = rig.fx(&linked);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));

    let missing = rig.path("missing").display().to_string();
    let not_visible = "the token is empty or holds a character that is not visible ASCII";
    let cases = [
        (
            missing.as_str(),
            &b""[..],
            format!("cannot read the mode of --token-file {missing}"),
        ),
        (token.as_str(), b"", not_visible.to_owned()),
        (token.as_str(), b"two words\n", not_visible.to_owned()),
        (token.as_str(), b"line\nbreak\n", not_visible.to_owned()),
        (token.as_str(), b"back\\slash", not_visible.to_owned()),
        (token.as_str(), b"tab\tbed", not_visible.to_owned()),
        (token.as_str(), b"caf\xc3\xa9", not_visible.to_owned()),
        (token.as_str(), b"crlf\r\n", not_visible.to_owned()),
        (
            token.as_str(),
            b"nul\0byte",
            "the token holds a NUL byte".to_owned(),
        ),
    ];
    for (path, content, why) in cases {
        rig.put("token", content, 0o600);
        let mut words = globals.clone();
        words[at] = path.to_owned();
        let ran = rig.fx(&words);
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hk: {why}\n")),
            "{path} {content:?}"
        );
    }
    // A token file that is a directory only its owner can read.
    std::fs::create_dir(rig.path("private")).expect("a directory");
    std::fs::set_permissions(rig.path("private"), std::fs::Permissions::from_mode(0o700))
        .expect("a mode");
    let mut words = globals.clone();
    words[at] = rig.path("private").display().to_string();
    let ran = rig.fx(&words);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hk: cannot read the token from {}\n",
                rig.path("private").display()
            )
        )
    );
    // A token from stdin that is no token.
    let words = args(&["--token-stdin", "gadgets", "list"]);
    let mut all = rig.globals(server.port);
    all.truncate(4);
    all.extend(words);
    let ran = rig.run(Opts::fixture().stdin(b"two words\n"), &all);
    assert_eq!((ran.code, ran.stderr), (2, format!("hk: {not_visible}\n")));
    assert_eq!(server.seen().len(), 4);
}
