//! The operator-doc check on the hand verbs of `hk` (`ssh`, `ssh-proxy`, `ssh-revoke`,
//! `ssh-status`), which are not in the command table, and on a fenced `hkctl` line.

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
