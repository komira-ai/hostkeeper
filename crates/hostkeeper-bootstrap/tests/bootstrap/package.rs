//! The descriptor and pointer that name the pkg, and the download that is held to them.

use std::fs;

use hostkeeper_bootstrap::package::{PackageError, download, fetch, locate};
use hostkeeper_bootstrap::settings::Settings;

use crate::support::*;

fn settings(extra: &str) -> Settings {
    let dir = scratch("package-settings");
    paths_in(dir.path(), extra)
        .sources
        .load()
        .expect("settings")
}

/// The descriptor names the directory, the pointer names the version, size and digest, and the
/// pkg's URL is under the version's directory.
#[test]
fn the_pointer_names_the_pkg_of_the_stable_version() {
    let repo = Repo::good();
    let located = locate(&repo, &settings("")).expect("located");
    assert_eq!(located.version, VERSION);
    assert_eq!(located.url, PKG_URL);
    assert_eq!(located.sha256, sha256_hex(PKG_BYTES));
    assert_eq!(located.size, PKG_BYTES.len() as u64);
    assert_eq!(*repo.asked.borrow(), [DESCRIPTOR_URL, POINTER_URL]);
}

/// The pkg lands in a new 0600 file, byte for byte, when its size and digest are the pointer's.
#[test]
fn the_download_is_written_whole_when_it_matches_the_pointer() {
    let dir = scratch("package-ok");
    let repo = Repo::good();
    let settings = settings("");
    let located = locate(&repo, &settings).expect("located");
    let staging = dir.path().join("hostkeeper.pkg");
    download(&repo, &settings, &located, &staging).expect("downloaded");
    assert_eq!(fs::read(&staging).expect("pkg"), PKG_BYTES);
    assert_eq!(mode(&staging), 0o600);
    let again = download(&repo, &settings, &located, &staging).expect_err("never replaced");
    assert!(matches!(again, PackageError::Staging { .. }), "{again}");
}

/// A body of another size than the pointer's is refused, whether it is longer or shorter.
#[test]
fn a_wrong_size_is_refused() {
    let dir = scratch("package-size");
    let settings = settings("");
    let mut repo = Repo::good();
    let located = locate(&repo, &settings).expect("located");
    repo.answers
        .insert(PKG_URL.into(), (200, PKG_BYTES[..10].to_vec()));
    let error = download(&repo, &settings, &located, &dir.path().join("a.pkg")).expect_err("short");
    assert!(
        matches!(error, PackageError::Size { found: 10, .. }),
        "{error}"
    );
    repo.answers
        .insert(PKG_URL.into(), (200, [PKG_BYTES, b"more"].concat()));
    let error = download(&repo, &settings, &located, &dir.path().join("b.pkg")).expect_err("long");
    assert!(matches!(error, PackageError::TooLarge { .. }), "{error}");
}

/// A body of the right size and another digest is refused, naming both digests.
#[test]
fn a_wrong_digest_is_refused() {
    let dir = scratch("package-digest");
    let settings = settings("");
    let mut repo = Repo::good();
    let located = locate(&repo, &settings).expect("located");
    let other = vec![b'x'; PKG_BYTES.len()];
    repo.answers.insert(PKG_URL.into(), (200, other.clone()));
    let error =
        download(&repo, &settings, &located, &dir.path().join("a.pkg")).expect_err("digest");
    assert_eq!(
        error.to_string(),
        format!(
            "the pkg's SHA-256 is {}, the pointer says {}",
            sha256_hex(&other),
            sha256_hex(PKG_BYTES)
        )
    );
}

/// The download never writes more than `max_package_bytes`: a pkg over the bound is refused
/// from its announced length, and a body that outruns its announced length is cut at the bound.
#[test]
fn the_package_bound_is_enforced_by_announcement_and_by_count() {
    let dir = scratch("package-bound");
    let settings = settings("[bounds]\nmax_package_bytes = 16\n");
    let repo = Repo::good();
    let located = locate(&repo, &settings).expect("located");
    let error = download(&repo, &settings, &located, &dir.path().join("a.pkg")).expect_err("over");
    assert!(
        matches!(error, PackageError::TooLarge { limit: 16, .. }),
        "{error}"
    );
    let mut liar = Repo::good();
    liar.announced.insert(PKG_URL.into(), 1);
    let error = download(&liar, &settings, &located, &dir.path().join("b.pkg")).expect_err("over");
    assert!(
        matches!(error, PackageError::TooLarge { limit: 16, .. }),
        "{error}"
    );
    assert!(
        fs::metadata(dir.path().join("b.pkg"))
            .expect("partial file")
            .len()
            <= 17
    );
}

/// A down network, a status other than 200 and a body that cannot be read are each an error that
/// names the URL.
#[test]
fn network_failures_name_the_url() {
    let repo = Repo::good();
    repo.down.set(true);
    let error = locate(&repo, &settings("")).expect_err("down");
    assert!(matches!(error, PackageError::Net { .. }), "{error}");
    assert!(
        error
            .to_string()
            .starts_with(&format!("GET {DESCRIPTOR_URL}"))
    );
    let mut missing = Repo::good();
    missing.answers.remove(POINTER_URL);
    let error = locate(&missing, &settings("")).expect_err("404");
    assert!(
        matches!(error, PackageError::Status { status: 404, .. }),
        "{error}"
    );
}

/// The descriptor must hold a macos-pkg format for the Mac's platform, and parse.
#[test]
fn a_descriptor_without_the_mac_format_is_refused() {
    let settings = settings("");
    let mut repo = Repo::good();
    repo.serve(DESCRIPTOR_URL, "[]");
    assert!(matches!(
        locate(&repo, &settings),
        Err(PackageError::Descriptor(_))
    ));
    let without = descriptor().replace("macos-pkg", "other");
    repo.serve(DESCRIPTOR_URL, &without);
    assert!(matches!(
        locate(&repo, &settings),
        Err(PackageError::Descriptor(_)) | Err(PackageError::NoMacosPkg)
    ));
}

/// A pointer that is not JSON, has no version, a version that is not a path component, no entry
/// for the file, or an entry with no digest or size, is refused with what is wrong.
#[test]
fn a_malformed_pointer_is_refused() {
    let settings = settings("");
    let name = format!("hostkeeper-{VERSION}-macos-arm64.pkg");
    let digest = sha256_hex(PKG_BYTES);
    let cases = [
        ("not json".to_owned(), "expected"),
        ("{}".to_owned(), "version is missing"),
        (
            r#"{"version":"../x","files":{}}"#.to_owned(),
            "version is missing",
        ),
        (
            r#"{"version":"","files":{}}"#.to_owned(),
            "version is missing",
        ),
        (
            format!(r#"{{"version":"{VERSION}","files":{{}}}}"#),
            "lists no",
        ),
        (
            format!(r#"{{"version":"{VERSION}","files":{{"{name}":{{"sha256":"zz","size":1}}}}}}"#),
            "sha256",
        ),
        (
            format!(r#"{{"version":"{VERSION}","files":{{"{name}":{{"sha256":"{digest}"}}}}}}"#),
            "size",
        ),
    ];
    for (pointer, expect) in cases {
        let mut repo = Repo::good();
        repo.serve(POINTER_URL, &pointer);
        let error = locate(&repo, &settings).expect_err("refused");
        assert!(matches!(error, PackageError::Pointer { .. }), "{error}");
        assert!(error.to_string().contains(expect), "{error}");
        assert!(error.to_string().contains(POINTER_URL));
    }
}

/// A body that fails to read halfway is an error naming the URL, and a staging path that cannot
/// be created is an error naming the path.
#[test]
fn read_and_staging_failures_name_what_failed() {
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("the link dropped"))
        }
    }
    struct Drops;
    impl hostkeeper_package::net::Transport for Drops {
        fn open(
            &self,
            _: &str,
            _: hostkeeper_package::net::Timeouts,
        ) -> Result<hostkeeper_package::net::Response, hostkeeper_package::ports::NetError>
        {
            Ok(hostkeeper_package::net::Response {
                status: 200,
                content_length: None,
                location: None,
                www_authenticate: None,
                body: Box::new(Broken),
            })
        }
    }
    let error = locate(&Drops, &settings("")).expect_err("dropped");
    assert!(matches!(error, PackageError::Read { .. }), "{error}");
    assert!(error.to_string().contains("the link dropped"));
    let dir = scratch("package-staging");
    let repo = Repo::good();
    let settings = settings("");
    let located = locate(&repo, &settings).expect("located");
    let error = download(
        &repo,
        &settings,
        &located,
        &dir.path().join("missing/dir/a.pkg"),
    )
    .expect_err("no dir");
    assert!(matches!(error, PackageError::Staging { .. }), "{error}");
    let error =
        download(&Drops, &settings, &located, &dir.path().join("b.pkg")).expect_err("dropped");
    assert!(matches!(error, PackageError::Read { .. }), "{error}");
}

/// A sink that fails (a full disk) ends the fetch with an error naming the URL, and the bytes it
/// did take are counted.
#[test]
fn a_failing_sink_ends_the_fetch() {
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("no space left"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let timeouts = hostkeeper_package::net::Timeouts {
        connect: std::time::Duration::from_secs(1),
        total: std::time::Duration::from_secs(1),
    };
    let error = fetch(&Repo::good(), PKG_URL, timeouts, 1 << 20, &mut Full).expect_err("full");
    assert!(matches!(error, PackageError::Read { .. }), "{error}");
    assert!(error.to_string().contains("no space left"));
}
