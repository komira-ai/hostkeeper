//! Stand-ins for the tests of the `pkg` verbs: the files of a release of the installer, and the
//! S3 of the apt tests ([`crate::channels::apt::testing::FakeAws`]).

use std::path::PathBuf;

use super::{BUNDLE_SUFFIX, Request};
pub(crate) use crate::channels::apt::testing::{
    BUCKET, DENIED, FakeAws, NO_SUCH_KEY, PRECONDITION, etag, flag,
};
use crate::channels::spec::HOSTKEEPER;
use crate::channels::sums::{self, SUMS, Sha256};
use crate::testdir::TestDir;

pub(crate) const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

pub(crate) fn scratch(name: &str) -> TestDir {
    let beside = std::env::current_exe().expect("test binary");
    TestDir::under(&beside.parent().expect("deps").join("xtask-pkg-unit"), name)
}

/// `dir` as the `release-assets` artifact of the release `version`: the installer and its
/// bundle, and a `SHA256SUMS` that lists the installer and another file but not the bundle.
pub(crate) fn release_dir(dir: &std::path::Path, version: &str) {
    let installer = HOSTKEEPER.pkg_file(version);
    let bytes = format!("bytes of {installer}");
    std::fs::write(dir.join(&installer), &bytes).expect("installer");
    std::fs::write(
        dir.join(format!("{installer}{BUNDLE_SUFFIX}")),
        format!("bundle of {installer}"),
    )
    .expect("bundle");
    let lines = [
        sums::Line {
            sha256: Sha256::of(b"another file"),
            name: format!("hostkeeper-{version}-linux-x86_64.tar.gz"),
        },
        sums::Line {
            sha256: Sha256::of(bytes.as_bytes()),
            name: installer,
        },
    ];
    std::fs::write(dir.join(SUMS), sums::render(&lines)).expect("sums");
}

/// The names of the files of the version 1.2.3, sorted.
pub(crate) fn names() -> Vec<String> {
    let installer = HOSTKEEPER.pkg_file("1.2.3");
    vec![installer.clone(), format!("{installer}{BUNDLE_SUFFIX}")]
}

/// The release 1.2.3 in a directory of its own, and the work directory beside it.
pub(crate) struct Fixture {
    _scratch: TestDir,
    pub(crate) release: PathBuf,
    pub(crate) work: PathBuf,
}

impl Fixture {
    pub(crate) fn new(name: &str) -> Fixture {
        let scratch = scratch(name);
        let release = scratch.join("release");
        std::fs::create_dir(&release).expect("release");
        release_dir(&release, "1.2.3");
        let work = scratch.join("work");
        Fixture {
            _scratch: scratch,
            release,
            work,
        }
    }

    pub(crate) fn request<'a>(&'a self, distribution_id: &'a str) -> Request<'a> {
        Request {
            dir: &self.release,
            tag: "v1.2.3",
            commit: COMMIT,
            bucket: BUCKET,
            distribution_id,
            work: &self.work,
            attempts: 3,
        }
    }
}
