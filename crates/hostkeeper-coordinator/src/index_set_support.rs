//! Builders for the tests of a set that names a release index: the text of an index, a set of the
//! desired state that carries it with its SHA-256, and CI's set of it. The coordinator does not
//! read the text, so it is any text that differs with its arguments. The rig is
//! [`crate::marks_support::rig`]: `web` and `db` on the roles `daemon` and `storage`.

use serde_json::{Value, json};

use crate::desired::Desired;
use crate::marks_support::digest;
use crate::service::SetError;
use crate::testutil::{Rig, ci_token};

/// The three platforms of an index, each with the number [`digest`] makes its package digest of.
pub(crate) const ALL: [(&str, u8); 3] =
    [("linux-x86_64", 1), ("linux-arm64", 2), ("macos-arm64", 3)];

/// The text of an index of `service` at `version` that lists, for each of `packages`, the package
/// whose digest is [`digest`] of its number.
pub(crate) fn index_text(service: &str, version: &str, packages: &[(&str, u8)]) -> String {
    let listed: Vec<Value> = packages
        .iter()
        .map(|(platform, n)| json!({"platform": platform, "sha256": digest(*n), "bytes": 4096}))
        .collect();
    json!({"service": service, "version": version, "packages": listed}).to_string()
}

/// The SHA-256 of `text`, in lowercase hex.
pub(crate) fn sha(text: &str) -> String {
    crate::ops_file::digest(text.as_bytes())
}

/// The body of a set of `service` at `version` that names the index `text` and its digest
/// `digest_of_text`.
pub(crate) fn index_body(service: &str, version: &str, text: &str, digest_of_text: &str) -> Value {
    json!({
        "service": service,
        "version": version,
        "min_up": {"storage": 0, "frontend": 0},
        "release_index": text,
        "release_index_digest": digest_of_text,
    })
}

/// CI's set of `body`, with a token no one has used.
pub(crate) async fn try_set(rig: &Rig, body: &Value) -> Result<Desired, SetError> {
    rig.coordinator
        .set_desired(Some(&ci_token()), body.to_string().as_bytes())
        .await
}

/// CI sets `service` at `version` over the index that lists `packages`; the SHA-256 of the
/// index.
pub(crate) async fn set_index(
    rig: &Rig,
    service: &str,
    version: &str,
    packages: &[(&str, u8)],
) -> String {
    let text = index_text(service, version, packages);
    let digest = sha(&text);
    try_set(rig, &index_body(service, version, &text, &digest))
        .await
        .expect("the set is accepted");
    digest
}
