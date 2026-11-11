//! The `Users` and `ApiTokens` calls, as the real script sends them: the path it posts to and
//! the body curl reads, and that the one-time password or token of an answer reaches stdout
//! as it came and is not echoed anywhere else.

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

/// Proves: each call of `Users` and `ApiTokens` that takes a name posts to its own method with a
/// body that holds exactly the name, and exits 0.
#[test]
fn the_calls_that_take_a_name_post_exactly_the_name_to_their_method() {
    for (words, method) in [
        (&["users", "create", "--name", "carol"][..], "Users/Create"),
        (
            &["users", "disable", "--name", "carol"][..],
            "Users/Disable",
        ),
        (&["users", "enable", "--name", "carol"][..], "Users/Enable"),
        (
            &["users", "reset-password", "--name", "carol"][..],
            "Users/ResetPassword",
        ),
        (&["users", "delete", "--name", "carol"][..], "Users/Delete"),
        (
            &["api-tokens", "create", "--name", "carol"][..],
            "ApiTokens/Create",
        ),
        (
            &["api-tokens", "revoke", "--name", "carol"][..],
            "ApiTokens/Revoke",
        ),
    ] {
        let rig = Rig::new("accounts-name");
        rig.reply(200, "{}");
        let ran = rig.run(Script::Real, &call(&rig, words), b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{method}");
        let (url, body) = posted(&rig);
        assert_eq!(
            url,
            format!("https://127.0.0.1:1/hostkeeper.operator.v1.{method}")
        );
        assert_eq!(body, "{\"name\":\"carol\"}", "{method}");
    }
}

/// Proves: `users list` and `api-tokens list` post an empty request to their methods, and a
/// password the coordinator answers once is printed on stdout exactly as it came.
#[test]
fn the_lists_post_an_empty_request_and_an_answered_password_is_printed_once() {
    for (words, method) in [
        (&["users", "list"][..], "Users/List"),
        (&["api-tokens", "list"][..], "ApiTokens/List"),
    ] {
        let rig = Rig::new("accounts-list");
        rig.reply(200, "{}");
        let ran = rig.run(Script::Real, &call(&rig, words), b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{method}");
        let (url, body) = posted(&rig);
        assert_eq!(
            url,
            format!("https://127.0.0.1:1/hostkeeper.operator.v1.{method}")
        );
        assert_eq!(body, "{}", "{method}");
    }

    let rig = Rig::new("accounts-password");
    let answer = "{\"user\":{\"name\":\"carol\"},\"password\":\"Abcdefghjkmnpqrstuvwxyz2\"}";
    rig.reply(200, answer);
    let ran = rig.run(
        Script::Real,
        &call(&rig, &["users", "create", "--name", "carol"]),
        b"",
        &[],
    );
    assert_eq!(ran.code, 0);
    assert_eq!(ran.stdout.matches("Abcdefghjkmnpqrstuvwxyz2").count(), 1);
    assert_eq!(ran.stderr, "");
}
