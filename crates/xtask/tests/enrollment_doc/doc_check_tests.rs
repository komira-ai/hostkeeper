//! The operator-doc check on the hand verbs of `hk` (`ssh`, `ssh-proxy`, `ssh-revoke`,
//! `ssh-status`, `xcode-push`), which are not in the command table, and on a fenced `hkctl`
//! line.

use super::*;

/// Catches the doc check refusing the hand verbs of the ssh tunnel (`ssh <host-id>`,
/// `ssh-proxy`, `ssh-revoke`, `ssh-status`, in a fenced line or a span), and passing one with a
/// flag it has not, a second host id, a revoke without or with two selectors, a token that is
/// not a file, or no endpoint; and catches it passing a method of the service `ssh` as a host id.
#[test]
fn the_doc_check_reads_the_ssh_hand_verbs() {
    let good = fenced_call("ssh host-1 --identity key --reason 'a fix' -- uptime")
        + &fenced_call("ssh-proxy host-1 --identity key")
        + &fenced_call("ssh-revoke --operator ana")
        + &fenced_call("ssh-revoke --serial 7")
        + &fenced_call("ssh-revoke --host=host-1")
        + &fenced_call("ssh-status host-1")
        + &fenced_call("ssh sign-user-cert --help")
        + "`hk ssh host-1 --reason r`, `hk ssh-proxy %h`, `hk ssh-status <host-id>`, `hk ssh-known-host`, \
           `hk ssh-revoke --operator ana`, `hk --endpoint https://<l> ssh-status h`, \
           `hk ssh sign-user-cert --request r` and `hk ssh`.\n";
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    assert_eq!(
        command_of(&format!("{CALL} ssh host-1 -- uptime")),
        Ok(Some("ssh".to_owned()))
    );
    assert_eq!(
        command_of(&format!("{CALL} ssh-status host-1")),
        Ok(Some("ssh-status".to_owned()))
    );
    let line = |rest: &str| format!("{CALL} {rest}");
    let cases = [
        (line("ssh --reason r host-1"), "unknown flag --reason"),
        (
            line("ssh-known-host"),
            "no service ssh-known-host; 'hk --help' lists them",
        ),
        (line("ssh host-1 --bogus"), "ssh has no flag --bogus"),
        (
            line("ssh-status host-1 --identity k"),
            "ssh-status has no flag --identity",
        ),
        (line("ssh-proxy a b"), "ssh-proxy takes one host id"),
        (line("ssh-proxy"), "ssh-proxy needs a host id"),
        (line("ssh-status"), "ssh-status needs a host id"),
        (
            line("ssh-proxy host-1 --identity a --identity b"),
            "--identity is given twice",
        ),
        (line("ssh-proxy host-1 --reason"), "--reason needs a value"),
        (
            line("ssh-revoke"),
            "ssh-revoke takes exactly one of --operator, --serial and --host",
        ),
        (
            line("ssh-revoke --operator a --serial 3"),
            "ssh-revoke takes exactly one of --operator, --serial and --host",
        ),
        (
            line("ssh-revoke --serial x"),
            "--serial takes a whole number",
        ),
        (
            line("ssh-revoke host-1"),
            "ssh-revoke takes no argument; it has flags only",
        ),
        (
            "hk --endpoint https://c.example.com --ca-file c --token-stdin ssh-status h".to_owned(),
            "ssh-status takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it",
        ),
        (
            format!("{CALL} --response-file r ssh-status h"),
            "ssh-status takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it",
        ),
        (
            "hk ssh-status h --ca-file c --token-file t".to_owned(),
            "a call needs --endpoint",
        ),
        (
            line("ssh sign-user-cert --nope"),
            "ssh sign-user-cert has no flag --nope",
        ),
    ];
    for (line, problem) in cases {
        let error = command_of(&line).expect_err(&line);
        assert_eq!(error, format!("{line}\n{problem}"));
    }
    let spans = [
        ("`hk ssh host-1 --bogus`", "ssh has no flag --bogus"),
        (
            "`hk ssh-status h --identity k`",
            "ssh-status has no flag --identity",
        ),
        (
            "`hk ssh-revoke --identity k`",
            "ssh-revoke has no flag --identity",
        ),
        (
            "`hk ssh sign-user-cert --nope`",
            "ssh sign-user-cert has no flag --nope",
        ),
    ];
    for (span, problem) in spans {
        let text = span.trim_matches('`');
        assert_eq!(
            doc_problems("docs/x.md", span),
            [format!("docs/x.md: `{text}`: {problem}")]
        );
    }
}

/// Catches the doc check passing a fenced line that begins `hkctl`, or reading it
/// as a call of `hk`, and catches it refusing a span or a prose line that only names `hkctl`.
#[test]
fn the_doc_check_refuses_a_fenced_hkctl_line() {
    let line = "hkctl hosts get --node-id n";
    assert_eq!(
        doc_problems("docs/x.md", &fenced(line)),
        [format!(
            "docs/x.md: a fenced `hkctl` line; the operator docs name hk, not hkctl: {line}"
        )]
    );
    let prose = "`hkctl` is not a program of this repository; `hkctl hosts nonsense` is not one.\n";
    assert_eq!(doc_problems("docs/x.md", prose), Vec::<String>::new());
}

/// Catches the doc check refusing `hk xcode-push` with a version or an archive (in a fenced line
/// or a span, `--help` too, and with `--mac`), and passing it with no argument or two, `--mac`
/// twice or without a value, a flag it has not, or a token that is not a file.
#[test]
fn the_doc_check_reads_xcode_push() {
    let good = fenced_call("xcode-push 27.2")
        + &fenced_call("xcode-push ~/Downloads/Xcode_27.2.xip")
        + &fenced_call("xcode-push --mac mac-01 27.2")
        + "`hk xcode-push`, `hk xcode-push 27.2`, `hk xcode-push --help` and \
           `hk xcode-push <version> | <archive.xip>`.\n";
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    assert_eq!(
        command_of(&format!("{CALL} xcode-push 27.2")),
        Ok(Some("xcode-push".to_owned()))
    );
    let one = "hk xcode-push takes one Xcode version (27.2) or the path of an archive \
               (~/Downloads/Xcode_27.2.xip)";
    let token = "hk xcode-push takes --token-file only: --token-stdin, --github-oidc-audience \
                 and --response-file are not for it (stdin is the terminal xcodes asks a \
                 two-factor code on)";
    let line = |rest: &str| format!("{CALL} {rest}");
    let cases = [
        (line("xcode-push"), one),
        (line("xcode-push 27.2 27.1"), one),
        (
            line("xcode-push 27.2 --force"),
            "xcode-push has no flag --force",
        ),
        (format!("{CALL} --response-file r xcode-push 27.2"), token),
        (
            line("xcode-push --mac a --mac b 27.2"),
            "--mac is given twice",
        ),
        (line("xcode-push 27.2 --mac"), "--mac needs a value"),
    ];
    for (line, problem) in cases {
        let error = command_of(&line).expect_err(&line);
        assert_eq!(error, format!("{line}\n{problem}"));
    }
    assert_eq!(
        doc_problems("docs/x.md", "`hk xcode-push --force`"),
        ["docs/x.md: `hk xcode-push --force`: xcode-push has no flag --force"]
    );
}

/// Catches the doc check refusing `hk tailnet-wait` as hk takes it (in a fenced line or a span,
/// `--help` too), and passing it with an argument, a flag or a number it has not, a token, or
/// without the CA file, each in hk's own words.
#[test]
fn the_doc_check_reads_tailnet_wait() {
    let call = "hk --endpoint https://coordinator.example:8443 --ca-file ca.pem";
    let good = format!(
        "```sh\n{call} tailnet-wait\n{call} tailnet-wait --attempts 3 --interval-secs=0\n```\n"
    ) + "`hk tailnet-wait`, `hk tailnet-wait --help` and `hk tailnet-wait --attempts 3`.\n";
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    assert_eq!(
        command_of(&format!("{call} tailnet-wait")),
        Ok(Some("tailnet-wait".to_owned()))
    );
    let token = "hk tailnet-wait takes no token and writes no answer: --token-file, \
                 --token-stdin, --github-oidc-audience and --response-file are not for it";
    let cases = [
        (
            format!("{call} tailnet-wait coordinator"),
            "hk tailnet-wait takes no argument: coordinator",
        ),
        (
            format!("{call} tailnet-wait --ping"),
            "hk tailnet-wait has no flag --ping",
        ),
        (
            format!("{call} tailnet-wait --attempts 0"),
            "--attempts takes a whole number from 1 to 100",
        ),
        (
            format!("{call} tailnet-wait --attempts=101"),
            "--attempts takes a whole number from 1 to 100",
        ),
        (
            format!("{call} tailnet-wait --interval-secs 601"),
            "--interval-secs takes a whole number from 0 to 600",
        ),
        (
            format!("{call} tailnet-wait --attempts 2 --attempts 2"),
            "--attempts is given twice",
        ),
        (format!("{call} --token-file t tailnet-wait"), token),
        (format!("{call} tailnet-wait --response-file r"), token),
        (
            "hk --endpoint https://coordinator.example:8443 tailnet-wait".to_owned(),
            "hk tailnet-wait needs --endpoint and --ca-file",
        ),
        (
            "hk --endpoint http://coordinator.example --ca-file ca.pem tailnet-wait".to_owned(),
            "--endpoint takes https://<host>[:<port>]",
        ),
    ];
    for (line, problem) in cases {
        let error = command_of(&line).expect_err(&line);
        assert_eq!(error, format!("{line}\n{problem}"));
    }
    assert_eq!(
        doc_problems("docs/x.md", "`hk tailnet-wait --ping`"),
        ["docs/x.md: `hk tailnet-wait --ping`: tailnet-wait has no flag --ping"]
    );
}

/// Catches the doc check refusing `hk settings init` as hk takes it (in a fenced line or a span,
/// `--help` too), and passing it with another word, a flag it has not, another credential or
/// without one of the three flags, each in hk's own words.
#[test]
fn the_doc_check_reads_settings_init() {
    let all = "hk --endpoint https://coordinator.example:8443 --ca-file ca.pem --token-file t";
    let good = format!("```sh\n{all} settings init\n```\n")
        + "`hk settings init --endpoint URL --ca-file PATH --token-file PATH` and `hk settings --help`.\n";
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    assert_eq!(
        command_of(&format!("{all} settings init")),
        Ok(Some("settings".to_owned()))
    );
    let credential = "hk settings init takes --token-file only: --token-stdin, \
                      --github-oidc-audience and --response-file are not for it";
    let one = "settings takes the one word init; 'hk settings --help' says what it does";
    let cases = [
        (format!("{all} settings"), one),
        (format!("{all} settings other"), one),
        (format!("{all} settings init again"), one),
        (
            format!("{all} settings init --force"),
            "hk settings has no flag --force",
        ),
        (format!("{all} settings init --token-stdin"), credential),
        (format!("{all} settings init --response-file r"), credential),
        (
            "hk --endpoint https://coordinator.example:8443 --ca-file ca.pem settings init"
                .to_owned(),
            "hk settings init needs --endpoint, --ca-file and --token-file",
        ),
        (
            "hk --endpoint http://coordinator.example --ca-file ca.pem --token-file t settings init"
                .to_owned(),
            "--endpoint takes https://<host>[:<port>]",
        ),
    ];
    for (line, problem) in cases {
        let error = command_of(&line).expect_err(&line);
        assert_eq!(error, format!("{line}\n{problem}"));
    }
    assert_eq!(
        doc_problems("docs/x.md", "`hk settings init --force`"),
        ["docs/x.md: `hk settings init --force`: settings has no flag --force"]
    );
}

/// Catches the doc check refusing `hk xcode add` as hk takes it (in a fenced line with no flags,
/// with a version, with a path, with `--mac`, with a global flag or in a span, `--help` too), and
/// passing it with two arguments, `--mac` twice or without a value, a flag it has not, or a token
/// that is not a file.
#[test]
fn the_doc_check_reads_xcode_add() {
    let good = fenced("hk xcode add")
        + &fenced("hk xcode add 27.2")
        + &fenced("hk xcode add ~/Downloads/Xcode_27.2.xip")
        + &fenced("hk xcode add --mac mac-01")
        + &fenced("hk xcode add --mac=mac-01 ~/Downloads/Xcode_27.2.xip")
        + &fenced_call("xcode add 27.2")
        + &fenced("hk --timeout-secs 30 xcode add --help")
        + "`hk xcode add`, `hk xcode add [<version> | <archive.xip>]` and `hk xcode add --help`.\n";
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    assert_eq!(
        command_of("hk xcode add 27.2"),
        Ok(Some("xcode add".to_owned()))
    );
    let one = "hk xcode add takes at most one Xcode version (27.2) or the path of an archive \
               (~/Downloads/Xcode_27.2.xip)";
    let token = "hk xcode add takes --token-file only: --token-stdin, --github-oidc-audience and \
                 --response-file are not for it (stdin is the terminal xcodes asks a two-factor \
                 code on)";
    let cases = [
        ("hk xcode add 27.2 27.1", one),
        ("hk xcode add --force", "hk xcode add has no flag --force"),
        ("hk xcode add --mac a --mac b", "--mac is given twice"),
        ("hk xcode add 27.2 --mac", "--mac needs a value"),
        ("hk xcode add --token-stdin", token),
        ("hk --response-file r xcode add", token),
    ];
    for (line, problem) in cases {
        let error = command_of(line).expect_err(line);
        assert_eq!(error, format!("{line}\n{problem}"));
    }
    assert_eq!(
        doc_problems("docs/x.md", "`hk xcode add --force`"),
        ["docs/x.md: `hk xcode add --force`: xcode add has no flag --force"]
    );
}

/// Catches the doc check refusing `hk xcode verify` as hk takes it (in a fenced line, with no
/// endpoint, with `--mac`, or in a span, `--help` and a global flag too), and passing it with no
/// archive, two, `--apple-root` or `--mac` twice or without a value, a flag it has not, or in a
/// span with a flag it has not; and catches it passing another method of `xcode` that is not in
/// the table.
#[test]
fn the_doc_check_reads_xcode_verify() {
    let good = fenced("hk xcode verify ~/Downloads/Xcode_27.2.xip")
        + &fenced("hk xcode verify --apple-root root.cer ~/Downloads/Xcode_27.2.xip")
        + &fenced("hk --timeout-secs 30 xcode verify --apple-root=root.cer a.xip")
        + &fenced("hk xcode verify --help")
        + &fenced("hk xcode verify --mac mac-01 ~/Downloads/Xcode_27.2.xip")
        + "`hk xcode verify <archive.xip>`, `hk xcode verify --apple-root FILE`, \
           `hk xcode verify --timeout-secs 5 --help` and `hk xcode verify`.\n";
    assert_eq!(doc_problems("docs/x.md", &good), Vec::<String>::new());
    assert_eq!(
        command_of("hk xcode verify a.xip"),
        Ok(Some("xcode verify".to_owned()))
    );
    let one = "hk xcode verify takes the path of one archive (~/Downloads/Xcode_27.2.xip)";
    let cases = [
        ("hk xcode verify", one),
        ("hk xcode verify a.xip b.xip", one),
        ("hk xcode verify --apple-root r.cer", one),
        (
            "hk xcode verify --apple-root r.cer --apple-root r.cer a.xip",
            "--apple-root is given twice",
        ),
        (
            "hk xcode verify a.xip --apple-root",
            "--apple-root needs a value",
        ),
        (
            "hk xcode verify --mac a --mac b a.xip",
            "--mac is given twice",
        ),
        ("hk xcode verify a.xip --mac", "--mac needs a value"),
        (
            "hk xcode verify --force a.xip",
            "hk xcode verify has no flag --force",
        ),
        (
            "hk xcode push a.xip",
            "the service xcode has no method push; 'hk xcode --help' lists them",
        ),
    ];
    for (line, problem) in cases {
        let error = command_of(line).expect_err(line);
        assert_eq!(error, format!("{line}\n{problem}"));
    }
    assert_eq!(
        doc_problems("docs/x.md", "`hk xcode verify --force` and `hk xcode push`"),
        [
            "docs/x.md: `hk xcode verify --force`: xcode verify has no flag --force",
            "docs/x.md: `hk xcode push`: the service xcode has no method push"
        ]
    );
}
