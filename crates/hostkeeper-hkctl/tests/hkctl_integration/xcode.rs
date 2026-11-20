//! The `Xcode` calls, as the real script sends them: the path each posts to, the request body
//! curl reads, and how an answer and a refusal end.

use crate::rig::{Ran, Rig, Script, args};

/// The path of a method of `Xcode` on the coordinator the rig's global flags name.
fn path(method: &str) -> String {
    format!("https://127.0.0.1:1/hostkeeper.operator.v1.Xcode/{method}")
}

/// An archive's SHA-256, 64 lower-case hex characters.
const SHA256: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

/// Runs the real script with `words` after the global flags of a call to a coordinator at a port
/// nothing listens on, the fake curl answering `status` with `body`: what it ended with, and the
/// target and body of the one call it posted.
fn run(name: &str, words: &[&str], status: u16, body: &str) -> (Ran, String, String) {
    let rig = Rig::new(name);
    rig.reply(status, body);
    let mut all = rig.globals(1);
    all.extend(args(words));
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

/// Proves: `xcode list` posts to `Xcode/List` the empty request `{}`, exits 0 and prints the
/// coordinator's answer on standard output, with nothing on standard error.
///
/// Catches: a list posted to another method, a body that is not the empty message, and an
/// answer that is lost or altered.
#[test]
fn xcode_list_posts_an_empty_request_and_prints_the_answer() {
    let answer = format!(
        "{{\"configured\":true,\"archives\":[{{\"sha256\":\"{SHA256}\",\"version\":\"26.1.1\",\
         \"build\":\"17B100\",\"sizeBytes\":\"1048576\"}}],\"keep\":2,\"maxBytes\":\"4294967296\"}}"
    );
    let (ran, url, sent) = run("xcode-list", &["xcode", "list"], 200, &answer);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(url, path("List"));
    assert_eq!(sent, "{}");
}

/// Proves: `xcode remove --sha256 H --reason R` posts to `Xcode/Remove` a body holding exactly
/// the SHA-256 and the reason under their JSON names, exits 0 and prints the audit entry the
/// coordinator answers on standard output.
///
/// Catches: a flag that does not reach its field (the digest, the reason), a reason cut at its
/// first space, and a call posted to another method.
#[test]
fn xcode_remove_posts_the_sha256_and_the_reason() {
    let answer = "{\"entry\":{\"seq\":\"12\",\"action\":\"xcode_remove\"}}";
    let (ran, url, sent) = run(
        "xcode-remove",
        &[
            "xcode",
            "remove",
            "--sha256",
            SHA256,
            "--reason",
            "superseded by 26.2",
        ],
        200,
        answer,
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(url, path("Remove"));
    assert_eq!(
        sent,
        format!("{{\"sha256\":\"{SHA256}\",\"reason\":\"superseded by 26.2\"}}")
    );
}

/// Proves: the refusal `unknown_archive` (HTTP 404, `NOT_FOUND`, as the coordinator answers a
/// SHA-256 its store does not hold) makes `xcode remove` exit 1 with the code and the refusal on
/// standard error, whole, and nothing on standard output.
///
/// Catches: a refused removal that exits 0, or whose reason is lost.
#[test]
fn a_removal_of_an_unknown_archive_exits_1_and_names_the_refusal() {
    let (ran, url, sent) = run(
        "xcode-remove-refused",
        &["xcode", "remove", "--sha256", SHA256, "--reason", "stale"],
        404,
        "{\"code\":\"NOT_FOUND\",\"reason\":\"unknown_archive\"}",
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            1,
            "",
            "hkctl: the coordinator answered NOT_FOUND\nrefused: unknown_archive\n"
        )
    );
    assert_eq!(url, path("Remove"));
    assert_eq!(
        sent,
        format!("{{\"sha256\":\"{SHA256}\",\"reason\":\"stale\"}}")
    );
}
