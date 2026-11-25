//! `hk xcode-push` (the second name of `hk xcode add`, whose own cases are in `xcode_add`) against stand-ins for the Mac's programs (`xcodes`, `pkgutil`, `xip`,
//! `plutil`: the test binary `hk-fake-mac-tool` under each name, which records its arguments and
//! environment) and a fake Xcode store on a loopback port of 0 that refuses a body whose SHA-256
//! is not its path's.

use std::path::Path;

use hostkeeper_hk::xcode_push::Mac;
use sha2::{Digest as _, Sha256};

use crate::rig::{Early, Opts, Ran, Reply, Rig, Server, TOKEN, args, call, closed_port};

/// The bytes of the archive of the tests.
const ARCHIVE: &[u8] = b"xar!, the bytes of an Xcode archive, as Apple ships them";

/// What `pkgutil --check-signature` says of an archive Apple signed.
const SIGNED_BY_APPLE: &str = "\
Package \"Xcode_27.2.xip\":
   Status: signed Apple Software
   Notarization: trusted by the Apple notary service
   Signed with a trusted timestamp on: 2026-11-18 17:00:00 +0000
   Certificate Chain:
    1. Software Signing
       Expires: 2030-10-24 17:39:41 +0000
       SHA256 Fingerprint:
           D8 4D B9 6A F8 C2 E6 0A C4 C8 51 A2 1E C4 60 F6 F8 4E 02 35 BE B1
           7D 24 A7 87 12 B9 B0 21 ED 57
       ------------------------------------------------------------------------
    2. Apple Code Signing Certification Authority
       Expires: 2026-10-24 17:39:41 +0000
       SHA256 Fingerprint:
           5B DA B1 28 8F C1 68 92 FE F5 0C 65 8D B5 4F 1E 2E 19 CF 8F 71 CC
           55 F7 7D E2 B9 5E 05 1E 25 62
       ------------------------------------------------------------------------
    3. Apple Root CA
       Expires: 2035-02-09 21:40:36 +0000
       SHA256 Fingerprint:
           B0 B1 73 0E CB C7 FF 45 05 14 2C 49 F1 29 5E 6E DA 6B CA ED 7E 2C
           68 C5 BE 91 B5 A1 10 01 F0 24
";

/// The path `Xcode.List` is called on.
const LIST: &str = "/hostkeeper.operator.v1.Xcode/List";

/// The requests of `server` that are uploads.
fn puts(server: &Server) -> Vec<crate::rig::Seen> {
    server
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "PUT")
        .collect()
}

/// A store that answers `Xcode.List` an empty store and every upload with `status` and `body`.
fn refusing_store(rig: &Rig, status: u16, body: &str) -> Server {
    let body = body.to_owned();
    Server::serve(
        &rig.pki,
        || (),
        move |(), request| {
            if request.target == LIST {
                Reply::new(200, EMPTY_STORE)
            } else if request.target.ends_with("/upload") {
                Reply::new(200, NOTHING_HELD)
            } else {
                Reply::new(status, body.clone())
            }
        },
    )
}

/// The lower-case hex SHA-256 of `bytes`.
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the query of an upload answers: the store holds nothing of these bytes.
const NOTHING_HELD: &str = "{\"state\":\"none\",\"received\":0,\"size\":0}";

/// What `Xcode.List` answers: a store with nothing in it.
const EMPTY_STORE: &str = "{\"configured\":true}";

/// A fake Xcode store: `Xcode.List` is answered an empty store; a `PUT /v1/xcode/<sha256>?...`
/// whose body has that SHA-256 is stored (201 and the JSON of the archive); one whose body does
/// not is refused 400 `digest_mismatch`.
fn store(rig: &Rig) -> Server {
    Server::serve(
        &rig.pki,
        || (),
        |(), request| {
            if request.target == LIST {
                return Reply::new(200, EMPTY_STORE);
            }
            if request.target.ends_with("/upload") {
                return Reply::new(200, NOTHING_HELD);
            }
            let named = request
                .target
                .strip_prefix("/v1/xcode/")
                .and_then(|rest| rest.split('?').next())
                .unwrap_or_default();
            if request.method != "PUT" || named != sha256(&request.body) {
                return Reply::new(400, "{\"error\":\"digest_mismatch\"}");
            }
            let answer = format!(
                "{{\"sha256\":\"{named}\",\"version\":\"27.2\",\"build\":\"27B52\",\"size\":{}}}",
                request.body.len()
            );
            Reply::new(201, answer)
        },
    )
}

/// A test's directory with the stand-ins in `bin`, set up for an archive Apple signed whose app
/// is Xcode 27.2 (27B52).
struct Push {
    rig: Rig,
    mac: Mac,
}

impl Push {
    fn new(name: &str) -> Push {
        let rig = Rig::new(&format!("xcode-push-{name}"));
        let bin = rig.path("bin");
        std::fs::create_dir(&bin).expect("the bin directory");
        let fake = Path::new(env!("CARGO_BIN_EXE_hk-fake-mac-tool"));
        for tool in ["xcodes", "pkgutil", "xip", "plutil", "codesign", "df"] {
            // A link, not a copy: a copy is a file open for writing, which a process another
            // test forks meanwhile could hold when this one runs it.
            std::fs::hard_link(fake, bin.join(tool)).expect("a stand-in");
        }
        let mac = Mac {
            os: "macos",
            xcodes: bin.join("xcodes"),
            pkgutil: bin.join("pkgutil"),
            xip: bin.join("xip"),
            plutil: bin.join("plutil"),
            codesign: bin.join("codesign"),
            df: bin.join("df"),
            clock: crate::verify_rig::clock,
            ..Mac::system()
        };
        let push = Push { rig, mac };
        push.tool("df.out", crate::verify_rig::df(900_000_000).as_bytes());
        push.tool("pkgutil.out", SIGNED_BY_APPLE.as_bytes());
        push.tool("xip.version", b"27.2\n");
        push.tool("xip.product-build", b"27B52\n");
        push
    }

    /// Writes `name` in `bin`, beside the stand-ins.
    fn tool(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.rig.path("bin").join(name), bytes).expect("written");
    }

    /// Removes `name` from `bin`.
    fn untool(&self, name: &str) {
        std::fs::remove_file(self.rig.path("bin").join(name)).expect("removed");
    }

    /// What the stand-in wrote to `name` in `bin`; empty when it wrote nothing.
    fn record(&self, name: &str) -> String {
        std::fs::read_to_string(self.rig.path("bin").join(name)).unwrap_or_default()
    }

    /// The archive of the tests, in the test's directory: its path.
    fn archive(&self) -> String {
        std::fs::create_dir_all(self.rig.path("downloads")).expect("a downloads directory");
        self.rig.put("downloads/Xcode_27.2.xip", ARCHIVE, 0o644)
    }

    /// `hk` with the global flags of a call on `port`, then `words`, on the stand-ins.
    fn run(&self, port: u16, words: &[&str]) -> Ran {
        self.run_with(Opts::real(), port, words)
    }

    /// [`Push::run`] with `opts`.
    fn run_with<'a>(&'a self, opts: Opts<'a>, port: u16, words: &[&str]) -> Ran {
        self.rig
            .run(opts.mac(&self.mac), &call(&self.rig, port, words))
    }

    /// The entries of the archive's directory: what an expansion left behind.
    fn beside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.rig.path("downloads"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// What the second name says on stderr before anything else.
const NOTICE: &str = "hk xcode-push is now hk xcode add\n";

/// The last line `ran` wrote to stderr.
fn last_line(ran: &Ran) -> &str {
    ran.stderr.lines().last().unwrap_or_default()
}

/// Proves: when Apple's signature is not on the archive, nothing is uploaded (the store saw no
/// request, so zero bytes), the push exits 1 saying why, and the archive is never expanded: when
/// pkgutil exits non-zero; when it exits 0 with the status of a developer's signature rather
/// than Apple's own; when the chain ends at a root named Apple Root CA whose fingerprint is not
/// Apple's; when it ends at another root; and when the report has no status or no chain.
#[test]
fn a_signature_check_that_fails_uploads_nothing() {
    let developer = SIGNED_BY_APPLE.replace(
        "signed Apple Software",
        "signed by a developer certificate issued by Apple for distribution",
    );
    let wrong_root = SIGNED_BY_APPLE.replace("68 C5 BE 91", "68 C5 BE 92");
    let other_root = SIGNED_BY_APPLE.replace("3. Apple Root CA", "3. Some Root CA");
    let no_status = SIGNED_BY_APPLE.replace("Status: signed Apple Software", "");
    let no_chain: String = SIGNED_BY_APPLE
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    let cases: [(&str, &str, &str); 6] = [
        ("1", "", "failed (exit status: 1); nothing was uploaded"),
        (
            "0",
            &developer,
            "does not report Apple's own signature: its status is \"signed by a developer \
             certificate issued by Apple for distribution\", not \"signed Apple Software\"; \
             nothing was uploaded",
        ),
        (
            "0",
            &wrong_root,
            "does not report Apple's own signature: its chain ends at \"Apple Root CA\" with the \
             SHA-256 fingerprint \"B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE92B5A11001F024\", \
             not at Apple Root CA (B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024); \
             nothing was uploaded",
        ),
        (
            "0",
            &other_root,
            "does not report Apple's own signature: its chain ends at \"Some Root CA\" with the \
             SHA-256 fingerprint \"B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024\", \
             not at Apple Root CA (B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024); \
             nothing was uploaded",
        ),
        (
            "0",
            &no_status,
            "does not report Apple's own signature: it gives no status; nothing was uploaded",
        ),
        (
            "0",
            &no_chain,
            "does not report Apple's own signature: it gives no certificate chain; nothing was \
             uploaded",
        ),
    ];
    for (n, (exit, report, says)) in cases.into_iter().enumerate() {
        let push = Push::new(&format!("unsigned-{n}"));
        push.tool("pkgutil.exit", exit.as_bytes());
        push.tool("pkgutil.out", report.as_bytes());
        let archive = push.archive();
        let server = store(&push.rig);
        let ran = push.run(server.port, &["xcode-push", &archive]);
        assert_eq!(ran.code, 1, "{n}: {ran:?}");
        assert!(last_line(&ran).ends_with(says), "{n}: {ran:?}");
        assert!(puts(&server).is_empty(), "{n}: the store saw a request");
        assert_eq!(
            push.record("pkgutil.args"),
            format!("--check-signature\t{archive}\n"),
            "{n}"
        );
        assert_eq!(push.record("xip.args"), "", "{n}: the archive was expanded");
        assert_eq!(ran.stdout, "", "{n}");
    }
}

/// Proves: an archive Apple signed is uploaded as exactly its bytes, by one `PUT` to
/// `/v1/xcode/<the SHA-256 of those bytes>?version=27.2&build=27B52` (the version and the build
/// the expanded app's property lists give), as `application/octet-stream`, with
/// `Expect: 100-continue` and the operator's token as the bearer; the store's answer is printed
/// and the push exits 0; the expansion beside the archive, and what an earlier push left there,
/// is gone afterwards.
#[test]
fn a_good_archive_uploads_exactly_its_bytes_under_their_sha256() {
    let push = Push::new("good");
    let archive = push.archive();
    // What a push stopped by a signal left beside the archive: removed, then used.
    let stale = push.rig.path("downloads/.hk-xcode-push-expansion");
    std::fs::create_dir(&stale).expect("a stale expansion");
    std::fs::write(stale.join("left-over"), b"left").expect("a stale file");
    let server = store(&push.rig);
    let ran = push.run(server.port, &["xcode-push", &archive]);
    assert_eq!(ran.code, 0, "{ran:?}");
    let seen = puts(&server);
    assert_eq!(seen.len(), 1, "{seen:#?}");
    let put = &seen[0];
    let sha = sha256(ARCHIVE);
    assert_eq!(put.method, "PUT");
    assert_eq!(
        put.target,
        format!("/v1/xcode/{sha}?version=27.2&build=27B52")
    );
    assert_eq!(put.body, ARCHIVE);
    assert_eq!(sha256(&put.body), sha);
    assert_eq!(
        put.header("content-length"),
        Some(ARCHIVE.len().to_string().as_str())
    );
    assert_eq!(put.header("content-type"), Some("application/octet-stream"));
    assert_eq!(put.header("expect"), Some("100-continue"));
    assert_eq!(
        put.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    // `hk xcode add` answers `stored: ...` on stdout, not the store's JSON.
    assert_eq!(
        ran.stdout,
        format!(
            "stored: Xcode 27.2 (27B52), sha256 {sha}, {} bytes\n",
            ARCHIVE.len()
        )
    );
    assert!(
        ran.stderr.contains(&format!(
            "hk: sha256 {sha}, 56 B; the store holds no archive of these bytes\n"
        )),
        "{ran:?}"
    );
    let absolute = std::path::absolute(&archive).expect("absolute");
    assert_eq!(
        push.record("xip.args"),
        format!("--expand\t{}\n", absolute.display())
    );
    let plutil = push.record("plutil.args");
    let lines: Vec<&str> = plutil.lines().collect();
    assert_eq!(lines.len(), 2, "{plutil}");
    assert!(lines[0].starts_with("-extract\tCFBundleShortVersionString\traw\t-o\t-\t"));
    assert!(lines[0].ends_with("/Xcode.app/Contents/Info.plist"));
    assert!(lines[1].starts_with("-extract\tProductBuildVersion\traw\t-o\t-\t"));
    assert!(lines[1].ends_with("/Xcode.app/Contents/version.plist"));
    assert_eq!(push.beside(), ["Xcode_27.2.xip"]);
    assert_eq!(push.record("xcodes.args"), "", "a file is not downloaded");
}

/// Proves: given a version, the push runs `xcodes download` with that version and only it, into
/// `$HOME/Downloads/hk-xcode-push/<version>`, and uploads the archive xcodes put there.
#[test]
fn given_a_version_xcodes_downloads_that_version_only() {
    let push = Push::new("version");
    push.tool("xcodes.xip", ARCHIVE);
    let server = store(&push.rig);
    let ran = push.run(server.port, &["xcode-push", "27.2"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    let into = push.rig.path("home/Downloads/hk-xcode-push/27.2");
    assert_eq!(
        push.record("xcodes.args"),
        format!("download\t27.2\t--directory\t{}\n", into.display())
    );
    let seen = puts(&server);
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].body, ARCHIVE);
    assert_eq!(
        push.record("pkgutil.args"),
        format!(
            "--check-signature\t{}\n",
            into.join("Xcode-27.2.xip").display()
        )
    );
}

/// Proves: the operator's token is in no argument and no environment variable of any program the
/// push runs (`xcodes`, `pkgutil`, `xip`, `plutil`), and in no line `hk` prints; it is only the
/// bearer of the upload.
#[test]
fn the_token_is_in_no_argument_environment_or_output_line() {
    let push = Push::new("token");
    push.tool("xcodes.xip", ARCHIVE);
    let server = store(&push.rig);
    let ran = push.run(server.port, &["xcode-push", "27.2"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    for tool in ["xcodes", "pkgutil", "xip", "plutil"] {
        let argv = push.record(&format!("{tool}.args"));
        let env = push.record(&format!("{tool}.env"));
        assert!(!argv.is_empty() && !env.is_empty(), "{tool} did not run");
        assert!(!argv.contains(TOKEN), "{tool}'s arguments hold the token");
        assert!(!env.contains(TOKEN), "{tool}'s environment holds the token");
    }
    assert!(!ran.stdout.contains(TOKEN));
    assert!(!ran.stderr.contains(TOKEN));
    assert_eq!(
        puts(&server)[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

/// Proves: the store's refusal `digest_mismatch` (a body whose SHA-256 is not the path's) ends the
/// push with exit 2, a line in words that the archive changed after it was hashed and what to do,
/// and `refused: digest_mismatch`; and bytes added to the archive after it was hashed (here, by
/// the expansion) are not sent: the body is exactly the bytes that were hashed.
#[test]
fn a_body_whose_sha256_differs_from_its_path_is_refused() {
    let push = Push::new("mismatch");
    let archive = push.archive();
    let server = refusing_store(&push.rig, 400, "{\"error\":\"digest_mismatch\"}");
    let ran = push.run(server.port, &["xcode-push", &archive]);
    assert_eq!(ran.code, 2, "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "run hk xcode add again\nhk: the coordinator refused the upload (HTTP 400)\nrefused: digest_mismatch\n"
        ),
        "{ran:?}"
    );
    assert!(
        ran.stderr.contains(
            "hk: the bytes sent are not the ones whose SHA-256 was asked to be stored: the archive \
             changed after it was hashed; "
        ),
        "{ran:?}"
    );
    assert_eq!(ran.stdout, "");

    let push = Push::new("grown");
    push.tool("xip.append", b", and more");
    let archive = push.archive();
    let server = store(&push.rig);
    let ran = push.run(server.port, &["xcode-push", &archive]);
    assert_eq!(ran.code, 0, "{ran:?}");
    let seen = puts(&server);
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].body, ARCHIVE);
}

/// Proves: a refusal the coordinator makes from the request's head alone, with no
/// `100 Continue`, is read before any byte of the archive is sent: the coordinator receives none
/// of its bytes, and the push exits 2 with `refused: not_listed`.
#[test]
fn a_refusal_before_the_body_is_read_before_any_byte_of_the_archive_is_sent() {
    let push = Push::new("early");
    let archive = push.archive();
    let server = Early::refusing_after(
        &push.rig.pki,
        2,
        EMPTY_STORE,
        409,
        "{\"error\":\"not_listed\"}",
    );
    let ran = push.run(server.port, &["xcode-push", &archive]);
    assert_eq!(ran.code, 2, "{ran:?}");
    assert!(
        ran.stderr
            .ends_with("hk: the coordinator refused the upload (HTTP 409)\nrefused: not_listed\n"),
        "{ran:?}"
    );
    let (target, bytes) = server.heard();
    assert_eq!(
        target,
        format!("/v1/xcode/{}?version=27.2&build=27B52", sha256(ARCHIVE))
    );
    assert_eq!(bytes, 0, "bytes of the archive reached the coordinator");
}

/// Proves: off macOS the push refuses at once, exit 1, with a message that says it runs on the
/// Mac signed in to the Apple developer account; no program runs and nothing is uploaded. Its
/// help is still printed.
#[test]
fn off_macos_it_refuses_with_a_clear_message() {
    let mut push = Push::new("linux");
    push.mac.os = "linux";
    let archive = push.archive();
    let server = store(&push.rig);
    let ran = push.run(server.port, &["xcode-push", &archive]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            1,
            "",
            "hk xcode-push is now hk xcode add\nhk: hk xcode-push runs on macOS only, on the Mac signed in to the Apple developer \
             account: it runs xcodes, pkgutil and xip, which are macOS programs, and this system \
             is linux\n"
        )
    );
    assert!(puts(&server).is_empty());
    for tool in ["xcodes", "pkgutil", "xip", "plutil"] {
        assert_eq!(push.record(&format!("{tool}.args")), "", "{tool} ran");
    }
    let help = push.run(server.port, &["xcode-push", "--help"]);
    assert_eq!(help.code, 0);
    assert!(
        help.stdout
            .starts_with("hk xcode-push <version> | <archive.xip>\n")
    );
}

/// Proves: a download that does not end in one archive uploads nothing, exit 1: xcodes exiting
/// non-zero, xcodes that cannot be run, xcodes leaving no `.xip`, a directory holding two, and
/// no `$HOME` (unset or empty) or no `$HOME/Downloads` directory to download into.
#[test]
fn a_download_that_does_not_give_one_archive_uploads_nothing() {
    let into = "home/Downloads/hk-xcode-push/27.2";
    let cases: [(&str, &str); 7] = [
        (
            "downloads-file",
            "(Not a directory (os error 20)); nothing was uploaded",
        ),
        (
            "failed",
            "download 27.2 failed (exit status: 3); nothing was uploaded",
        ),
        (
            "missing",
            "is not installed, so Xcode 27.2 cannot be downloaded here; either install it \
             (brew install xcodesorg/made/xcodes), or download the archive in a browser from \
             Apple's developer downloads page (https://developer.apple.com/download/all/) into \
             ~/Downloads and run hk xcode add 27.2 again; nothing was uploaded",
        ),
        (
            "none",
            "home/Downloads/hk-xcode-push/27.2; nothing was uploaded",
        ),
        (
            "other-version-only",
            "home/Downloads/hk-xcode-push/27.2; nothing was uploaded",
        ),
        (
            "homeless",
            "hk xcode-push keeps its records and its lock under $HOME, which is not set",
        ),
        (
            "empty-home",
            "hk xcode-push keeps its records and its lock under $HOME, which is not set",
        ),
    ];
    for (case, says) in cases {
        let mut push = Push::new(&format!("download-{case}"));
        let mut opts = Opts::real();
        match case {
            "failed" => {
                push.tool("xcodes.xip", ARCHIVE);
                push.tool("xcodes.exit", b"3");
            }
            "missing" => push.mac.xcodes = push.rig.path("bin/no-such-xcodes"),
            "other-version-only" => {
                // xcodes leaves a file, but it is named for another version.
                std::fs::create_dir_all(push.rig.path(into)).expect("the directory");
                push.rig
                    .put(&format!("{into}/Xcode-27.1.xip"), b"older", 0o644);
            }
            "homeless" => opts = opts.without_home(),
            "empty-home" => opts = opts.home(Path::new("")),
            "downloads-file" => {
                push.rig.put("home/Downloads", b"a file", 0o644);
            }
            _ => {}
        }
        let server = store(&push.rig);
        let ran = push.run_with(opts, server.port, &["xcode-push", "27.2"]);
        assert_eq!(ran.code, 1, "{case}: {ran:?}");
        assert!(last_line(&ran).ends_with(says), "{case}: {ran:?}");
        assert!(puts(&server).is_empty(), "{case}");
        assert_eq!(push.record("pkgutil.args"), "", "{case}: checked");
    }
}

/// Proves: an archive whose version and build cannot be read, or are not a release's, or whose
/// version is not the one asked for, uploads nothing, exit 1, and leaves no expansion behind:
/// xip exiting non-zero, an archive with no app or two, a version or a build that is not one,
/// plutil that cannot be run or that cannot read the build, and a version other than the one
/// downloaded.
#[test]
fn an_archive_whose_release_cannot_be_read_uploads_nothing() {
    let cases: [(&str, &str); 8] = [
        ("xip", "failed (exit status: 4); nothing was uploaded"),
        (
            "two-apps",
            " holds 2 apps, not one Xcode; nothing was uploaded",
        ),
        (
            "build-unread",
            "/Xcode.app/Contents/version.plist failed (exit status: 1); nothing was uploaded",
        ),
        (
            "version",
            "/Xcode.app says version \"27.2b\" and build \"27B52\", which are not an Xcode \
             release's; nothing was uploaded",
        ),
        (
            "no-app",
            " holds 0 apps, not one Xcode; nothing was uploaded",
        ),
        (
            "build",
            "/Xcode.app says version \"27.2\" and build \"27 B52\", which are not an Xcode \
             release's; nothing was uploaded",
        ),
        (
            "plutil",
            "(No such file or directory (os error 2)); nothing was uploaded",
        ),
        (
            "other",
            "/Xcode-27.2.xip is Xcode 27.1 (27B52); nothing was uploaded",
        ),
    ];
    for (case, says) in cases {
        let mut push = Push::new(&format!("release-{case}"));
        let mut words = ["xcode-push".to_owned(), push.archive()];
        match case {
            "xip" => push.tool("xip.exit", b"4"),
            "no-app" => push.untool("xip.version"),
            "version" => push.tool("xip.version", b"27.2b\n"),
            "build" => push.tool("xip.product-build", b"27 B52\n"),
            "plutil" => push.mac.plutil = push.rig.path("bin/no-such-plutil"),
            "other" => {
                push.tool("xcodes.xip", ARCHIVE);
                push.tool("xip.version", b"27.1\n");
                words[1] = "27.2".to_owned();
            }
            "two-apps" => push.tool("xip.second", b""),
            "build-unread" => push.tool("plutil.refuse", b"ProductBuildVersion"),
            _ => {}
        }
        let server = store(&push.rig);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let ran = push.run(server.port, &words);
        assert_eq!(ran.code, 1, "{case}: {ran:?}");
        assert!(last_line(&ran).ends_with(says), "{case}: {ran:?}");
        assert!(puts(&server).is_empty(), "{case}");
        let left: Vec<String> = push
            .beside()
            .into_iter()
            .filter(|name| name.starts_with(".hk-xcode-push-"))
            .collect();
        assert_eq!(left, Vec::<String>::new(), "{case}");
    }
}

/// Proves: a path that is not a file to read uploads nothing and runs nothing, exit 1: one that
/// does not exist, and a directory.
#[test]
fn a_path_that_is_not_an_archive_uploads_nothing() {
    let push = Push::new("path");
    let missing = push.rig.path("Xcode_none.xip").display().to_string();
    let directory = push.rig.path("home").display().to_string();
    let cases = [
        (
            missing.clone(),
            format!(
                "cannot read {missing} (No such file or directory (os error 2)); nothing was uploaded"
            ),
        ),
        (
            directory.clone(),
            format!("{directory} is not a file; nothing was uploaded"),
        ),
    ];
    let server = store(&push.rig);
    for (path, says) in cases {
        let ran = push.run(server.port, &["xcode-push", &path]);
        assert_eq!(
            (ran.code, last_line(&ran)),
            (1, format!("hk: {says}").as_str())
        );
    }
    assert!(puts(&server).is_empty());
    assert_eq!(push.record("pkgutil.args"), "");
}

/// Proves: the store's refusals end the push with `refused: <code>` and the exit code of their
/// status (3 for 401 and 403, 2 for another 4xx, 1 for a 5xx); a code that is not a reason token
/// prints no `refused:` line; an answer that is not the store's JSON is exit 1 saying so; and no
/// answer at all is exit 1 saying the coordinator keeps nothing of it. A second build of a version
/// the store holds (409 `xcode_version_taken`) ends the push the same way, naming its code.
#[test]
fn the_stores_refusals_end_the_push_by_their_status() {
    let cases: [(u16, &str, u8, &str); 7] = [
        (
            401,
            "{\"error\":\"unauthorized\"}",
            3,
            "refused: unauthorized",
        ),
        (403, "{\"error\":\"forbidden\"}", 3, "refused: forbidden"),
        (
            409,
            "{\"error\":\"not_listed\"}\n",
            2,
            "refused: not_listed",
        ),
        (
            409,
            "{\"error\":\"xcode_version_taken\"}",
            2,
            "refused: xcode_version_taken",
        ),
        (
            500,
            "{\"error\":\"store_failed\"}",
            1,
            "refused: store_failed",
        ),
        (
            409,
            "{\"error\":\"Not A Token\"}",
            2,
            "hk: the coordinator refused the upload (HTTP 409)",
        ),
        (
            502,
            "<html>bad gateway</html>",
            1,
            "hk: the answer (HTTP 502) is not the Xcode store's",
        ),
    ];
    let push = Push::new("refusals");
    let archive = push.archive();
    for (status, body, code, says) in cases {
        let server = refusing_store(&push.rig, status, body);
        let ran = push.run(server.port, &["xcode-push", &archive]);
        assert_eq!((ran.code, last_line(&ran)), (code, says), "{status} {body}");
        assert_eq!(ran.stdout, "");
        assert_eq!(puts(&server).len(), 1);
    }
    let hangs_up = Server::serve(
        &push.rig.pki,
        || (),
        |(), request| {
            if request.target == LIST {
                Reply::new(200, EMPTY_STORE)
            } else if request.target.ends_with("/upload") {
                // An older coordinator: the whole archive in one request, which it cuts.
                Reply::new(405, "{\"error\":\"method_not_allowed\"}")
            } else {
                Reply::hang_up()
            }
        },
    );
    let ran = push.run(hangs_up.port, &["xcode-push", &archive]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        last_line(&ran).ends_with("; the coordinator keeps nothing of an upload that does not end"),
        "{ran:?}"
    );
}

/// Proves: the usage errors of `hk xcode-push` exit 2 before anything runs: no archive or
/// version, two of them, a flag it does not have, a credential other than the token file, a
/// missing global flag, an endpoint that is not https, a CA file that cannot be read or holds no
/// certificate, a token file others can read, and an empty token.
#[test]
fn its_usage_errors_exit_2_before_anything_runs() {
    let push = Push::new("usage");
    push.tool("xcodes.xip", ARCHIVE);
    let archive = push.archive();
    let rig = &push.rig;
    let not_ca = rig.put("not-ca.pem", b"no certificate here\n", 0o644);
    let open_token = rig.put("open-token", b"a-token\n", 0o644);
    let empty_token = rig.put("empty-token", b"\n", 0o600);
    let ca = rig.path("ca.pem").display().to_string();
    let token = rig.path("token").display().to_string();
    let port = closed_port();
    let endpoint = format!("https://127.0.0.1:{port}");
    let flags = |ca: &str, token: &str| -> Vec<String> {
        args(&[
            "--endpoint",
            &endpoint,
            "--ca-file",
            ca,
            "--token-file",
            token,
        ])
    };
    let with = |mut all: Vec<String>, words: &[&str]| -> Vec<String> {
        all.extend(args(words));
        all
    };
    let cases: Vec<(Vec<String>, String)> = vec![
        (
            with(flags(&ca, &token), &["xcode-push"]),
            "hk xcode-push takes one Xcode version (27.2) or the path of an archive \
             (~/Downloads/Xcode_27.2.xip)"
                .to_owned(),
        ),
        (
            with(flags(&ca, &token), &["xcode-push", "27.2", &archive]),
            "hk xcode-push takes one Xcode version (27.2) or the path of an archive \
             (~/Downloads/Xcode_27.2.xip)"
                .to_owned(),
        ),
        (
            with(flags(&ca, &token), &["xcode-push", "--force", &archive]),
            "hk xcode-push has no flag --force".to_owned(),
        ),
        (
            with(
                flags(&ca, &token),
                &["xcode-push", "--token-stdin", &archive],
            ),
            "hk xcode-push takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it (stdin is the terminal xcodes asks a two-factor code \
             on)"
            .to_owned(),
        ),
        (
            with(
                flags(&ca, &token),
                &[
                    "xcode-push",
                    "--github-oidc-audience",
                    "hostkeeper",
                    &archive,
                ],
            ),
            "hk xcode-push takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it (stdin is the terminal xcodes asks a two-factor code \
             on)"
            .to_owned(),
        ),
        (
            with(
                flags(&ca, &token),
                &["xcode-push", "--response-file", "answer.json", &archive],
            ),
            "hk xcode-push takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it (stdin is the terminal xcodes asks a two-factor code \
             on)"
            .to_owned(),
        ),
        (
            args(&[
                "--endpoint",
                &endpoint,
                "--ca-file",
                &ca,
                "xcode-push",
                &archive,
            ]),
            "hk xcode-push needs --endpoint, --ca-file and --token-file".to_owned(),
        ),
        (
            args(&[
                "--endpoint",
                "http://127.0.0.1",
                "--ca-file",
                &ca,
                "--token-file",
                &token,
                "xcode-push",
                &archive,
            ]),
            "--endpoint takes https://<host>[:<port>]".to_owned(),
        ),
        (
            with(flags("/no/such/ca.pem", &token), &["xcode-push", "27.2"]),
            "cannot read --ca-file /no/such/ca.pem".to_owned(),
        ),
        (
            with(flags(&not_ca, &token), &["xcode-push", "27.2"]),
            format!("--ca-file {not_ca} holds no certificate"),
        ),
        (
            with(flags(&ca, &open_token), &["xcode-push", "27.2"]),
            format!("--token-file {open_token} may be read by its group or others; make it 0600"),
        ),
        (
            with(flags(&ca, &empty_token), &["xcode-push", "27.2"]),
            "the token is empty or holds a character that is not visible ASCII".to_owned(),
        ),
    ];
    for (words, says) in cases {
        let ran = rig.run(Opts::real().mac(&push.mac), &words);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (2, "", format!("{NOTICE}hk: {says}\n").as_str()),
            "{words:?}"
        );
    }
    for tool in ["xcodes", "pkgutil", "xip", "plutil"] {
        assert_eq!(push.record(&format!("{tool}.args")), "", "{tool} ran");
    }
    // The CA and the token are read before a download: nothing was made to download into.
    assert!(!rig.path("home/Downloads").exists());
}
