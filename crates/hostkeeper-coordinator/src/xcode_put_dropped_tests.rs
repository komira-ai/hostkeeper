//! `PUT /v1/xcode/<sha256>` whose request is dropped while its body streams. The coordinator, the
//! requests and the bodies are in `xcode_put_support_tests.rs`.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::support::{ARCHIVE, PATIENCE, bearer, data, farm, good_uri, listing, request};
use crate::testutil::OPERATOR;

/// Proves: a request dropped after one frame of its body (the client went away, and the answer is
/// dropped before the body ends) leaves no upload file in the store's directory, stores and audits
/// nothing, and releases the version, so the next upload of it is taken.
///
/// Catches: an upload file left on the disk by a client that disconnects mid-body, and a version
/// reserved for ever by an upload nobody is writing.
#[tokio::test(start_paused = true)]
async fn a_dropped_request_leaves_no_upload_and_frees_the_version() {
    let farm = farm("xcode-put-dropped");
    let mut body = data(&[ARCHIVE]);
    body.stalls = true;
    let polled = body.polled.clone();
    let asked = request("PUT", &good_uri(ARCHIVE), bearer(OPERATOR), body);

    let answered = tokio::time::timeout(
        Duration::from_secs(1),
        farm.send_within(&None, asked, PATIENCE),
    )
    .await;

    assert!(answered.is_err(), "the request was answered");
    assert!(polled.load(Ordering::SeqCst), "the body was never read");
    assert_eq!(listing(&farm.dir), Vec::<String>::new());
    assert_eq!(farm.puts(), Vec::new());
    assert!(
        farm.store.reserve("27.2").is_some(),
        "the version stays reserved"
    );
}
