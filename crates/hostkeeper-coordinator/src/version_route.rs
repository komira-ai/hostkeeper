//! `GET /v1/version` on the private listener: which release this coordinator is, for anyone
//! that reaches the listener, with no token read. A release's deploy waits on it before it sets
//! the hosts' desired version (`release.yml`'s job `coordinator`), and `hkctl` asks it when a
//! call is refused `invalid_request`, to say whether the coordinator is older than the request
//! (docs/deploy/coordinator-update.md).
//!
//! - With no query the answer is `200` and `{"version":"X.Y.Z","commit":"<12 hex>"}`: the crate's
//!   version and [`crate::stamp::BUILD_COMMIT`], what `--version` prints as `X.Y.Z+<commit>`.
//! - With the query `at_least=<version>` it is that `200` when this coordinator's version is at
//!   least that one, in the order of [`Version::release_numbers`] (its leading numbers; a build
//!   suffix does not order), and `409` with
//!   `{"error":"coordinator_older","version":...,"commit":...}` when it is older.
//! - Any other query, or a version that does not parse, is `400` `{"error":"bad_request"}`.
//!
//! The answer names nothing a published release does not: the version and the commit of the
//! coordinator's own build. It reads no state, writes no audit line and costs no token.

use hostkeeper_wire::version::Version;
use hyper::StatusCode;
use serde::Serialize;

use crate::http::{error, json};

/// The reason of the `409`: the coordinator is older than the version asked for.
pub(crate) const COORDINATOR_OLDER: &str = "coordinator_older";

/// The query that asks for a version at least as new.
const AT_LEAST: &str = "at_least=";

/// What the route says of this coordinator.
#[derive(Serialize)]
struct Said<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    version: &'a str,
    commit: &'a str,
}

/// The answer to `GET /v1/version` with `query`, for this build.
pub(crate) fn answer(query: Option<&str>) -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    answer_for(env!("CARGO_PKG_VERSION"), crate::stamp::BUILD_COMMIT, query)
}

/// The answer to `GET /v1/version` with `query`, for a coordinator of `version` built from
/// `commit`.
pub(crate) fn answer_for(
    version: &str,
    commit: &str,
    query: Option<&str>,
) -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    let said = |error| Said {
        error,
        version,
        commit,
    };
    let Some(query) = query else {
        return json(StatusCode::OK, &said(None));
    };
    let asked = query
        .strip_prefix(AT_LEAST)
        .and_then(|text| Version::parse(text).ok());
    let (Some(asked), Ok(running)) = (asked, Version::parse(version)) else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    if running.release_numbers() >= asked.release_numbers() {
        json(StatusCode::OK, &said(None))
    } else {
        json(StatusCode::CONFLICT, &said(Some(COORDINATOR_OLDER)))
    }
}
