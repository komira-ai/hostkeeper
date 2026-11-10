//! `hkctl services set-repository`, as the real script sends it: the path it posts to, the
//! request body curl reads, and how an answer and a refusal end.

use crate::rig::{Ran, Rig, Script, args};

/// The call's path on the coordinator the rig's global flags name.
const PATH: &str = "https://127.0.0.1:1/hostkeeper.operator.v1.Services/SetRepository";

/// Runs the real script with `services set-repository` and `flags` after the global flags of a
/// call to a coordinator at a port nothing listens on, the fake curl answering `status` with
/// `body`: what it ended with, and the target and body of the one call it posted.
fn set_repository(name: &str, flags: &[&str], status: u16, body: &str) -> (Ran, String, String) {
    let rig = Rig::new(name);
    rig.reply(status, body);
    let mut all = rig.globals(1);
    all.extend(args(&["services", "set-repository"]));
    all.extend(args(flags));
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

/// Proves: `services set-repository` posts to `Services/SetRepository` a body holding exactly
/// the service, the repository and the reason under their JSON names, exits 0 and prints the
/// coordinator's answer (the audit entry) on standard output.
///
/// Catches: a flag that does not reach its field, or a call posted to another method.
#[test]
fn set_repository_posts_the_service_the_repository_and_the_reason() {
    let answer = "{\"entry\":{\"seq\":\"7\",\"action\":\"set_repository\"}}";
    let (ran, url, sent) = set_repository(
        "sr-set",
        &[
            "--service",
            "tierstore-config-web",
            "--repository",
            "private",
            "--reason",
            "config moves",
        ],
        200,
        answer,
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(url, PATH);
    assert_eq!(
        sent,
        "{\"service\":\"tierstore-config-web\",\"repository\":\"private\",\"reason\":\"config moves\"}"
    );
}

/// Proves: the two ways to go back to the default repository: `--repository ""` sends the empty
/// name, and leaving the flag out sends no `repository`, which the coordinator reads as empty.
///
/// Catches: an empty flag dropped or turned into something else, or a missing flag refused by
/// the script so a setting could not be cleared without naming the empty string.
#[test]
fn an_empty_or_absent_repository_clears_the_setting() {
    let (ran, url, sent) = set_repository(
        "sr-clear-empty",
        &[
            "--service",
            "tierstore",
            "--repository",
            "",
            "--reason",
            "back",
        ],
        200,
        "{}",
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(url, PATH);
    assert_eq!(
        sent,
        "{\"service\":\"tierstore\",\"repository\":\"\",\"reason\":\"back\"}"
    );

    let (ran, url, sent) = set_repository(
        "sr-clear-absent",
        &["--service", "tierstore", "--reason", "back"],
        200,
        "{}",
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(url, PATH);
    assert_eq!(sent, "{\"service\":\"tierstore\",\"reason\":\"back\"}");
}

/// Proves: a refusal (`repository_unknown`, HTTP 404, as the coordinator answers a repository its
/// registry does not hold) makes `hkctl` exit 1 with the code and the refusal on standard error,
/// whole, and nothing on standard output.
///
/// Catches: a refused set that exits 0, or whose reason is lost.
#[test]
fn a_refused_set_repository_exits_1_and_names_the_refusal() {
    let (ran, url, sent) = set_repository(
        "sr-refused",
        &[
            "--service",
            "tierstore",
            "--repository",
            "nope",
            "--reason",
            "r",
        ],
        404,
        "{\"code\":\"NOT_FOUND\",\"reason\":\"repository_unknown\"}",
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            1,
            "",
            "hkctl: the coordinator answered NOT_FOUND\nrefused: repository_unknown\n"
        )
    );
    assert_eq!(url, PATH);
    assert_eq!(
        sent,
        "{\"service\":\"tierstore\",\"repository\":\"nope\",\"reason\":\"r\"}"
    );
}
