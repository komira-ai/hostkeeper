use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::channels::aws::Answer;
use crate::channels::pkg::testing::scratch;

const URL: &str = "https://packages.komira.ai";
const BASE: &str = "https://packages.komira.ai/pkg/hostkeeper/1.2.3/";
const INSTALLER: &str = "hostkeeper-1.2.3-macos-arm64.pkg";
const BUNDLE: &str = "hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json";

/// A `curl` that serves files from memory, by URL, and records the command lines.
#[derive(Default)]
struct FakeCurl {
    served: RefCell<BTreeMap<String, Vec<u8>>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeCurl {
    fn serve(&self, name: &str, bytes: &[u8]) {
        self.served
            .borrow_mut()
            .insert(format!("{BASE}{name}"), bytes.to_vec());
    }

    /// The host as the upload left it for the release whose installer is `installer`.
    fn published() -> FakeCurl {
        let curl = FakeCurl::default();
        curl.serve(INSTALLER, b"installer bytes");
        curl.serve(BUNDLE, b"bundle bytes");
        curl.serve(
            SUMS,
            directory_sums(b"installer bytes", b"bundle bytes").as_bytes(),
        );
        curl
    }
}

fn directory_sums(installer: &[u8], bundle: &[u8]) -> String {
    sums::render(&[
        sums::Line {
            sha256: Sha256::of(bundle),
            name: BUNDLE.to_owned(),
        },
        sums::Line {
            sha256: Sha256::of(installer),
            name: INSTALLER.to_owned(),
        },
    ])
}

impl Host for FakeCurl {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        assert_eq!(program, Path::new("curl"));
        self.calls.borrow_mut().push(args.to_vec());
        let url = args.last().expect("a url");
        let at = args.iter().position(|a| a == "--output").expect("--output");
        match self.served.borrow().get(url) {
            Some(bytes) => {
                std::fs::write(&args[at + 1], bytes).expect("write");
                Ok(Answer {
                    ok: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            None => Ok(Answer {
                ok: false,
                stdout: String::new(),
                stderr: "curl: (22) The requested URL returned error: 404\n".to_owned(),
            }),
        }
    }
}

struct Fixture {
    _scratch: crate::testdir::TestDir,
    sums: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let scratch = scratch(name);
        let sums_path = scratch.join(SUMS);
        let release = sums::render(&[sums::Line {
            sha256: Sha256::of(b"installer bytes"),
            name: INSTALLER.to_owned(),
        }]);
        std::fs::write(&sums_path, release).expect("sums");
        let work = scratch.join("work");
        Fixture {
            _scratch: scratch,
            sums: sums_path,
            work,
        }
    }

    fn request(&self) -> Request<'_> {
        Request {
            sums: &self.sums,
            version: "1.2.3",
            url: URL,
            work: &self.work,
            curl: Path::new("curl"),
        }
    }
}

#[test]
fn the_installer_the_host_serves_is_the_one_the_release_lists() {
    let f = Fixture::new("ok");
    let curl = FakeCurl::published();
    let said = check(&curl, &f.request()).expect("checked");
    assert_eq!(said.len(), 1);
    assert!(
        said[0].starts_with(&format!(
            "check-pkg: {INSTALLER} from {BASE} is {}, as the release's SHA256SUMS lists;",
            Sha256::of(b"installer bytes")
        )),
        "{said:?}"
    );
    assert!(
        said[0].ends_with("these are the release's signed bytes"),
        "{said:?}"
    );
    // Three downloads, over HTTPS only, each to the work directory.
    let calls = curl.calls.borrow();
    assert_eq!(calls.len(), 3);
    for call in calls.iter() {
        assert_eq!(
            call[..7],
            [
                "--proto",
                "=https",
                "--tlsv1.2",
                "--fail",
                "--silent",
                "--show-error",
                "--output"
            ]
        );
    }
    assert_eq!(calls[0][8], format!("{BASE}{INSTALLER}"));
    assert_eq!(calls[1][8], format!("{BASE}{BUNDLE}"));
    assert_eq!(calls[2][8], format!("{BASE}SHA256SUMS"));
}

#[test]
fn an_installer_that_is_not_the_one_the_release_built_is_named() {
    let f = Fixture::new("changed");
    let curl = FakeCurl::published();
    curl.serve(INSTALLER, b"another installer");
    let Error::Refused(problems) = check(&curl, &f.request()).unwrap_err() else {
        panic!("refused")
    };
    let served = Sha256::of(b"another installer");
    assert_eq!(
        problems,
        [
            format!(
                "{BASE}{INSTALLER} is {served}, but the release's SHA256SUMS lists {}",
                Sha256::of(b"installer bytes")
            ),
            format!(
                "{BASE}SHA256SUMS lists {INSTALLER} as {}, but it is {served}",
                Sha256::of(b"installer bytes")
            ),
        ]
    );
}

#[test]
fn a_bundle_the_directory_sums_do_not_vouch_for_is_named() {
    // The bundle differs from the one the directory's sums list, and the installer is not listed.
    let f = Fixture::new("bundle");
    let curl = FakeCurl::published();
    curl.serve(BUNDLE, b"another bundle");
    let Error::Refused(problems) = check(&curl, &f.request()).unwrap_err() else {
        panic!("refused")
    };
    assert_eq!(
        problems,
        [format!(
            "{BASE}SHA256SUMS lists {BUNDLE} as {}, but it is {}",
            Sha256::of(b"bundle bytes"),
            Sha256::of(b"another bundle")
        )]
    );

    let g = Fixture::new("unlisted");
    let curl = FakeCurl::published();
    curl.serve(SUMS, format!("{}  other\n", Sha256::of(b"x")).as_bytes());
    let Error::Refused(problems) = check(&curl, &g.request()).unwrap_err() else {
        panic!("refused")
    };
    assert_eq!(
        problems,
        [
            format!("{BASE}SHA256SUMS does not list {INSTALLER}"),
            format!("{BASE}SHA256SUMS does not list {BUNDLE}"),
        ]
    );
}

#[test]
fn a_directory_sums_file_that_is_malformed_is_refused() {
    let f = Fixture::new("malformed");
    let curl = FakeCurl::published();
    curl.serve(SUMS, b"not a sums file");
    let Error::Refused(problems) = check(&curl, &f.request()).unwrap_err() else {
        panic!("refused")
    };
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].starts_with(&format!("{BASE}SHA256SUMS: line 1: ")),
        "{problems:?}"
    );
}

#[test]
fn a_file_the_host_does_not_serve_fails_with_what_curl_said() {
    for missing in [INSTALLER, BUNDLE, SUMS] {
        let f = Fixture::new("missing");
        let curl = FakeCurl::published();
        curl.served.borrow_mut().remove(&format!("{BASE}{missing}"));
        let e = check(&curl, &f.request()).unwrap_err();
        assert_eq!(
            e,
            Error::Failed(format!(
                "{BASE}{missing} could not be downloaded: curl: (22) The requested URL returned error: 404"
            ))
        );
    }
}

#[test]
fn a_release_sums_file_without_the_installer_fails_before_anything_is_downloaded() {
    let f = Fixture::new("not-listed");
    std::fs::write(&f.sums, format!("{}  other\n", Sha256::of(b"x"))).expect("sums");
    let curl = FakeCurl::published();
    let e = check(&curl, &f.request()).unwrap_err().to_string();
    assert!(
        e.ends_with(&format!("SHA256SUMS does not list {INSTALLER}")),
        "{e}"
    );
    assert!(curl.calls.borrow().is_empty());
}

#[test]
fn flags_out_of_their_form_are_usage_errors_before_anything_is_downloaded() {
    let f = Fixture::new("usage");
    let curl = FakeCurl::published();
    let mut request = f.request();
    request.version = "v1.2.3";
    assert!(matches!(check(&curl, &request), Err(Error::Usage(_))));
    let mut request = f.request();
    request.url = "http://packages.komira.ai";
    assert!(matches!(check(&curl, &request), Err(Error::Usage(_))));
    assert!(curl.calls.borrow().is_empty());
}
