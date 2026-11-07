//! The rule that a job's service and container images are pinned by digest, on [`CLEAN`] with a
//! service added to its job.

use super::{CLEAN, assert_problems, with_in};

const DIGEST: &str = "f9a7b9ebf6f56201f949db745e47bc423c0f0f8294bafe991cc10bf74b029657";

/// [`CLEAN`] whose job has one service `postgres` with `body` as its mapping.
fn service(body: &str) -> String {
    with_in(
        CLEAN,
        "    steps:\n",
        &format!("    services:\n      postgres:\n{body}    steps:\n"),
    )
}

/// [`CLEAN`] whose job has one service `postgres` with the image `image`.
fn image(image: &str) -> String {
    service(&format!("        image: {image}\n"))
}

/// [`CLEAN`] whose job has the container `container` (a string or a mapping body).
fn container(container: &str) -> String {
    with_in(
        CLEAN,
        "    steps:\n",
        &format!("    container:{container}\n    steps:\n"),
    )
}

/// Proves: a job's service image is clean only as `name[:tag]@sha256:<64 lowercase hex>`; a
/// tag alone, no tag and no digest, a short digest, a long digest, an uppercase digest,
/// another algorithm, and a service with no image are each refused with their own
/// message, and a job container is held to the same pin, as a string or as `image:`.
/// Catches: a service image a registry can move from under a run, or one the lint skips.
#[test]
fn a_service_image_not_pinned_by_digest_is_refused() {
    let digest = |d: &str| format!("postgres:16@sha256:{d}");
    assert_problems(&image(&digest(DIGEST)), &[]);
    assert_problems(&image(&format!("postgres@sha256:{DIGEST}")), &[]);
    let refused = |who: &str, text: &str| {
        format!("job test: {who}: image {text} is not pinned by digest (name@sha256:<64 hex>)")
    };
    for bad in [
        "postgres:16".to_owned(),
        "postgres".to_owned(),
        digest(&DIGEST[1..]),
        digest(&format!("{DIGEST}0")),
        digest(&DIGEST.to_uppercase()),
        format!("postgres:16@sha512:{DIGEST}"),
    ] {
        assert_problems(&image(&bad), &[&refused("service postgres", &bad)]);
    }
    assert_problems(
        &service("        env:\n          A: b\n"),
        &["job test: service postgres has no image"],
    );
    assert_problems(&container(&format!(" rust@sha256:{DIGEST}")), &[]);
    assert_problems(&container(" rust:1"), &[&refused("container", "rust:1")]);
    assert_problems(
        &container("\n      image: rust:1"),
        &[&refused("container", "rust:1")],
    );
    assert_problems(
        &container("\n      options: --user 1"),
        &["job test: container has no image"],
    );
}
