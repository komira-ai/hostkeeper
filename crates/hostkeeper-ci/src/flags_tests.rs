//! The checks of the flags, and the roots file.

use crate::flags::{SetDesiredFlags, is_service, is_version, validate};
use crate::flow::set_desired;
use crate::ports::Ports;
use crate::testing::{Fake, flags, scratch, stored};
use crate::token::read_roots;

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
            |f| f.package_digest = "A".repeat(64),
            "hostkeeper ci set-desired: --package-digest must be 64 lower-case hexadecimal characters\n",
        ),
        case(
            "digest-length",
            |f| f.package_digest = "a".repeat(63),
            "hostkeeper ci set-desired: --package-digest must be 64 lower-case hexadecimal characters\n",
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

/// Catches the real ports being given a different pause or line than the flow asked for: they
/// are the thread's sleep (of no seconds here) and stdout.
#[test]
fn the_network_ports_pause_and_say() {
    let mut ports = crate::ports::NetworkPorts::new(&[]);
    ports.pause(0);
    ports.say("a line for the test log");
    let _ = ports.connect();
}
