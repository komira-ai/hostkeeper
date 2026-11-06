//! The checks of the flags, and the roots file.

use crate::flags::{MAX_INDEX_BYTES, Release, SetDesiredFlags, is_service, is_version, validate};
use crate::flow::set_desired;
use crate::ports::Ports;
use crate::testing::{Fake, flags, scratch, stored};
use crate::token::read_roots;

const RELEASE_HELP: &str = "hostkeeper ci set-desired: give either --package-digest, or --release-index-file together with --release-index-digest\n";
const SERVICE_HELP: &str = "hostkeeper ci set-desired: --service must be 1 to 255 bytes of A-Z a-z 0-9 . _ - and must not start with - or .\n";

/// Catches a grammar that lets a name through that the coordinator would refuse, or refuses one
/// it takes.
#[test]
fn names_and_versions_follow_their_grammar() {
    assert!(is_service("svc"));
    assert!(is_service("a.b_c-d"));
    assert!(is_service(&"a".repeat(255)));
    assert!(!is_service(&"a".repeat(256)));
    assert!(!is_service(""));
    assert!(!is_service("-a"));
    assert!(!is_service(".a"));
    assert!(!is_service("a b"));
    assert!(!is_service("a/b"));
    assert!(!is_service("1.2.3+build"));
    assert!(is_version("1.2.3+build.5"));
    assert!(!is_version("1.2.3\n"));
}

type Tweak = fn(&mut SetDesiredFlags);

fn case(name: &'static str, tweak: Tweak, message: &str) -> (&'static str, Tweak, String) {
    (name, tweak, message.to_owned())
}

/// Catches a bad flag that is accepted, or that asks for a token before it is refused.
#[test]
fn invalid_flags_are_refused_before_any_token() {
    let cases = vec![
        case("service", |f| f.service = "-x".to_owned(), SERVICE_HELP),
        case(
            "version",
            |f| f.service_version = String::new(),
            "hostkeeper ci set-desired: --version must be 1 to 255 bytes of A-Z a-z 0-9 . _ - + and must not start with - or .\n",
        ),
        case(
            "digest-case",
            |f| f.package_digest = Some("A".repeat(64)),
            "hostkeeper ci set-desired: --package-digest must be 64 lower-case hexadecimal characters\n",
        ),
        case(
            "digest-length",
            |f| f.package_digest = Some("a".repeat(63)),
            "hostkeeper ci set-desired: --package-digest must be 64 lower-case hexadecimal characters\n",
        ),
        case(
            "index-digest-case",
            |f| {
                f.package_digest = None;
                f.release_index_file = Some(f.work_dir.join("index.json"));
                f.release_index_digest = Some("B".repeat(64));
            },
            "hostkeeper ci set-desired: --release-index-digest must be 64 lower-case hexadecimal characters\n",
        ),
        case(
            "index-digest-length",
            |f| {
                f.package_digest = None;
                f.release_index_file = Some(f.work_dir.join("index.json"));
                f.release_index_digest = Some("b".repeat(65));
            },
            "hostkeeper ci set-desired: --release-index-digest must be 64 lower-case hexadecimal characters\n",
        ),
        case("neither-release", |f| f.package_digest = None, RELEASE_HELP),
        case(
            "empty-is-absent",
            |f| {
                f.package_digest = Some(String::new());
                f.release_index_file = Some(std::path::PathBuf::new());
                f.release_index_digest = Some(String::new());
            },
            RELEASE_HELP,
        ),
        case(
            "both-releases",
            |f| {
                f.release_index_file = Some(f.work_dir.join("index.json"));
                f.release_index_digest = Some("b".repeat(64));
            },
            RELEASE_HELP,
        ),
        case(
            "package-with-index-digest",
            |f| f.release_index_digest = Some("b".repeat(64)),
            RELEASE_HELP,
        ),
        case(
            "package-with-index-file",
            |f| f.release_index_file = Some(f.work_dir.join("index.json")),
            RELEASE_HELP,
        ),
        case(
            "index-file-alone",
            |f| {
                f.package_digest = None;
                f.release_index_file = Some(f.work_dir.join("index.json"));
            },
            RELEASE_HELP,
        ),
        case(
            "index-digest-alone",
            |f| {
                f.package_digest = None;
                f.release_index_digest = Some("b".repeat(64));
            },
            RELEASE_HELP,
        ),
        case(
            "endpoint",
            |f| f.endpoint = "http://coordinator.example".to_owned(),
            "hostkeeper ci set-desired: --endpoint must start with https://\n",
        ),
        case(
            "polls-zero",
            |f| f.max_polls = 0,
            "hostkeeper ci set-desired: --max-polls must be from 1 to 1000\n",
        ),
        case(
            "polls-many",
            |f| f.max_polls = 1001,
            "hostkeeper ci set-desired: --max-polls must be from 1 to 1000\n",
        ),
        case(
            "audience-empty",
            |f| f.audience = String::new(),
            "hostkeeper ci set-desired: --audience must not be empty or hold a control character\n",
        ),
        case(
            "audience-control",
            |f| f.audience = "a\nb".to_owned(),
            "hostkeeper ci set-desired: --audience must not be empty or hold a control character\n",
        ),
    ];
    for (name, tweak, message) in cases {
        let dir = scratch(name);
        let mut flags = flags(dir.path());
        tweak(&mut flags);
        let mut fake = Fake::new(dir.path(), stored("1.2.3"), vec![]);
        let mut stderr = Vec::new();
        let code = set_desired(&flags, &mut stderr, |_| &mut fake);
        assert_eq!(code, 2, "{name}");
        assert_eq!(String::from_utf8(stderr).expect("UTF-8"), message, "{name}");
        assert!(
            fake.log.lock().expect("lock").audiences.is_empty(),
            "{name}"
        );
        assert!(fake.log.lock().expect("lock").events.is_empty(), "{name}");
        assert!(!dir.path().join("github-output").exists(), "{name}");
    }
}

/// Catches a work directory that is missing or a file being accepted, and roots that hold no
/// certificate being accepted.
#[test]
fn the_work_dir_and_the_web_roots_must_be_usable() {
    let dir = scratch("paths");
    let mut missing = flags(dir.path());
    missing.work_dir = dir.path().join("nowhere");
    let error = validate(&missing).expect_err("a missing directory");
    assert_eq!(
        error.to_string(),
        format!(
            "--work-dir {} is not an existing directory",
            missing.work_dir.display()
        )
    );

    let mut empty = flags(dir.path());
    empty.web_roots = dir.path().join("empty.pem");
    std::fs::write(&empty.web_roots, "no certificate here\n").expect("write");
    assert_eq!(
        validate(&empty).expect_err("no certificate").to_string(),
        format!(
            "--web-roots: {} holds no certificate",
            empty.web_roots.display()
        )
    );

    let mut gone = flags(dir.path());
    gone.web_roots = dir.path().join("gone.pem");
    let text = validate(&gone).expect_err("no file").to_string();
    assert!(
        text.starts_with(&format!(
            "--web-roots: cannot read {}: ",
            gone.web_roots.display()
        )),
        "{text}"
    );

    let mut broken = flags(dir.path());
    broken.web_roots = dir.path().join("broken.pem");
    std::fs::write(
        &broken.web_roots,
        "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
    )
    .expect("write");
    let text = validate(&broken).expect_err("bad PEM").to_string();
    assert!(
        text.starts_with(&format!(
            "--web-roots: {} is not PEM certificates: ",
            broken.web_roots.display()
        )),
        "{text}"
    );
}

/// Catches a roots file read as fewer or more certificates than it holds.
#[test]
fn every_certificate_of_the_roots_file_is_read() {
    let dir = scratch("roots");
    let path = dir.path().join("two.pem");
    std::fs::write(
        &path,
        format!(
            "{}{}",
            crate::testing::root_pem(),
            crate::testing::root_pem()
        ),
    )
    .expect("write");
    assert_eq!(read_roots(&path).expect("two roots").len(), 2);
    let (settings, roots) = validate(&flags(dir.path())).expect("valid");
    assert_eq!(roots.len(), 1);
    assert_eq!(settings.service, "svc");
    assert_eq!(settings.version, "1.2.3");
}

/// A writer whose bytes the test reads back after the ports that own it wrote them.
#[derive(Clone, Default)]
struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Catches the real ports printing nothing, another line, or lines run together, and handing ctl
/// another transport than the network one: each line the flow says comes out exactly, with its
/// newline, in order; and the transport they give reads the call's CA file before it connects,
/// so a missing one is refused with ctl's own read error and no connection.
#[test]
fn the_network_ports_pause_and_say() {
    let out = Shared::default();
    let mut ports = crate::ports::NetworkPorts::writing_to(&[], Box::new(out.clone()));
    ports.pause(0);
    ports.say("::add-mask::a-token");
    ports.say("poll 1: in_progress, desired 1.2.3");
    assert_eq!(
        String::from_utf8(out.0.borrow().clone()).expect("UTF-8"),
        "::add-mask::a-token\npoll 1: in_progress, desired 1.2.3\n"
    );
    let dir = scratch("network-ports");
    let missing = dir.path().join("missing-ca.pem");
    let target = hostkeeper_ctl::Target {
        endpoint: hostkeeper_ctl::Endpoint::parse("https://coordinator.example")
            .expect("an https endpoint"),
        transport: hostkeeper_ctl::TransportKind::Grpc,
        ca_file: missing.clone(),
        client_identity: None,
        timeout: std::time::Duration::from_secs(1),
    };
    let refused = hostkeeper_ctl::Connect::connect(ports.connect(), &target)
        .err()
        .map(|e| e.to_string());
    let not_found = std::fs::read(&missing).expect_err("the CA file is missing");
    assert_eq!(
        refused,
        Some(format!(
            "cannot read the CA from {}: {not_found}",
            missing.display()
        ))
    );
}

/// The flags of an index release in `dir`, whose file holds `bytes`.
fn index_flags(dir: &std::path::Path, bytes: &[u8]) -> SetDesiredFlags {
    let mut flags = flags(dir);
    let file = dir.join("index.json");
    std::fs::write(&file, bytes).expect("write the index");
    flags.package_digest = None;
    flags.release_index_file = Some(file);
    flags.release_index_digest = Some("b".repeat(64));
    flags
}

/// Catches an index changed on the way to the request (a newline added or dropped, a CRLF turned
/// into LF, a multi-byte character mangled), and the digest the caller states being replaced by
/// one the client computed.
#[test]
fn the_index_is_read_byte_for_byte_and_its_digest_kept_as_given() {
    let dir = scratch("index-bytes");
    for (name, text) in [
        ("no-newline", "{\"service\":\"svc\"}"),
        ("one-newline", "{\"service\":\"svc\"}\n"),
        ("crlf", "{\r\n \"service\": \"svc\"\r\n}\r\n\r\n"),
        ("unicode", "{\"note\":\"caf\u{e9} \u{1f680}\"}  \n"),
    ] {
        let flags = index_flags(dir.path(), text.as_bytes());
        let (settings, _) = validate(&flags).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            settings.release,
            Release::Index {
                text: text.to_owned(),
                digest: "b".repeat(64)
            },
            "{name}"
        );
    }
    let package = validate(&flags(dir.path())).expect("valid").0;
    assert_eq!(package.release, Release::Package("a".repeat(64)));
}

/// Catches an index file that is missing, too large, or not UTF-8 being accepted, one of exactly
/// the limit being refused, and any of them reaching a token request.
#[test]
fn an_index_file_that_cannot_be_used_is_refused_before_any_token() {
    let dir = scratch("index-file");
    let mut gone = index_flags(dir.path(), b"{}");
    let path = dir.path().join("gone.json");
    gone.release_index_file = Some(path.clone());
    let text = validate(&gone).expect_err("no file").to_string();
    assert!(
        text.starts_with(&format!(
            "--release-index-file {}: cannot be read (",
            path.display()
        )),
        "{text}"
    );

    let mut directory = index_flags(dir.path(), b"{}");
    directory.release_index_file = Some(dir.path().to_owned());
    let text = validate(&directory).expect_err("a directory").to_string();
    assert!(text.contains(": cannot be read ("), "{text}");

    let big = index_flags(dir.path(), &vec![b' '; MAX_INDEX_BYTES + 1]);
    let path = big.release_index_file.clone().expect("a file");
    assert_eq!(
        validate(&big).expect_err("too large").to_string(),
        format!(
            "--release-index-file {}: is larger than 3072 bytes",
            path.display()
        )
    );
    let exact = index_flags(dir.path(), &vec![b' '; MAX_INDEX_BYTES]);
    assert!(validate(&exact).is_ok());

    let latin = index_flags(dir.path(), b"{\"note\":\"caf\xe9\"}");
    let path = latin.release_index_file.clone().expect("a file");
    assert_eq!(
        validate(&latin).expect_err("not UTF-8").to_string(),
        format!("--release-index-file {}: is not UTF-8", path.display())
    );

    for flags in [gone, big, latin] {
        let mut fake = Fake::new(dir.path(), stored("1.2.3"), vec![]);
        let mut stderr = Vec::new();
        assert_eq!(set_desired(&flags, &mut stderr, |_| &mut fake), 2);
        assert!(fake.log.lock().expect("lock").audiences.is_empty());
        assert!(!dir.path().join("github-output").exists());
    }
}

/// Catches the other flags going unchecked when the index is good.
#[test]
fn the_other_flags_are_checked_when_the_index_is_good() {
    let dir = scratch("index-other");
    let mut flags = index_flags(dir.path(), b"{}");
    flags.endpoint = "http://coordinator.example".to_owned();
    assert_eq!(
        validate(&flags).expect_err("endpoint").to_string(),
        "--endpoint must start with https://"
    );
}
