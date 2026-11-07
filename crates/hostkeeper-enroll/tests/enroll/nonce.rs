//! The Mac nonce (docs/design/hostkeeper-mdm.md, section 5.3): the enroller reads the managed
//! preferences in each step that enrolls, posts `Enroll` once more with the nonce they hold for
//! its own request, then polls; and it posts nothing more for a file it cannot trust or use.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_config::trust::TrustError;
use hostkeeper_enroll::{
    EnrollRequest, Enroller, Enrollment, MAC_NONCE_BYTES, MANAGED_ENROLLMENT, MacNonce,
    ManagedError, ManagedFile, ManagedNonce, ManagedPreferences, NOT_ENROLLED, Outcome,
    RenewalState, RequestId, read_nonce,
};
use hostkeeper_testkit::Scratch;
use plist::Value;

use crate::support::{HOST, Managed, World, managed_plist, nonce_plist};

/// The nonce the tests' gate gives first.
const FIRST: [u8; MAC_NONCE_BYTES] = [0x11; MAC_NONCE_BYTES];
/// The nonce it gives when it issues again.
const SECOND: [u8; MAC_NONCE_BYTES] = [0x22; MAC_NONCE_BYTES];

/// The id of the enroller's pending request.
fn pending_id(enroller: &Enroller) -> RequestId {
    match enroller.enrollment() {
        Enrollment::Pending { key_sha256 } => key_sha256,
        other => panic!("not pending: {other:?}"),
    }
}

/// The nonce bytes each `Enroll` the coordinator received carried, in order; empty for none.
fn posted_nonces(world: &World) -> Vec<Vec<u8>> {
    world
        .coordinator()
        .enrolls
        .iter()
        .map(|request| {
            request
                .mac_nonce
                .as_ref()
                .map_or_else(Vec::new, |nonce| nonce.as_bytes().to_vec())
        })
        .collect()
}

/// `request` without its nonce.
fn without_nonce(request: &EnrollRequest) -> EnrollRequest {
    EnrollRequest {
        mac_nonce: None,
        ..request.clone()
    }
}

/// Catches an enroller that never sends the nonce, sends it before polling is due, sends it at
/// every step, or sends it changed: the host posts once and polls; once the profile holds a
/// nonce for its request, the next step posts the same request again with that nonce and then
/// polls; the step after posts nothing more; a nonce issued again is posted once more.
#[test]
fn a_pending_host_reposts_once_with_the_nonce_for_its_request() {
    let world = World::new("nonce-repost");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);
    assert_eq!(posted_nonces(&world), vec![Vec::<u8>::new()]);
    assert_eq!(world.managed_reads(), 2);

    let id = pending_id(&enroller);
    world.set_managed(Managed::Bytes(nonce_plist(id.as_str(), &FIRST)));
    let polls = world.coordinator().polls.len();
    assert_eq!(enroller.run_once().expect("reposted"), Outcome::Pending);
    assert_eq!(posted_nonces(&world), vec![Vec::new(), FIRST.to_vec()]);
    {
        let c = world.coordinator();
        assert_eq!(without_nonce(&c.enrolls[1]), c.enrolls[0]);
        // After the post, the step polls as before: an unsigned poll and a signed one.
        assert_eq!(c.polls.len(), polls + 2);
    }

    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);
    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);
    assert_eq!(posted_nonces(&world).len(), 2);

    world.set_managed(Managed::Bytes(nonce_plist(id.as_str(), &SECOND)));
    assert_eq!(enroller.run_once().expect("reposted"), Outcome::Pending);
    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);
    assert_eq!(
        posted_nonces(&world),
        vec![Vec::new(), FIRST.to_vec(), SECOND.to_vec()]
    );
}

/// Catches a repost whose approval is not followed through: the coordinator approves the
/// request the nonce was posted with and answers it decided, and the same step's poll brings
/// the certificate.
#[test]
fn a_nonce_approved_on_its_repost_is_enrolled_in_the_same_step() {
    let world = World::new("nonce-approved");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    world.coordinator().nonce_approves = Some(HOST.to_owned());
    let id = pending_id(&enroller);
    world.set_managed(Managed::Bytes(nonce_plist(id.as_str(), &FIRST)));
    let Outcome::Enrolled(identity) = enroller.run_once().expect("enrolled") else {
        panic!("not enrolled");
    };
    assert_eq!(identity.host_id.as_str(), HOST);
    assert_eq!(posted_nonces(&world), vec![Vec::new(), FIRST.to_vec()]);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(posted_nonces(&world).len(), 2);
}

/// Catches a daemon that starts with the profile already in place and posts twice: its first
/// post carries the nonce, and no later step posts again.
#[test]
fn a_host_started_after_the_profile_posts_the_nonce_in_its_first_post() {
    let world = World::new("nonce-first-post");
    let mut first = world.open();
    assert_eq!(first.run_once().expect("posted"), Outcome::Pending);
    let id = pending_id(&first);
    drop(first);
    world.set_managed(Managed::Bytes(nonce_plist(id.as_str(), &FIRST)));

    // A restarted daemon: the same pending key, nothing posted by this process yet.
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);
    assert_eq!(
        posted_nonces(&world),
        vec![Vec::<u8>::new(), FIRST.to_vec()]
    );
}

/// Catches a nonce taken as posted when its post failed: a post that got no answer is made
/// again, with the nonce, at the next step, and then not again.
#[test]
fn a_nonce_post_that_failed_is_posted_at_the_next_step() {
    let world = World::new("nonce-lost");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    let id = pending_id(&enroller);
    world.set_managed(Managed::Bytes(nonce_plist(id.as_str(), &FIRST)));
    world.coordinator().down = Some("connection refused".to_owned());
    enroller.run_once().expect_err("the coordinator is down");
    world.coordinator().down = None;
    assert_eq!(enroller.run_once().expect("reposted"), Outcome::Pending);
    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);
    assert_eq!(posted_nonces(&world), vec![Vec::new(), FIRST.to_vec()]);
}

/// The managed preferences a test cannot trust or use, each with what makes it so.
fn untrusted_profiles(own: &RequestId) -> Vec<(&'static str, Managed)> {
    let other = RequestId::of_spki(b"another key");
    let nonce = || Value::Data(FIRST.to_vec());
    let id = || Value::String(own.to_string());
    let bytes = |entries| Managed::Bytes(managed_plist(entries));
    vec![
        (
            "for another request",
            Managed::Bytes(nonce_plist(other.as_str(), &FIRST)),
        ),
        ("missing", Managed::Missing),
        ("not root's", Managed::NotRoots),
        ("writable by others", Managed::Writable),
        ("not a plist", Managed::Bytes(b"nonce = 1".to_vec())),
        ("not a dictionary", {
            let mut out = Vec::new();
            Value::Array(vec![nonce(), id()])
                .to_writer_xml(&mut out)
                .expect("an XML plist");
            Managed::Bytes(out)
        }),
        ("no nonce", bytes(vec![("request_id", id())])),
        (
            "a nonce that is not data",
            bytes(vec![
                ("nonce", Value::String("11".repeat(32))),
                ("request_id", id()),
            ]),
        ),
        (
            "a short nonce",
            Managed::Bytes(nonce_plist(own.as_str(), &FIRST[1..])),
        ),
        ("no request_id", bytes(vec![("nonce", nonce())])),
        (
            "a request_id that is not a string",
            bytes(vec![
                ("nonce", nonce()),
                ("request_id", Value::Integer(7.into())),
            ]),
        ),
        (
            "a request_id in upper case",
            Managed::Bytes(nonce_plist(&own.as_str().to_ascii_uppercase(), &FIRST)),
        ),
    ]
}

/// Catches a profile the enroller should not trust being posted, or failing enrollment: for
/// another request's nonce, a missing file, one not root's or writable by others, and each way
/// the file is malformed, the step posts nothing more and is pending, and the operator's
/// approval still enrolls the host.
#[test]
fn the_daemon_ignores_a_profile_it_cannot_trust() {
    let cases = untrusted_profiles(&RequestId::of_spki(b"any key")).len();
    assert_eq!(cases, 12);
    for at in 0..cases {
        let world = World::new(&format!("nonce-untrusted-{at}"));
        let mut enroller = world.open();
        assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
        let (what, managed) = untrusted_profiles(&pending_id(&enroller)).swap_remove(at);
        world.set_managed(managed);
        assert_eq!(
            enroller.run_once().expect("polled"),
            Outcome::Pending,
            "{what}"
        );
        assert_eq!(posted_nonces(&world), vec![Vec::<u8>::new()], "{what}");
        world.coordinator().approve(HOST);
        assert!(
            matches!(enroller.run_once(), Ok(Outcome::Enrolled(_))),
            "{what}"
        );
        assert_eq!(world.managed_reads(), 3, "{what}");
    }
}

/// Catches a nonce read while the host is enrolled and its certificate valid: such a step
/// enrolls nothing, so it reads nothing.
#[test]
fn an_enrolled_host_does_not_read_the_profile() {
    let world = World::new("nonce-enrolled");
    let mut enroller = world.enrolled();
    let reads = world.managed_reads();
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.managed_reads(), reads);
}

/// Catches a host without managed preferences (Linux) whose enrollment changes: it posts once,
/// polls, and is enrolled at approval.
#[test]
fn a_host_without_managed_preferences_enrolls_as_before() {
    let world = World::new("nonce-none");
    let ports = hostkeeper_enroll::Ports {
        managed: None,
        ..world.ports_with(None, None)
    };
    let mut enroller = Enroller::open(world.settings(), ports).expect("open");
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    world.coordinator().approve(HOST);
    assert!(matches!(enroller.run_once(), Ok(Outcome::Enrolled(_))));
    assert_eq!(posted_nonces(&world), vec![Vec::<u8>::new()]);
    assert_eq!(world.managed_reads(), 0);
}

/// Catches a byte of the nonce in what the daemon says: the loop's whole log, from the post
/// through the repost with the nonce to the certificate, equals its two lines; and the `Debug`
/// of the nonce, of what the profile says and of the request that carries it shows none of it.
#[test]
fn no_byte_of_a_nonce_reaches_a_log_or_the_audit() {
    let world = World::new("nonce-log");
    let mut enroller = world.open();
    world.coordinator().nonce_approves = Some(HOST.to_owned());
    let mut lines = Vec::new();
    let mut steps = 0;
    let renewal = RenewalState::default();
    let managed = world.managed.clone();
    let mut id = None;
    enroller.run_loop(
        &mut |line| lines.push(line.to_owned()),
        &mut |_step, enrollment| {
            if let Enrollment::Pending { key_sha256 } = enrollment {
                let plist = nonce_plist(key_sha256.as_str(), &FIRST);
                managed.lock().expect("managed").file = Managed::Bytes(plist);
                id = Some(key_sha256);
            }
        },
        &renewal,
        &mut || {
            steps += 1;
            steps <= 3
        },
    );
    assert_eq!(
        lines,
        vec![
            NOT_ENROLLED.to_owned(),
            format!("enrolled as host:{HOST} (generation 1)"),
        ]
    );
    assert_eq!(posted_nonces(&world), vec![Vec::new(), FIRST.to_vec()]);

    let nonce = MacNonce::new(FIRST.to_vec()).expect("a nonce");
    assert_eq!(format!("{nonce:?}"), "MacNonce(..)");
    let id = id.expect("the request id");
    let said = ManagedNonce {
        request_id: id.clone(),
        nonce,
    };
    assert_eq!(
        format!("{said:?}"),
        format!("ManagedNonce {{ request_id: RequestId(\"{id}\"), nonce: MacNonce(..) }}")
    );
    let request = world.coordinator().enrolls[1].clone();
    assert_eq!(
        format!("{request:?}"),
        format!(
            "{:?}",
            EnrollRequest {
                mac_nonce: None,
                ..request.clone()
            }
        )
        .replace("mac_nonce: None", "mac_nonce: Some(MacNonce(..))")
    );
}

/// Catches managed preferences read wrong: a binary and an XML property list each give the
/// nonce and the request id, keys beside the two are ignored, and each malformed file is
/// refused with its reason, whose text carries no byte of the file.
#[test]
fn the_managed_preferences_are_read_binary_or_xml() {
    let id = RequestId::of_spki(b"a key");
    let dict: plist::Dictionary = [
        ("nonce", Value::Data(FIRST.to_vec())),
        ("request_id", Value::String(id.to_string())),
        ("PayloadUUID", Value::String("ignored".to_owned())),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    let mut binary = Vec::new();
    Value::Dictionary(dict.clone())
        .to_writer_binary(&mut binary)
        .expect("a binary plist");
    let mut xml = Vec::new();
    Value::Dictionary(dict)
        .to_writer_xml(&mut xml)
        .expect("an XML plist");
    let expected = ManagedNonce {
        request_id: id.clone(),
        nonce: MacNonce::new(FIRST.to_vec()).expect("a nonce"),
    };
    assert_eq!(read_nonce(&binary), Ok(expected.clone()));
    assert_eq!(read_nonce(&xml), Ok(expected));

    let refusals = [
        (
            b"nonce = 1".to_vec(),
            ManagedError::NotPlist,
            "not a property list",
        ),
        (
            {
                let mut out = Vec::new();
                Value::Array(Vec::new())
                    .to_writer_xml(&mut out)
                    .expect("an XML plist");
                out
            },
            ManagedError::NotDictionary,
            "not a dictionary at its top level",
        ),
        (
            managed_plist(vec![("request_id", Value::String(id.to_string()))]),
            ManagedError::Missing("nonce"),
            "no nonce key",
        ),
        (
            managed_plist(vec![("nonce", Value::Boolean(true))]),
            ManagedError::NonceNotData,
            "its nonce is not data",
        ),
        (
            nonce_plist(id.as_str(), &[0x11; MAC_NONCE_BYTES + 1]),
            ManagedError::NonceLength(MAC_NONCE_BYTES + 1),
            "its nonce has 33 bytes, not 32",
        ),
        (
            managed_plist(vec![("nonce", Value::Data(FIRST.to_vec()))]),
            ManagedError::Missing("request_id"),
            "no request_id key",
        ),
        (
            nonce_plist(&id.as_str()[1..], &FIRST),
            ManagedError::RequestId,
            "its request_id is not 64 lower-case hex digits",
        ),
    ];
    for (bytes, error, text) in refusals {
        assert_eq!(read_nonce(&bytes), Err(error.clone()), "{text}");
        assert_eq!(error.to_string(), text);
    }
}

/// Catches a nonce of the wrong length made a nonce.
#[test]
fn a_mac_nonce_is_32_bytes() {
    assert_eq!(MacNonce::new(Vec::new()), None);
    assert_eq!(MacNonce::new(vec![1; MAC_NONCE_BYTES - 1]), None);
    assert_eq!(MacNonce::new(vec![1; MAC_NONCE_BYTES + 1]), None);
    let nonce = MacNonce::new(vec![1; MAC_NONCE_BYTES]).expect("a nonce");
    assert_eq!(nonce.as_bytes(), &[1; MAC_NONCE_BYTES]);
}

/// The effective uid of the test.
fn my_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Writes `bytes` to `path` with `mode`.
fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    std::fs::write(path, bytes).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Catches the managed preferences file read without its checks: a file the trusted account
/// owns and only it can write is read; one another account owns, one the group can write, a
/// link, and a missing file are refused; and the host's file is the documented path, trusted
/// for root.
#[test]
fn the_managed_file_is_read_only_when_root_trusts_it() {
    let dir = Scratch::under(
        &std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hostkeeper-enroll"),
        "managed-file",
    )
    .expect("a scratch directory");
    let file = dir.path().join("dev.hostkeeper.enrollment.plist");
    write_mode(&file, b"bytes", 0o644);
    assert_eq!(
        ManagedFile::new(&file, my_uid()).read().expect("read"),
        b"bytes"
    );
    assert!(matches!(
        ManagedFile::new(&file, my_uid() + 1).read(),
        Err(TrustError::Owner { found, wanted }) if found == my_uid() && wanted == my_uid() + 1
    ));
    write_mode(&file, b"bytes", 0o664);
    assert!(matches!(
        ManagedFile::new(&file, my_uid()).read(),
        Err(TrustError::Writable { mode: 0o664 })
    ));
    let link = dir.path().join("link.plist");
    std::os::unix::fs::symlink(&file, &link).expect("a link");
    assert!(matches!(
        ManagedFile::new(&link, my_uid()).read(),
        Err(TrustError::Io(_))
    ));
    assert!(matches!(
        ManagedFile::new(&dir.path().join("missing.plist"), my_uid()).read(),
        Err(TrustError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
    ));
    assert_eq!(
        ManagedFile::host(),
        ManagedFile::new(Path::new(MANAGED_ENROLLMENT), 0)
    );
    assert_eq!(
        MANAGED_ENROLLMENT,
        "/Library/Managed Preferences/dev.hostkeeper.enrollment.plist"
    );
}
