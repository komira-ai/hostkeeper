//! The push of the coordinator's image: every way an attempt ends (pushed and served byte for
//! byte, the push failing, the read back failing, other bytes served), the waits between
//! attempts, the last attempt, and the command lines of the skopeo adapter, each by its full
//! message.

use std::path::Path;

use super::*;
use crate::release::fakes::{FakeExec, FakeRegistry, FakeWait, scratch};
use crate::release::ports::Registry;
use crate::testdir::TestDir;

const VERSION: &str = "0.1.5";

/// The bytes of the release's image index in the fixtures.
const INDEX: &[u8] = br#"{"manifests":[],"mediaType":"application/vnd.oci.image.index.v1+json"}"#;

/// The digest of [`INDEX`].
fn digest() -> String {
    digest_of(INDEX)
}

/// A release directory holding the image index of [`VERSION`], and the request to push it.
fn release(name: &str) -> (TestDir, Request) {
    let dir = scratch(name);
    std::fs::write(dir.join("hostkeeper-coordinator-0.1.5.image.json"), INDEX)
        .expect("write the index");
    let request = Request {
        dir: dir.to_path_buf(),
        version: VERSION.parse().expect("a version"),
        layout: "/work/layout".into(),
    };
    (dir, request)
}

fn copy_call() -> String {
    format!("copy /work/layout {VERSION} {IMAGE_REPOSITORY}")
}

fn served_call() -> String {
    format!("served {IMAGE_REPOSITORY}@{}", digest())
}

fn pushed_line(attempt: usize) -> String {
    format!(
        "pushed {IMAGE_REPOSITORY}:{VERSION} ({}) in attempt {attempt} of 5; the registry serves \
         hostkeeper-coordinator-0.1.5.image.json byte for byte",
        digest()
    )
}

fn failed_line(attempt: usize, why: &str) -> String {
    format!("attempt {attempt} of 5 failed: {why}")
}

fn unauthorized() -> PortError {
    PortError::new(
        "skopeo copy",
        "uploading layer chunked: unauthorized: unauthenticated: User cannot be authenticated \
         with the token provided.",
    )
}

fn unauthorized_text() -> String {
    unauthorized().to_string()
}

fn registry(
    copies: Vec<Result<(), PortError>>,
    served: Vec<Result<Vec<u8>, PortError>>,
) -> FakeRegistry {
    let registry = FakeRegistry::default();
    *registry.copies.borrow_mut() = copies;
    *registry.served.borrow_mut() = served;
    registry
}

#[test]
fn a_push_the_registry_serves_back_byte_for_byte_succeeds_in_one_attempt_with_no_wait() {
    // Catches: the read back skipped or made before the push, the image tagged other than with
    // the release's version or pushed to another repository, or a wait before the first attempt.
    let (_dir, request) = release("image-push-first");
    let registry = registry(vec![Ok(())], vec![Ok(INDEX.to_vec())]);
    let wait = FakeWait::default();
    assert_eq!(push(&registry, &wait, &request), Ok(vec![pushed_line(1)]));
    assert_eq!(*registry.calls.borrow(), [copy_call(), served_call()]);
    assert!(wait.waits.borrow().is_empty());
}

#[test]
fn a_push_the_registry_refuses_is_tried_again_after_each_wait_until_it_succeeds() {
    // Catches: the run's failure (the registry refusing the job's token for one upload, then for
    // one blob check) ending the job at the first refusal, no wait between attempts, or the
    // earlier failures left out of what the verb prints.
    let (_dir, request) = release("image-push-retry");
    let registry = registry(
        vec![Err(unauthorized()), Err(unauthorized()), Ok(())],
        vec![Ok(INDEX.to_vec())],
    );
    let wait = FakeWait::default();
    let why = unauthorized_text();
    assert_eq!(
        push(&registry, &wait, &request),
        Ok(vec![
            failed_line(1, &why),
            failed_line(2, &why),
            pushed_line(3)
        ])
    );
    assert_eq!(
        *registry.calls.borrow(),
        [copy_call(), copy_call(), copy_call(), served_call()]
    );
    assert_eq!(
        *wait.waits.borrow(),
        [(60, failed_line(1, &why)), (120, failed_line(2, &why))]
    );
}

#[test]
fn a_read_back_that_fails_is_tried_again_with_the_push() {
    // Catches: a failed read back taken for a mismatch (no second attempt), or a second attempt
    // that reads back without pushing again.
    let (_dir, request) = release("image-push-read-back");
    let refused = PortError::new("skopeo inspect", "authentication required");
    let registry = registry(
        vec![Ok(()), Ok(())],
        vec![Err(refused.clone()), Ok(INDEX.to_vec())],
    );
    let wait = FakeWait::default();
    let why = refused.to_string();
    assert_eq!(
        push(&registry, &wait, &request),
        Ok(vec![failed_line(1, &why), pushed_line(2)])
    );
    assert_eq!(
        *registry.calls.borrow(),
        [copy_call(), served_call(), copy_call(), served_call()]
    );
    assert_eq!(*wait.waits.borrow(), [(60, failed_line(1, &why))]);
}

#[test]
fn five_failed_attempts_refuse_the_push_with_every_reason_and_no_wait_after_the_last() {
    // Catches: an endless loop, fewer or more than five attempts, a wait after the last attempt
    // (the job held for nothing), or a refusal that hides the reasons of the earlier attempts.
    let (_dir, request) = release("image-push-all-fail");
    let registry = registry(Vec::new(), Vec::new());
    let wait = FakeWait::default();
    let why = "copy: not scripted";
    let lines: Vec<String> = (1..=5).map(|a| failed_line(a, why)).collect();
    assert_eq!(
        push(&registry, &wait, &request),
        Err(Error::Refused(lines.clone()))
    );
    assert_eq!(*registry.calls.borrow(), vec![copy_call(); 5]);
    let waits: Vec<(u64, String)> = [60, 120, 240, 480].into_iter().zip(lines).collect();
    assert_eq!(*wait.waits.borrow(), waits);
}

#[test]
fn five_failed_read_backs_refuse_the_push() {
    // Catches: a push whose image index the registry never served taken for a success.
    let (_dir, request) = release("image-push-never-served");
    let registry = registry(vec![Ok(()); 5], Vec::new());
    let wait = FakeWait::default();
    let lines: Vec<String> = (1..=5)
        .map(|a| failed_line(a, "served: not scripted"))
        .collect();
    assert_eq!(push(&registry, &wait, &request), Err(Error::Refused(lines)));
    assert_eq!(wait.waits.borrow().len(), 4);
}

#[test]
fn other_bytes_served_for_the_digest_fail_at_once_with_no_other_attempt() {
    // Catches: a registry that serves something else for the release's digest let through, or
    // tried again (no attempt can make a digest name other bytes).
    let (_dir, request) = release("image-push-other-bytes");
    let other = b"{}".to_vec();
    let registry = registry(
        vec![Ok(()), Ok(())],
        vec![Ok(other.clone()), Ok(INDEX.to_vec())],
    );
    let wait = FakeWait::default();
    assert_eq!(
        push(&registry, &wait, &request),
        Err(Error::Failed(format!(
            "{IMAGE_REPOSITORY}@{} serves other bytes than hostkeeper-coordinator-0.1.5.image.json ({})",
            digest(),
            digest_of(&other)
        )))
    );
    assert_eq!(*registry.calls.borrow(), [copy_call(), served_call()]);
    assert!(wait.waits.borrow().is_empty());
}

#[test]
fn a_release_without_its_image_index_pushes_nothing() {
    // Catches: a push with no digest to read it back by.
    let dir = scratch("image-push-no-index");
    let request = Request {
        dir: dir.to_path_buf(),
        version: VERSION.parse().expect("a version"),
        layout: "/work/layout".into(),
    };
    let registry = FakeRegistry::default();
    let result = push(&registry, &FakeWait::default(), &request);
    let path = dir.join("hostkeeper-coordinator-0.1.5.image.json");
    assert!(
        matches!(&result, Err(Error::Io { what, .. }) if *what == format!("read {}", path.display())),
        "{result:?}"
    );
    assert!(registry.calls.borrow().is_empty());
}

#[test]
fn skopeo_copies_every_image_keeping_its_digests_and_reads_the_index_back_raw_by_digest() {
    // Catches: a copy of one platform only (no `--all`), a copy that may rewrite a manifest (no
    // `--preserve-digests`), the wrong source or destination, or a read back by tag.
    let digest = digest();
    let copy = [
        "skopeo",
        "copy",
        "--all",
        "--preserve-digests",
        "oci:/work/layout:0.1.5",
        "docker://ghcr.io/komira-ai/hostkeeper-coordinator:0.1.5",
    ];
    let inspect_ref = format!("docker://ghcr.io/komira-ai/hostkeeper-coordinator@{digest}");
    let inspect = ["skopeo", "inspect", "--raw", inspect_ref.as_str()];
    let exec = FakeExec::default()
        .on(&copy, true, "Copying 2 images\n", "")
        .on(&inspect, true, "{\"manifests\":[]}", "");
    let skopeo = SkopeoCli::new(exec);
    assert_eq!(
        skopeo.copy(Path::new("/work/layout"), "0.1.5", IMAGE_REPOSITORY),
        Ok(())
    );
    assert_eq!(
        skopeo.served(IMAGE_REPOSITORY, &digest),
        Ok(b"{\"manifests\":[]}".to_vec())
    );
    assert_eq!(
        *skopeo.exec.calls.borrow(),
        [
            copy.map(str::to_owned).to_vec(),
            inspect.map(str::to_owned).to_vec()
        ]
    );
}

#[test]
fn a_skopeo_that_fails_is_an_error_with_its_command_line_and_stderr() {
    // Catches: a failed copy read as done, or its reason lost.
    let copy = [
        "skopeo",
        "copy",
        "--all",
        "--preserve-digests",
        "oci:/l:0.1.5",
        "docker://ghcr.io/komira-ai/hostkeeper-coordinator:0.1.5",
    ];
    let exec = FakeExec::default().on(&copy, false, "", "level=fatal msg=\"unauthorized\"\n");
    assert_eq!(
        SkopeoCli::new(exec).copy(Path::new("/l"), "0.1.5", IMAGE_REPOSITORY),
        Err(PortError::new(
            copy.join(" "),
            "level=fatal msg=\"unauthorized\""
        ))
    );
}

#[cfg(unix)]
#[test]
fn a_layout_path_that_is_not_utf8_is_refused_before_skopeo_runs() {
    // Catches: a lossy path handed to skopeo, which would copy another layout.
    use std::os::unix::ffi::OsStrExt;
    let skopeo = SkopeoCli::new(FakeExec::default());
    let layout = Path::new(std::ffi::OsStr::from_bytes(b"/work/\xff"));
    assert_eq!(
        skopeo.copy(layout, "0.1.5", "r"),
        Err(PortError::new(
            "skopeo copy",
            "the layout /work/\u{fffd} is not UTF-8"
        ))
    );
    assert!(skopeo.exec.calls.borrow().is_empty());
}

#[test]
fn the_push_image_verb_pushes_the_layout_it_is_given_through_the_registry_port() {
    // Catches: the verb's directory, version or layout lost between the command line and the
    // push, or the push run without the registry and wait of its ports.
    let (dir, _) = release("image-push-verb");
    let fakes = crate::release::fakes::Fakes::over(Default::default());
    *fakes.registry.copies.borrow_mut() = vec![Ok(())];
    *fakes.registry.served.borrow_mut() = vec![Ok(INDEX.to_vec())];
    let dir_text = dir.to_str().expect("a UTF-8 scratch path");
    let verb = crate::release::tests::verb(&[
        "push-image",
        dir_text,
        "--version",
        VERSION,
        "--layout",
        "/work/layout",
    ]);
    use crate::release::ports::Adapters;
    assert_eq!(
        crate::release::run_with(verb, &fakes.ports()),
        Ok(vec![pushed_line(1)])
    );
    assert_eq!(*fakes.registry.calls.borrow(), [copy_call(), served_call()]);
}

#[test]
fn the_real_wait_of_no_seconds_returns() {
    // Catches: the real wait wired in panicking or blocking; nothing is timed.
    Sleep.wait(0, "attempt 1 of 5 failed: a reason");
}
