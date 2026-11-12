//! `hkctl hosts retry`, as the real script sends it: the path it posts to, the request body curl
//! reads, and how an answer and a refusal end.

use crate::rig::{Ran, Rig, Script, args};

/// The call's path on the coordinator the rig's global flags name.
const PATH: &str = "https://127.0.0.1:1/hostkeeper.operator.v1.Hosts/Retry";

/// Runs the real script with `hosts retry` and `flags` after the global flags of a call to a
/// coordinator at a port nothing listens on, the fake curl answering `status` with `body`: what
/// it ended with, and the target and body of the one call it posted.
fn retry(name: &str, flags: &[&str], status: u16, body: &str) -> (Ran, String, String) {
    let rig = Rig::new(name);
    rig.reply(status, body);
    let mut all = rig.globals(1);
    all.extend(args(&["hosts", "retry"]));
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

/// Proves: `hosts retry --node-id N --service S` posts to `Hosts/Retry` a body holding exactly
/// the node id and the service under their JSON names, exits 0 and prints the coordinator's
/// answer (the report set aside and the audit entry) on standard output.
///
/// Catches: a flag that does not reach its field, or a call posted to another method.
#[test]
fn retry_posts_the_node_id_and_the_service() {
    let answer = "{\"setAside\":{\"target\":\"0.3.99\",\"outcome\":\"OUTCOME_FAILED\",\
                  \"orderTry\":\"1\"},\"entry\":{\"seq\":\"254\",\"action\":\"retry\"}}";
    let (ran, url, sent) = retry(
        "retry-posted",
        &["--node-id", "node-4", "--service", "tierstore"],
        200,
        answer,
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, format!("{answer}\n").as_str(), "")
    );
    assert_eq!(url, PATH);
    assert_eq!(sent, "{\"nodeId\":\"node-4\",\"service\":\"tierstore\"}");
}

/// Proves: the refusal `nothing_to_retry` (HTTP 400, `FAILED_PRECONDITION`, as the coordinator
/// answers a host whose last report is no failure) makes `hkctl` exit 2 with the code and the
/// refusal on standard error, whole, and nothing on standard output.
///
/// Catches: a refused retry that exits 0, or whose reason is lost.
#[test]
fn a_refused_retry_exits_2_and_names_the_refusal() {
    let (ran, url, sent) = retry(
        "retry-refused",
        &["--node-id", "node-4", "--service", "tierstore"],
        400,
        "{\"code\":\"FAILED_PRECONDITION\",\"reason\":\"nothing_to_retry\"}",
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            2,
            "",
            "hkctl: the coordinator answered FAILED_PRECONDITION\nrefused: nothing_to_retry\n"
        )
    );
    assert_eq!(url, PATH);
    assert_eq!(sent, "{\"nodeId\":\"node-4\",\"service\":\"tierstore\"}");
}
