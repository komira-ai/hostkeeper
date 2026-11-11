use super::*;
use crate::channels::pkg::testing::*;
use crate::channels::pkg::upload::upload;

const PREFIX: &str = "pkg/hostkeeper/1.2.3/";
const STABLE: &str = "pkg/hostkeeper/stable.json";

fn json(bytes: Vec<u8>) -> Value {
    serde_json::from_slice(&bytes).expect("json")
}

/// A bucket that holds the testing directory of the release 1.2.3.
fn uploaded(f: &Fixture) -> FakeAws {
    let aws = FakeAws::new();
    upload(&aws, &f.request("")).expect("uploaded");
    aws.calls.borrow_mut().clear();
    aws
}

#[test]
fn the_first_promotion_reads_the_directory_back_and_then_writes_the_pointer_once() {
    let f = Fixture::new("first");
    let aws = uploaded(&f);
    let said = promote(&aws, &f.request("EDIST123")).expect("promoted");
    // Everything is read before the pointer is written.
    let order: Vec<(String, String)> = aws
        .calls
        .borrow()
        .iter()
        .map(|c| (c[1].clone(), flag(c, "--key").unwrap_or_default()))
        .collect();
    assert_eq!(
        order,
        [
            ("get-object", format!("{PREFIX}hello-1.2.3-macos-arm64.pkg")),
            (
                "get-object",
                format!("{PREFIX}hello-1.2.3-macos-arm64.pkg.sigstore.json")
            ),
            (
                "get-object",
                format!("{PREFIX}hostkeeper-1.2.3-macos-arm64.pkg")
            ),
            (
                "get-object",
                format!("{PREFIX}hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json")
            ),
            ("get-object", format!("{PREFIX}SHA256SUMS")),
            ("get-object", STABLE.to_owned()),
            ("put-object", STABLE.to_owned()),
            ("create-invalidation", String::new()),
        ]
        .map(|(a, b)| (a.to_owned(), b))
    );
    assert_eq!(aws.keys_put(), [STABLE]);
    let put = &aws.calls_of("put-object")[0];
    assert_eq!(flag(put, "--if-none-match").as_deref(), Some("*"));
    assert_eq!(flag(put, "--if-match"), None);
    assert_eq!(
        flag(put, "--cache-control").as_deref(),
        Some("public, max-age=60")
    );
    assert_eq!(
        flag(put, "--content-type").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        aws.calls_of("create-invalidation"),
        [[
            "cloudfront",
            "create-invalidation",
            "--distribution-id",
            "EDIST123",
            "--paths",
            "/pkg/hostkeeper/stable.json"
        ]]
    );
    assert_eq!(said.len(), 7);
    assert_eq!(said[5], format!("{STABLE}: now 1.2.3"));
    assert_eq!(said[6], "/pkg/hostkeeper/stable.json: invalidated");
    assert!(said[0].starts_with(&format!("{PREFIX}hello-1.2.3-macos-arm64.pkg: sha256 ")));
    assert!(said[2].starts_with(&format!(
        "{PREFIX}hostkeeper-1.2.3-macos-arm64.pkg: sha256 "
    )));
}

#[test]
fn the_pointer_names_the_release_and_lists_each_installer_and_its_bundle_with_size_and_digest() {
    let f = Fixture::new("contents");
    let aws = uploaded(&f);
    promote(&aws, &f.request("")).expect("promoted");
    let pointer = json(aws.get(STABLE).expect("pointer"));
    assert_eq!(pointer["version"], "1.2.3");
    assert_eq!(pointer["tag"], "v1.2.3");
    assert_eq!(pointer["commit"], COMMIT);
    let files = pointer["files"].as_object().expect("files");
    assert_eq!(files.keys().cloned().collect::<Vec<_>>(), names());
    let installer = &files["hostkeeper-1.2.3-macos-arm64.pkg"];
    assert_eq!(
        installer["size"],
        "bytes of hostkeeper-1.2.3-macos-arm64.pkg".len()
    );
    assert_eq!(
        installer["sha256"],
        Sha256::of(b"bytes of hostkeeper-1.2.3-macos-arm64.pkg").to_string()
    );
    let hello = &files["hello-1.2.3-macos-arm64.pkg"];
    assert_eq!(hello["size"], "bytes of hello-1.2.3-macos-arm64.pkg".len());
    assert_eq!(
        hello["sha256"],
        Sha256::of(b"bytes of hello-1.2.3-macos-arm64.pkg").to_string()
    );
    // No distribution given: nothing is invalidated.
    assert!(aws.calls_of("create-invalidation").is_empty());
}

#[test]
fn a_directory_that_is_not_what_the_release_built_leaves_the_pointer_alone() {
    // Bytes that differ from the release's sums, and a file that is not there.
    let f = Fixture::new("different");
    let aws = uploaded(&f);
    let key = format!("{PREFIX}hostkeeper-1.2.3-macos-arm64.pkg");
    aws.put(&key, b"other");
    let Error::Failed(why) = promote(&aws, &f.request("EDIST123")).unwrap_err() else {
        panic!("failed")
    };
    assert!(why.starts_with(&format!("{key} holds sha256 ")), "{why}");
    assert_eq!(aws.get(STABLE), None);
    assert!(aws.calls_of("create-invalidation").is_empty());

    let g = Fixture::new("missing");
    let aws = FakeAws::new();
    let e = promote(&aws, &g.request("")).unwrap_err();
    assert_eq!(
        e,
        Error::Failed(format!(
            "{PREFIX}hello-1.2.3-macos-arm64.pkg could not be read back: {NO_SUCH_KEY}"
        ))
    );

    // The directory's own sums are held to the digest too.
    let h = Fixture::new("sums");
    let aws = uploaded(&h);
    aws.put(&format!("{PREFIX}SHA256SUMS"), b"not the sums\n");
    let Error::Failed(why) = promote(&aws, &h.request("")).unwrap_err() else {
        panic!("failed")
    };
    assert!(
        why.starts_with(&format!("{PREFIX}SHA256SUMS holds sha256 ")),
        "{why}"
    );
    assert_eq!(aws.get(STABLE), None);
}

#[test]
fn a_release_whose_files_are_not_the_sums_is_refused_before_the_bucket_is_read() {
    let f = Fixture::new("local");
    std::fs::remove_file(f.release.join("hostkeeper-1.2.3-macos-arm64.pkg")).expect("remove");
    let aws = FakeAws::new();
    assert!(matches!(
        promote(&aws, &f.request("")),
        Err(Error::Refused(_))
    ));
    assert!(aws.calls.borrow().is_empty());
}

#[test]
fn a_newer_pointer_is_left_alone() {
    let f = Fixture::new("newer");
    let aws = uploaded(&f);
    aws.put(STABLE, br#"{"version": "1.10.0"}"#);
    let said = promote(&aws, &f.request("EDIST123")).expect("promoted");
    assert_eq!(
        said.last().map(String::as_str),
        Some("pkg/hostkeeper/stable.json: at 1.10.0, newer than 1.2.3: left as it is")
    );
    assert_eq!(aws.get(STABLE).expect("kept"), br#"{"version": "1.10.0"}"#);
    assert!(aws.calls_of("create-invalidation").is_empty());
}

#[test]
fn a_pointer_at_this_version_is_not_written_again_but_invalidated_again() {
    let f = Fixture::new("equal");
    let aws = uploaded(&f);
    promote(&aws, &f.request("EDIST123")).expect("first");
    let before = aws.store.borrow().clone();
    let said = promote(&aws, &f.request("EDIST123")).expect("second");
    assert_eq!(*aws.store.borrow(), before);
    assert_eq!(said[said.len() - 2], format!("{STABLE}: already at 1.2.3"));
    assert_eq!(
        said[said.len() - 1],
        "/pkg/hostkeeper/stable.json: invalidated"
    );
    assert_eq!(aws.keys_put(), [STABLE]);
}

#[test]
fn an_older_pointer_is_replaced_under_if_match_on_the_etag_that_was_read() {
    // The versions are compared as numbers: 1.2.3 is above 1.1.9, and above 1.2.2.
    for (n, stored) in ["1.2.2", "1.1.9"].into_iter().enumerate() {
        let f = Fixture::new(&format!("older-{n}"));
        let aws = uploaded(&f);
        let text = format!("{{\"version\": \"{stored}\"}}");
        aws.put(STABLE, text.as_bytes());
        promote(&aws, &f.request("")).expect("promoted");
        let put = aws.calls_of("put-object").pop().expect("a put");
        assert_eq!(flag(&put, "--key").as_deref(), Some(STABLE));
        assert_eq!(flag(&put, "--if-match"), Some(etag(text.as_bytes())));
        assert_eq!(flag(&put, "--if-none-match"), None);
        assert_eq!(json(aws.get(STABLE).expect("pointer"))["version"], "1.2.3");
    }
}

#[test]
fn a_pointer_the_role_may_not_see_is_taken_for_absent_and_written_without_overwriting() {
    // A role that may not list the bucket is answered `AccessDenied` for a key that is not there.
    let f = Fixture::new("denied-absent");
    let aws = uploaded(&f);
    *aws.missing.borrow_mut() = DENIED.to_owned();
    promote(&aws, &f.request("")).expect("promoted");
    let put = aws.calls_of("put-object").pop().expect("a put");
    assert_eq!(flag(&put, "--if-none-match").as_deref(), Some("*"));
}

#[test]
fn a_concurrent_write_of_the_pointer_makes_the_verb_read_and_decide_again() {
    // The other release is older: the second try writes ours. Or newer: ours is left alone.
    let f = Fixture::new("race-older");
    let aws = uploaded(&f);
    aws.put(STABLE, br#"{"version": "1.0.0"}"#);
    aws.before_put_of(
        "stable.json",
        Box::new(|store| {
            store.insert(STABLE.to_owned(), br#"{"version": "1.2.0"}"#.to_vec());
        }),
    );
    promote(&aws, &f.request("")).expect("promoted");
    assert_eq!(aws.keys_put(), [STABLE, STABLE]);
    assert_eq!(json(aws.get(STABLE).expect("pointer"))["version"], "1.2.3");

    let g = Fixture::new("race-newer");
    let aws = uploaded(&g);
    aws.put(STABLE, br#"{"version": "1.0.0"}"#);
    aws.before_put_of(
        "stable.json",
        Box::new(|store| {
            store.insert(STABLE.to_owned(), br#"{"version": "1.3.0"}"#.to_vec());
        }),
    );
    let said = promote(&aws, &g.request("")).expect("promoted");
    assert_eq!(
        said.last().map(String::as_str),
        Some("pkg/hostkeeper/stable.json: at 1.3.0, newer than 1.2.3: left as it is")
    );
    assert_eq!(aws.get(STABLE).expect("kept"), br#"{"version": "1.3.0"}"#);
}

#[test]
fn a_pointer_that_keeps_changing_fails_after_the_attempts() {
    let f = Fixture::new("race-always");
    let aws = uploaded(&f);
    aws.put(STABLE, br#"{"version": "1.0.0"}"#);
    // Every write of the pointer loses the race.
    aws.refuse_when("--key pkg/hostkeeper/stable.json --body", PRECONDITION);
    let mut request = f.request("");
    request.attempts = 2;
    assert_eq!(
        promote(&aws, &request).unwrap_err(),
        Error::Failed(
            "pkg/hostkeeper/stable.json was written by another release 2 times in a row; run the job again"
                .to_owned()
        )
    );
}

#[test]
fn a_pointer_that_cannot_be_read_or_understood_fails_and_is_not_written_over() {
    let no_version =
        "pkg/hostkeeper/stable.json in the bucket has no <major>.<minor>.<patch> version";
    // (what the bucket holds, what `get-object` prints, the failure)
    let cases: [(&str, Option<&str>, &str); 6] = [
        (
            "not json",
            None,
            "pkg/hostkeeper/stable.json in the bucket is not JSON",
        ),
        (r#"{"tag": "v1.0.0"}"#, None, no_version),
        (r#"{"version": "1.0"}"#, None, no_version),
        (
            r#"{"version": "99999999999999999999.0.0"}"#,
            None,
            no_version,
        ),
        (
            r#"{"version": "1.0.0"}"#,
            Some("not json at all"),
            "read pkg/hostkeeper/stable.json: aws printed no ETag",
        ),
        (
            r#"{"version": "1.0.0"}"#,
            Some("{\"ContentLength\": 3}"),
            "read pkg/hostkeeper/stable.json: aws printed no ETag",
        ),
    ];
    for (n, (stored, printed, want)) in cases.into_iter().enumerate() {
        let f = Fixture::new(&format!("unreadable-{n}"));
        let aws = uploaded(&f);
        aws.put(STABLE, stored.as_bytes());
        *aws.stdout.borrow_mut() = printed.map(str::to_owned);
        let Error::Failed(why) = promote(&aws, &f.request("")).unwrap_err() else {
            panic!("{stored}")
        };
        assert!(why.starts_with(want), "{stored}: {why}");
        assert_eq!(aws.get(STABLE).expect("kept"), stored.as_bytes());
    }
}

#[test]
fn a_refused_read_or_write_of_the_pointer_or_a_refused_invalidation_fails() {
    let f = Fixture::new("read-refused");
    let aws = uploaded(&f);
    aws.refuse_when(&format!("--key {STABLE}"), "Throttled");
    assert_eq!(
        promote(&aws, &f.request("")).unwrap_err(),
        Error::Failed("read pkg/hostkeeper/stable.json: Throttled".to_owned())
    );

    let g = Fixture::new("put-refused");
    let aws = uploaded(&g);
    aws.refuse_when("--key pkg/hostkeeper/stable.json --body", "Access Denied");
    assert_eq!(
        promote(&aws, &g.request("")).unwrap_err(),
        Error::Failed("put pkg/hostkeeper/stable.json: Access Denied".to_owned())
    );

    let h = Fixture::new("invalidation-refused");
    let aws = uploaded(&h);
    aws.refuse_when("create-invalidation", "no rights");
    assert_eq!(
        promote(&aws, &h.request("EDIST123")).unwrap_err(),
        Error::Failed("invalidate /pkg/hostkeeper/stable.json: no rights".to_owned())
    );

    // A re-run at the version already stored invalidates too, and fails the same way.
    let i = Fixture::new("invalidation-refused-again");
    let aws = uploaded(&i);
    promote(&aws, &i.request("")).expect("first");
    aws.refuse_when("create-invalidation", "no rights");
    assert_eq!(
        promote(&aws, &i.request("EDIST123")).unwrap_err(),
        Error::Failed("invalidate /pkg/hostkeeper/stable.json: no rights".to_owned())
    );
}

#[test]
fn flags_out_of_their_form_are_usage_errors_before_anything_is_read() {
    let f = Fixture::new("usage");
    let aws = FakeAws::new();
    let mut request = f.request("");
    request.attempts = 0;
    assert!(matches!(promote(&aws, &request), Err(Error::Usage(_))));
    assert!(aws.calls.borrow().is_empty());
}
