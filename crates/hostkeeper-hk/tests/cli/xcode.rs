//! The `Xcode` commands of the real protos: the path each posts to, the body its flags make, the
//! answer printed, and the refusal that ends a removal.

use crate::rig::{Ran, Rig, Server, call};

/// The path of a method of `Xcode` on the gateway.
fn path(method: &str) -> String {
    format!("/hostkeeper.operator.v1.Xcode/{method}")
}

/// An archive's SHA-256, 64 lower-case hex characters.
const SHA256: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

/// Runs the real `hk` with `words` against a server that answers `status` and `body`: what it
/// ended with, and the path and the body of the one request it posted.
fn post(rig: &Rig, words: &[&str], status: u16, body: &str) -> (Ran, String, String) {
    let server = Server::canned(&rig.pki, status, body);
    let ran = rig.hk(&call(rig, server.port, words));
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "{words:?}: {seen:#?}");
    assert_eq!(seen[0].method, "POST");
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/json"),
        "{words:?}"
    );
    (ran, seen[0].target.clone(), seen[0].text())
}

/// Proves: `xcode list` posts to `Xcode/List` the empty request `{}`, exits 0 and prints the
/// coordinator's answer with one newline on standard output, with nothing on standard error.
#[test]
fn xcode_list_posts_an_empty_request_and_prints_the_answer() {
    let rig = Rig::new("xcode-list");
    let answer = format!(
        "{{\"configured\":true,\"archives\":[{{\"sha256\":\"{SHA256}\",\"version\":\"26.1.1\",\
         \"build\":\"17B100\",\"sizeBytes\":\"1048576\"}}],\"keep\":2,\"maxBytes\":\"4294967296\"}}"
    );
    let (ran, target, sent) = post(&rig, &["xcode", "list"], 200, &answer);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(target, path("List"));
    assert_eq!(sent, "{}");
}

/// Proves: `xcode remove --sha256 H --reason R` posts to `Xcode/Remove` a body holding exactly
/// the SHA-256 and the reason under their JSON names (the reason whole, with its spaces), exits 0
/// and prints the audit entry the coordinator answers.
#[test]
fn xcode_remove_posts_the_sha256_and_the_reason() {
    let rig = Rig::new("xcode-remove");
    let answer = "{\"entry\":{\"seq\":\"12\",\"action\":\"xcode_remove\"}}";
    let (ran, target, sent) = post(
        &rig,
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
    assert_eq!(target, path("Remove"));
    assert_eq!(
        sent,
        format!("{{\"sha256\":\"{SHA256}\",\"reason\":\"superseded by 26.2\"}}")
    );
}

/// Proves: the refusal `unknown_archive` (HTTP 404, `NOT_FOUND`, as the coordinator answers a
/// SHA-256 its store does not hold) makes `xcode remove` exit 1 with the code and the reason on
/// standard error, whole, and nothing on standard output.
#[test]
fn a_removal_of_an_unknown_archive_exits_1_and_names_the_refusal() {
    let rig = Rig::new("xcode-remove-refused");
    let (ran, target, sent) = post(
        &rig,
        &["xcode", "remove", "--sha256", SHA256, "--reason", "stale"],
        404,
        "{\"code\":\"NOT_FOUND\",\"reason\":\"unknown_archive\"}",
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            1,
            "",
            "hk: the coordinator answered NOT_FOUND\nrefused: unknown_archive\n"
        )
    );
    assert_eq!(target, path("Remove"));
    assert_eq!(
        sent,
        format!("{{\"sha256\":\"{SHA256}\",\"reason\":\"stale\"}}")
    );
}
