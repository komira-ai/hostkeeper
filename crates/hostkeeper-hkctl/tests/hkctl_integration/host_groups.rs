//! The `HostGroups` calls and the new `host_group` fields, as the real script sends them: the
//! request body curl reads, the path it posts to, and how an `UNIMPLEMENTED` answer (the one this
//! release's coordinator gives every `HostGroups` call) ends.

use crate::rig::{Rig, Script, args};

/// `words` after the global flags of a call to a coordinator at a port nothing listens on: the
/// fake curl answers from a canned reply or the test reads what it was sent.
fn call(rig: &Rig, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(1);
    all.extend(args(words));
    all
}

/// The target and the body of the only `curl` call the rig recorded.
fn posted(rig: &Rig) -> (String, String) {
    let curls = rig.curls();
    assert_eq!(curls.len(), 1, "{curls:#?}");
    let url = curls[0]
        .argv
        .iter()
        .skip_while(|a| *a != "--url")
        .nth(1)
        .expect("a url")
        .clone();
    let body = curls[0]
        .read_after("--data-binary", 0)
        .expect("the body is read from a pipe");
    (url, body)
}

/// Proves: `host-groups move` posts to `HostGroups/Move` and its body holds exactly the node id,
/// the host group and the reason, under their JSON names.
#[test]
fn host_groups_move_posts_the_node_the_group_and_the_reason() {
    let rig = Rig::new("hg-move");
    rig.reply(200, "{}");
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            &[
                "host-groups",
                "move",
                "--node-id",
                "web-04",
                "--host-group",
                "web-canary",
                "--reason",
                "r",
            ],
        ),
        b"",
        &[],
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let (url, body) = posted(&rig);
    assert_eq!(
        url,
        "https://127.0.0.1:1/hostkeeper.operator.v1.HostGroups/Move"
    );
    assert_eq!(
        body,
        "{\"nodeId\":\"web-04\",\"hostGroup\":\"web-canary\",\"reason\":\"r\"}"
    );
}

/// Proves: `host-groups put` with a request file posts the file's `hostGroup` object with the
/// `--if-generation` flag (a 64-bit integer, so written as a string) and the reason added.
#[test]
fn host_groups_put_adds_the_flags_to_the_request_file() {
    let rig = Rig::new("hg-put");
    rig.reply(200, "{}");
    let request = rig.put(
        "group.json",
        b"{\"hostGroup\":{\"name\":\"web\",\"packages\":[\"hostkeeper\",\"tierstore\"]}}",
        0o644,
    );
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            &[
                "host-groups",
                "put",
                "--request",
                &request,
                "--if-generation",
                "4",
                "--reason",
                "r",
            ],
        ),
        b"",
        &[],
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let (url, body) = posted(&rig);
    assert_eq!(
        url,
        "https://127.0.0.1:1/hostkeeper.operator.v1.HostGroups/Put"
    );
    assert_eq!(
        body,
        "{\"hostGroup\":{\"name\":\"web\",\"packages\":[\"hostkeeper\",\"tierstore\"]},\"ifGeneration\":\"4\",\"reason\":\"r\"}"
    );
}

/// Proves: a coordinator that answers a `HostGroups` call `UNIMPLEMENTED` with an empty reason
/// (HTTP 501, as this release's coordinator does) makes `hkctl` exit 1, print the code on stderr
/// and no `refused:` line, and nothing on stdout.
#[test]
fn an_unimplemented_host_groups_call_exits_1_and_names_the_code_only() {
    let rig = Rig::new("hg-unimplemented");
    rig.reply(501, "{\"code\":\"UNIMPLEMENTED\",\"reason\":\"\"}");
    let ran = rig.run(
        Script::Real,
        &call(&rig, &["host-groups", "list"]),
        b"",
        &[],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (1, "", "hkctl: the coordinator answered UNIMPLEMENTED\n")
    );
    let (url, _) = posted(&rig);
    assert_eq!(
        url,
        "https://127.0.0.1:1/hostkeeper.operator.v1.HostGroups/List"
    );
}

/// Proves: `releases set-desired` and `rollouts pause` carry `--host-group` as `hostGroup`.
#[test]
fn the_host_group_flag_reaches_set_desired_and_pause() {
    let rig = Rig::new("hg-flag");
    let desired = rig.put(
        "desired.json",
        b"{\"desired\":{\"version\":\"1.2.3\"}}",
        0o644,
    );
    for (words, method, want) in [
        (
            &[
                "releases",
                "set-desired",
                "--service",
                "s",
                "--host-group",
                "web",
                "--request",
                &desired,
            ][..],
            "Releases/SetDesired",
            "\"hostGroup\":\"web\"",
        ),
        (
            &[
                "rollouts",
                "pause",
                "--service",
                "s",
                "--host-group",
                "web",
                "--reason",
                "r",
            ][..],
            "Rollouts/Pause",
            "\"hostGroup\":\"web\"",
        ),
    ] {
        rig.reset();
        rig.reply(200, "{}");
        let ran = rig.run(Script::Real, &call(&rig, words), b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{method}");
        let (url, body) = posted(&rig);
        assert_eq!(
            url,
            format!("https://127.0.0.1:1/hostkeeper.operator.v1.{method}")
        );
        assert!(body.contains(want), "{method}: {body}");
    }
}
