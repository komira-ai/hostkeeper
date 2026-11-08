//! The coordinator's container build file, `packaging/container/Dockerfile`: a distroless base
//! pinned by digest, the released binary and the operator web app (`ui/`, at the path the
//! coordinator serves when given no `--ui-dir`) copied in and nothing else, a non-root user, the
//! binary as the entrypoint, and no environment variable, build argument, default argument (the
//! coordinator's configuration is its flags, AGENTS.md S2) or health check (the image has no shell
//! or client to run one; a probe outside the container asks `serve --health-listen`'s
//! `GET /healthz`, docs/design/two-programs.md section 2.7). The file is read as text; nothing
//! here builds an image.

use std::fs;
use std::path::PathBuf;

use crate::altered;

/// The base image, up to its digest: distroless with glibc and libgcc, for a dynamically linked
/// Rust binary, as the `nonroot` user.
const BASE: &str = "gcr.io/distroless/cc-debian13:nonroot@sha256:";

/// The binary copied in, and how.
const COPY: &str = "COPY --chmod=0555 hostkeeper-coordinator /usr/bin/hostkeeper-coordinator";

/// The web app copied in: to the coordinator's default UI directory, keeping the modes of the
/// context (a `--chmod` would set its directories' mode too, and no one could enter them).
const COPY_UI: &str = "COPY ui /usr/share/hostkeeper-coordinator/ui";

/// The user the coordinator runs as: distroless's `nonroot`.
const USER: &str = "USER 65532:65532";

/// The entrypoint: the binary, with no argument.
const ENTRYPOINT: &str = "ENTRYPOINT [\"/usr/bin/hostkeeper-coordinator\"]";

fn dockerfile() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/container/Dockerfile");
    fs::read_to_string(path).expect("read packaging/container/Dockerfile")
}

/// Whether `text` is 64 lowercase hex digits.
fn is_digest(text: &str) -> bool {
    (text.len() == 64) & text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Passes when the instructions of `text` (its lines that are neither blank nor comments) are
/// exactly: `FROM` the [`BASE`] with a digest, [`COPY`], [`COPY_UI`], [`USER`] and
/// [`ENTRYPOINT`], in that order.
fn check_dockerfile(text: &str) -> Result<(), String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() & !l.starts_with('#'))
        .collect();
    let [from, rest @ ..] = lines.as_slice() else {
        return Err("the file has no instruction".to_owned());
    };
    let pinned = from
        .strip_prefix("FROM ")
        .and_then(|image| image.strip_prefix(BASE))
        .is_some_and(is_digest);
    if !pinned {
        return Err(format!("`{from}` is not `FROM {BASE}<64 hex digits>`"));
    }
    let want = [COPY, COPY_UI, USER, ENTRYPOINT];
    if rest != want {
        return Err(format!(
            "the instructions after FROM are {rest:?}, not {want:?}"
        ));
    }
    Ok(())
}

/// Catches an unpinned or moved base, a base other than distroless, a file copied in other than
/// the binary and the web app, the web app copied elsewhere or with a mode for its directories, a
/// root user, an environment variable, a build argument, default arguments, a `HEALTHCHECK`, or an
/// entrypoint other than the binary.
#[test]
fn the_coordinator_image_is_the_binary_on_a_pinned_base_as_nonroot_with_flags_only() {
    let text = dockerfile();
    assert_eq!(check_dockerfile(&text), Ok(()));
    let digest = "@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1";
    let cases = [
        (altered(&text, digest, ""), "is not `FROM"),
        (altered(&text, digest, "@sha256:08476c24"), "is not `FROM"),
        (
            altered(&text, "gcr.io/distroless/cc-debian13", "debian"),
            "is not `FROM",
        ),
        (altered(&text, USER, "USER 0:0"), "not [\""),
        (
            altered(
                &text,
                ENTRYPOINT,
                "ENV RUST_LOG=info\nENTRYPOINT [\"/usr/bin/hostkeeper-coordinator\"]",
            ),
            "\"ENV RUST_LOG=info\"",
        ),
        (
            altered(
                &text,
                ENTRYPOINT,
                "ARG COMMIT\nENTRYPOINT [\"/usr/bin/hostkeeper-coordinator\"]",
            ),
            "\"ARG COMMIT\"",
        ),
        (
            altered(
                &text,
                ENTRYPOINT,
                "ENTRYPOINT [\"/usr/bin/hostkeeper-coordinator\"]\nCMD [\"serve\"]",
            ),
            "CMD",
        ),
        (
            altered(
                &text,
                ENTRYPOINT,
                "HEALTHCHECK CMD [\"/usr/bin/hostkeeper-coordinator\", \"healthcheck\"]\n\
                 ENTRYPOINT [\"/usr/bin/hostkeeper-coordinator\"]",
            ),
            "HEALTHCHECK",
        ),
        (altered(&text, COPY, "COPY . /usr/bin/"), "COPY . /usr/bin/"),
        (
            altered(&text, COPY_UI, "COPY ui /srv/ui"),
            "\"COPY ui /srv/ui\"",
        ),
        (
            altered(
                &text,
                COPY_UI,
                "COPY --chmod=0444 ui /usr/share/hostkeeper-coordinator/ui",
            ),
            "--chmod=0444 ui",
        ),
        (altered(&text, &format!("{COPY_UI}\n"), ""), "not [\""),
        (
            altered(
                &text,
                COPY_UI,
                "COPY ui /usr/share/hostkeeper-coordinator/ui\nCOPY secrets /etc/secrets",
            ),
            "COPY secrets",
        ),
        (
            altered(&text, ENTRYPOINT, "ENTRYPOINT [\"/bin/sh\", \"-c\"]"),
            "/bin/sh",
        ),
    ];
    for (changed, words) in cases {
        match check_dockerfile(&changed) {
            Ok(()) => panic!("accepted a file that must be refused for {words:?}:\n{changed}"),
            Err(message) => assert!(message.contains(words), "{message:?} lacks {words:?}"),
        }
    }
    assert_eq!(
        check_dockerfile("# only a comment\n"),
        Err("the file has no instruction".to_owned())
    );
}
