//! `Repositories.Discover` and `Repositories.Add` ([`crate::operator_repositories`],
//! [`crate::repository_discover`]) against an origin served in process
//! ([`crate::repository_origin_support`]): what a read of a repository's descriptor and key shows,
//! what `Add` refuses when what it reads is not what was confirmed, and what it stores. The
//! deadlines run on tokio's paused clock; no socket is opened.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::RepositoriesRefusal as Refusal;
use hostkeeper_release::registry::{self, keyring_sha256};
use serde_json::{Value, json};

use crate::marks_support::{operator, rig_on};
use crate::operator::Caller;
use crate::repositories_support::{credential_in, fingerprint, keyring};
use crate::repository_discover::Limits;
use crate::repository_origin::RepositoryOrigin;
use crate::repository_origin_support::{
    FakeOrigin, Then, bare, hanging_dial, refusing_dial, reply, response,
};
use crate::store::Fake;
use crate::testutil::{Log, Rig};

const BASE: &str = "https://origin.test";
const SECRET: &str = "Bearer s3cret-token-value";

/// What the origin serves: the descriptor text and the keyring, by path.
#[derive(Clone)]
struct Served {
    descriptor: Arc<Mutex<Vec<u8>>>,
    keyring: Arc<Mutex<Vec<u8>>>,
}

impl Served {
    /// A repository of the key of `seed`, apt under `apt`, the keyring at `apt/key.gpg`, serving
    /// macOS installers of `hello` from the directory `tools`.
    fn of(seed: u8) -> Self {
        let keyring = keyring(seed);
        Self {
            descriptor: Arc::new(Mutex::new(descriptor(&fingerprint(&keyring)))),
            keyring: Arc::new(Mutex::new(keyring)),
        }
    }

    fn answer(&self) -> crate::repository_origin_support::Answer {
        let me = self.clone();
        Arc::new(move |path: &str| match path {
            "/hostkeeper-repository.json" => {
                Then::Write(response("200 OK", &me.descriptor.lock().expect("lock")))
            }
            "/apt/key.gpg" => Then::Write(response("200 OK", &me.keyring.lock().expect("lock"))),
            _ => Then::Write(response("404 Not Found", b"")),
        })
    }
}

/// The descriptor of a repository holding the key `print`.
fn descriptor(print: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": 1,
        "name": "origin",
        "description": "an origin",
        "channels": ["testing", "stable"],
        "key": { "keyring": "apt/key.gpg", "fingerprints": [print] },
        "formats": {
            "apt": { "path": "apt", "components": ["main"], "architectures": ["amd64"] },
            "macos-pkg": {
                "path": "pkg",
                "platforms": ["macos-arm64"],
                "packages": { "hello": { "dir": "tools" } }
            }
        }
    }))
    .expect("json")
}

/// A rig whose coordinator reads origins through `dial`, over `store`.
fn rig_over(name: &str, store: &Fake, fake: &FakeOrigin, dial: hostkeeper_oidc::Dial) -> Rig {
    rig_limited(name, store, fake, dial, Limits::default())
}

/// The same, reading within `limits`.
fn rig_limited(
    name: &str,
    store: &Fake,
    fake: &FakeOrigin,
    dial: hostkeeper_oidc::Dial,
    limits: Limits,
) -> Rig {
    let mut rig = Rig::new(name);
    let client = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(dial);
    rig.coordinator = crate::marks_support::start_on(store, &rig.clock, "")
        .with_repository_origin(client)
        .with_discovery(limits.timeout, limits.descriptor_max_bytes);
    rig.at(2_000);
    rig
}

fn rig_serving(name: &str, served: &Served) -> (Rig, FakeOrigin, Fake) {
    let (fake, store) = (FakeOrigin::new(), Fake::new());
    let rig = rig_over(name, &store, &fake, fake.dial(served.answer()));
    (rig, fake, store)
}

fn discover_of(base: &str) -> proto::DiscoverRepositoryRequest {
    proto::DiscoverRepositoryRequest {
        base_uri: base.to_owned(),
        ..proto::DiscoverRepositoryRequest::default()
    }
}

async fn discover(rig: &Rig, base: &str) -> Result<proto::Discovery, Refusal> {
    rig.coordinator
        .repositories_discover(&operator(), discover_of(base))
        .await
        .map(|answer| answer.discovery.expect("a discovery"))
}

/// The `Add` that confirms what `found` showed, under `name`.
fn add_of(name: &str, found: &proto::Discovery) -> proto::AddRepositoryRequest {
    proto::AddRepositoryRequest {
        name: name.to_owned(),
        base_uri: BASE.to_owned(),
        descriptor_sha256: found.descriptor_sha256.clone(),
        fingerprints: found.fingerprints.clone(),
        reason: "add it".to_owned(),
        ..proto::AddRepositoryRequest::default()
    }
}

async fn add(
    rig: &Rig,
    request: proto::AddRepositoryRequest,
) -> Result<proto::AddRepositoryResponse, Refusal> {
    rig.coordinator.repositories_add(&operator(), request).await
}

/// Proves: `Discover` answers what the descriptor says (name, description, channels), the
/// platforms (the Linux one its apt format names and the Mac's), the macOS packages it maps, the
/// fingerprint and user ids computed from the keyring it names, and the SHA-256 of the descriptor
/// and of the keyring; it is not the pinned key; and it reads exactly the descriptor and the
/// keyring, once each, with no credential.
///
/// Catches: a fingerprint taken from the descriptor instead of the keyring, a platform list that
/// ignores the formats, or a read that sends a credential to a public repository.
#[tokio::test]
async fn discover_reports_platforms_channels_and_computed_fingerprints() {
    let served = Served::of(1);
    let (rig, fake, store) = rig_serving("discover-shows", &served);
    let before = (store.desired().repositories, store.audit().len());
    let found = discover(&rig, BASE).await.expect("discovered");
    let ring = keyring(1);
    assert_eq!(found.name, "origin");
    assert_eq!(found.description, "an origin");
    assert_eq!(found.channels, ["testing", "stable"]);
    assert_eq!(found.platforms, ["linux-x86_64", "macos-arm64"]);
    assert_eq!(found.macos_packages, ["hello"]);
    assert_eq!(found.fingerprints, [fingerprint(&ring)]);
    assert!(found.user_ids.is_empty());
    assert_eq!(found.keyring_sha256, keyring_sha256(&ring));
    assert_eq!(
        found.descriptor_sha256,
        keyring_sha256(&served.descriptor.lock().expect("lock"))
    );
    assert!(!found.pinned);
    let heads = fake.heard();
    assert_eq!(heads.len(), 2);
    assert!(heads[0].starts_with("GET /hostkeeper-repository.json HTTP/1.1"));
    assert!(heads[1].starts_with("GET /apt/key.gpg HTTP/1.1"));
    assert!(
        heads
            .iter()
            .all(|head| !head.to_lowercase().contains("authorization"))
    );
    // Discover changes nothing.
    assert_eq!((store.desired().repositories, store.audit().len()), before);
}

/// Proves: the key of the built-in `komira` is reported pinned, and a repository whose keyring
/// is the pinned one (served at the built-in descriptor's own paths) is the only one that is.
///
/// Catches: a pinned note shown for a key that is not the pinned one, or never shown.
#[tokio::test]
async fn discover_marks_the_pinned_key() {
    let ring = crate::builtin::komira().repository.keyring.clone();
    let served = Served {
        descriptor: Arc::new(Mutex::new(descriptor(crate::builtin::PINNED[0]))),
        keyring: Arc::new(Mutex::new(ring)),
    };
    let (rig, _fake, _store) = rig_serving("discover-pinned", &served);
    let found = discover(&rig, BASE).await.expect("discovered");
    assert!(found.pinned);
    assert_eq!(found.fingerprints, crate::builtin::PINNED);
    assert_eq!(found.user_ids.len(), 1);
    assert!(
        found.user_ids[0].starts_with("komira-ai packages archive <"),
        "{:?}",
        found.user_ids
    );
}

/// Proves: a descriptor that claims a fingerprint the keyring does not hold is refused
/// `descriptor_invalid`, as is one that is no descriptor, a higher version, and a keyring that is
/// no keyring; none changes anything.
///
/// Catches: a descriptor's claim trusted over the keyring's bytes.
#[tokio::test]
async fn discover_refuses_a_descriptor_whose_fingerprints_differ_from_its_keyring() {
    let served = Served::of(1);
    let (rig, _fake, _store) = rig_serving("discover-invalid", &served);
    *served.descriptor.lock().expect("lock") = descriptor(&fingerprint(&keyring(2)));
    assert_eq!(discover(&rig, BASE).await, Err(Refusal::DescriptorInvalid));
    *served.descriptor.lock().expect("lock") = b"{}".to_vec();
    assert_eq!(discover(&rig, BASE).await, Err(Refusal::DescriptorInvalid));
    let newer = json!({"version": 2});
    *served.descriptor.lock().expect("lock") = serde_json::to_vec(&newer).expect("json");
    assert_eq!(discover(&rig, BASE).await, Err(Refusal::DescriptorInvalid));
    *served.descriptor.lock().expect("lock") = descriptor(&fingerprint(&keyring(1)));
    *served.keyring.lock().expect("lock") = vec![1, 2, 3];
    assert_eq!(discover(&rig, BASE).await, Err(Refusal::DescriptorInvalid));
}

/// Proves: a read the coordinator cannot make is `discovery_failed`, whatever the cause: a
/// redirect, a 404, a 500, no `Content-Length`, a descriptor over the bound, a keyring over 64
/// KiB, a refused connection, no origin client, and a connection that never answers.
///
/// Catches: a redirect followed off the repository, a body read past its bound, or a failure
/// answered as a valid repository.
#[tokio::test(start_paused = true)]
async fn discover_refuses_a_redirect_a_404_and_an_oversize_body() {
    let served = Served::of(1);
    let fake = FakeOrigin::new();
    let store = Fake::new();
    let big = vec![b'x'; 65_537];
    let cases: Vec<(&str, hostkeeper_oidc::Dial)> = vec![
        (
            "a redirect",
            fake.dial_all(reply(
                "302 Found",
                &[("Location", "https://elsewhere.test/")],
                b"",
            )),
        ),
        (
            "a 404",
            fake.dial_all(Then::Write(response("404 Not Found", b""))),
        ),
        (
            "a 500",
            fake.dial_all(Then::Write(response("500 Oops", b""))),
        ),
        (
            "no length",
            fake.dial_all(bare(&["HTTP/1.1 200 OK", "Connection: close"], b"{}")),
        ),
        (
            "an oversize descriptor",
            fake.dial_all(Then::Write(response("200 OK", &big))),
        ),
        ("a refused connection", refusing_dial()),
        ("a connection that never answers", hanging_dial()),
        ("a silent origin", fake.dial_all(Then::Hang)),
    ];
    for (name, dial) in cases {
        let rig = rig_over("discover-failed", &store, &fake, dial);
        assert_eq!(
            discover(&rig, BASE).await,
            Err(Refusal::DiscoveryFailed),
            "{name}"
        );
    }
    // The keyring: a 404, and one over the bound of a keyring.
    let missing = Served {
        keyring: Arc::new(Mutex::new(Vec::new())),
        ..served.clone()
    };
    let answers: Arc<dyn Fn(&str) -> Then + Send + Sync> = {
        let descriptor = served.descriptor.clone();
        Arc::new(move |path: &str| match path {
            "/hostkeeper-repository.json" => {
                Then::Write(response("200 OK", &descriptor.lock().expect("lock")))
            }
            _ => Then::Write(response("404 Not Found", b"")),
        })
    };
    let rig = rig_over("discover-no-keyring", &store, &fake, fake.dial(answers));
    assert_eq!(discover(&rig, BASE).await, Err(Refusal::DiscoveryFailed));
    *missing.keyring.lock().expect("lock") = vec![b'k'; registry::MAX_KEYRING + 1];
    let rig = rig_over(
        "discover-big-keyring",
        &store,
        &fake,
        fake.dial(missing.answer()),
    );
    assert_eq!(discover(&rig, BASE).await, Err(Refusal::DiscoveryFailed));
    // No origin client at all.
    let bare_rig = rig_on("discover-no-client", &store, "");
    assert_eq!(
        discover(&bare_rig, BASE).await,
        Err(Refusal::DiscoveryFailed)
    );
}

/// Proves: the descriptor's size bound and the deadline are the coordinator's own settings: a
/// descriptor of 100 bytes is refused with a bound of 50, and an origin slower than a 1 second
/// deadline fails while the same origin passes a 30 second one.
///
/// Catches: `--repository-descriptor-max-bytes` and `--repository-discover-secs` that do not
/// reach the read.
#[tokio::test(start_paused = true)]
async fn the_bound_and_the_deadline_are_the_ones_the_coordinator_is_given() {
    let served = Served::of(1);
    let (fake, store) = (FakeOrigin::new(), Fake::new());
    let length = served.descriptor.lock().expect("lock").len();
    let limited = |bound: usize, secs: u64| {
        let limits = Limits {
            timeout: Duration::from_secs(secs),
            descriptor_max_bytes: bound,
        };
        rig_limited(
            "discover-limits",
            &store,
            &fake,
            fake.dial(served.answer()),
            limits,
        )
    };
    assert_eq!(
        discover(&limited(length - 1, 20), BASE).await,
        Err(Refusal::DiscoveryFailed)
    );
    assert!(discover(&limited(length, 20), BASE).await.is_ok());

    // An origin that takes longer than the deadline to answer.
    let limits = Limits {
        timeout: Duration::from_secs(1),
        descriptor_max_bytes: length,
    };
    let slow = rig_limited("discover-slow", &store, &fake, hanging_dial(), limits);
    assert_eq!(discover(&slow, BASE).await, Err(Refusal::DiscoveryFailed));
}

/// Proves: a URL that is no repository base (`http://`, a trailing `/`, a query), a private
/// request with no credential file, a public one that names one, and an access that is neither
/// are `invalid_request`; CI's token and none are `forbidden` and `unauthorized`.
///
/// Catches: a URL the origin client cannot judge handed to it, or a credential sent where it was
/// not asked for.
#[tokio::test]
async fn discover_refuses_what_is_no_request() {
    let served = Served::of(1);
    let (rig, fake, _store) = rig_serving("discover-request", &served);
    for base in [
        "http://origin.test",
        "https://origin.test/",
        "https://origin.test?x=1",
        "",
    ] {
        assert_eq!(
            discover(&rig, base).await,
            Err(Refusal::InvalidRequest),
            "{base}"
        );
    }
    for (access, file) in [("hosts", ""), ("public", "/x"), ("", "/x"), ("secret", "")] {
        let request = proto::DiscoverRepositoryRequest {
            access: access.to_owned(),
            credential_file: file.to_owned(),
            ..discover_of(BASE)
        };
        assert_eq!(
            rig.coordinator
                .repositories_discover(&operator(), request)
                .await
                .map(drop),
            Err(Refusal::InvalidRequest),
            "{access} {file}"
        );
    }
    for (caller, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        assert_eq!(
            rig.coordinator
                .repositories_discover(&caller, discover_of(BASE))
                .await
                .map(drop),
            Err(refusal)
        );
        assert_eq!(
            rig.coordinator
                .repositories_add(&caller, add_of("x", &proto::Discovery::default()))
                .await
                .map(drop),
            Err(refusal)
        );
    }
    assert_eq!(fake.dials(), 0, "no request reached the origin");
}

/// Proves: `Add` of what `Discover` showed stores one entry that serves apt and macOS: the apt
/// root under the base, the descriptor's channels and components, the computed fingerprints, the
/// keyring and the descriptor byte for byte and the base; the audit entry `repository_put` names
/// the operator's reason; the registry's generation is up by one; `List` shows the platforms.
///
/// Catches: an entry that is not the one confirmed, or one that serves apt only.
#[tokio::test]
async fn add_stores_a_repository_serving_apt_and_macos() {
    let served = Served::of(1);
    let (rig, _fake, store) = rig_serving("add-stores", &served);
    let found = discover(&rig, BASE).await.expect("discovered");
    let done = add(&rig, add_of("origin", &found)).await.expect("added");
    let held = done.repository.expect("held");
    let stored = held.repository.clone().expect("an entry");
    assert_eq!(stored.name, "origin");
    assert_eq!(stored.kind, "apt");
    assert_eq!(stored.uri, "https://origin.test/apt");
    assert_eq!(stored.base_uri, BASE);
    assert_eq!(stored.suites, ["testing", "stable"]);
    assert_eq!(stored.components, ["main"]);
    assert_eq!(stored.fingerprints, found.fingerprints);
    assert_eq!(stored.keyring, keyring(1));
    assert_eq!(stored.descriptor, *served.descriptor.lock().expect("lock"));
    assert_eq!(held.generation, 1);
    assert_eq!(held.platforms, ["linux-x86_64", "macos-arm64"]);
    assert!(!held.builtin);
    assert_eq!(registry::check(&stored), Ok(()));
    let entry = done.entry.expect("an audit entry");
    assert_eq!(entry.action, "repository_put");
    assert!(
        entry
            .reason
            .expect("a reason")
            .starts_with("add it: origin https://origin.test/apt suites testing,stable")
    );
    assert_eq!(store.desired().repositories.generation, 1);
    assert_eq!(store.audit().len(), 1);
    let listed = rig
        .coordinator
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed");
    let shown = listed
        .repositories
        .iter()
        .find(|r| r.repository.as_ref().is_some_and(|r| r.name == "origin"))
        .expect("listed");
    assert_eq!(shown.platforms, ["linux-x86_64", "macos-arm64"]);
    assert!(
        shown
            .repository
            .as_ref()
            .expect("entry")
            .descriptor
            .is_empty()
    );
}

/// Proves: `Add` refuses `descriptor_changed`, and stores nothing, when the descriptor changed
/// since `Discover` (a channel added), when the key changed (another keyring and descriptor), when
/// the confirmed fingerprints are not the keyring's (fewer, others), and when the digest is none.
///
/// Catches: a key or a descriptor the operator did not confirm stored: the gap between look and
/// commit.
#[tokio::test]
async fn add_refuses_when_what_it_reads_is_not_what_was_confirmed() {
    let served = Served::of(1);
    let (rig, _fake, store) = rig_serving("add-changed", &served);
    let found = discover(&rig, BASE).await.expect("discovered");
    let before = (store.desired().repositories, store.audit().len());

    // A channel added.
    let mut changed: Value =
        serde_json::from_slice(&served.descriptor.lock().expect("lock")).expect("json");
    changed["channels"] = json!(["testing", "stable", "edge"]);
    *served.descriptor.lock().expect("lock") = serde_json::to_vec(&changed).expect("json");
    assert_eq!(
        add(&rig, add_of("origin", &found)).await.map(drop),
        Err(Refusal::DescriptorChanged)
    );

    // The key changed, descriptor and keyring both.
    let again = Served::of(2);
    *served.descriptor.lock().expect("lock") = again.descriptor.lock().expect("lock").clone();
    *served.keyring.lock().expect("lock") = keyring(2);
    assert_eq!(
        add(&rig, add_of("origin", &found)).await.map(drop),
        Err(Refusal::DescriptorChanged)
    );

    // Back to what was confirmed, but confirmed with other fingerprints, or none, or no digest.
    *served.descriptor.lock().expect("lock") = descriptor(&fingerprint(&keyring(1)));
    *served.keyring.lock().expect("lock") = keyring(1);
    for (digest, prints) in [
        (
            found.descriptor_sha256.clone(),
            vec![fingerprint(&keyring(2))],
        ),
        (found.descriptor_sha256.clone(), Vec::new()),
        (String::new(), found.fingerprints.clone()),
    ] {
        let request = proto::AddRepositoryRequest {
            descriptor_sha256: digest,
            fingerprints: prints,
            ..add_of("origin", &found)
        };
        assert_eq!(
            add(&rig, request).await.map(drop),
            Err(Refusal::DescriptorChanged)
        );
    }
    assert_eq!((store.desired().repositories, store.audit().len()), before);
    add(&rig, add_of("origin", &found))
        .await
        .expect("what was confirmed is taken");
}

/// Proves: `Add` of the name `komira` with a key other than the pinned one is refused `builtin`
/// and changes nothing, while the pinned key is taken under that name (it is the override that
/// serves the same key), and a name that is no repository name or a reason that is empty is
/// `invalid_request`.
///
/// Catches: the built-in repository's name taken by another key through the form that asks only
/// for a URL.
#[tokio::test]
async fn add_of_komira_with_another_key_is_refused_builtin() {
    let served = Served::of(1);
    let (rig, _fake, store) = rig_serving("add-komira", &served);
    let found = discover(&rig, BASE).await.expect("discovered");
    let before = (store.desired().repositories, store.audit().len());
    assert_eq!(
        add(&rig, add_of("komira", &found)).await.map(drop),
        Err(Refusal::Builtin)
    );
    for bad in [
        add_of("Bad Name", &found),
        proto::AddRepositoryRequest {
            reason: String::new(),
            ..add_of("origin", &found)
        },
    ] {
        assert_eq!(add(&rig, bad).await.map(drop), Err(Refusal::InvalidRequest));
    }
    assert_eq!((store.desired().repositories, store.audit().len()), before);

    let ring = crate::builtin::komira().repository.keyring.clone();
    *served.descriptor.lock().expect("lock") = descriptor(crate::builtin::PINNED[0]);
    *served.keyring.lock().expect("lock") = ring;
    let pinned = discover(&rig, BASE).await.expect("discovered");
    let done = add(&rig, add_of("komira", &pinned))
        .await
        .expect("the pinned key is taken");
    assert!(!done.repository.expect("held").builtin);
}

/// Proves: `Add` goes through the code of `Put`: a channel that is another repository's is
/// `conflict`, a seventeenth repository is `capacity`, and a commit that fails is `store_failed`,
/// each with the registry and the audit as they were.
///
/// Catches: an `Add` that skips a refusal `Put` makes.
#[tokio::test]
async fn add_is_refused_as_put_is() {
    let served = Served::of(1);
    let (rig, fake, store) = rig_serving("add-as-put", &served);
    let found = discover(&rig, BASE).await.expect("discovered");
    // `a-b` + `testing` and `a` + `b-testing` are one channel, `a-b-testing`.
    let clash =
        crate::repositories_support::repository("a", "https://c.example.com/", &["b-testing"], 5);
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(clash),
                reason: "r".to_owned(),
            },
        )
        .expect("put");
    let before = (store.desired().repositories, store.audit().len());
    assert_eq!(
        add(&rig, add_of("a-b", &found)).await.map(drop),
        Err(Refusal::Conflict)
    );
    assert_eq!((store.desired().repositories, store.audit().len()), before);
    let _ = fake;

    // The registry full.
    let (rig, _fake, _store) = rig_serving("add-capacity", &served);
    for n in 0..16u8 {
        let name = format!("repo-{n}");
        let entry = crate::repositories_support::repository(
            &name,
            "https://r.example.com/",
            &[&format!("s{n}")],
            n,
        );
        rig.coordinator
            .repositories_put(
                &operator(),
                proto::PutRepositoryRequest {
                    repository: Some(entry),
                    reason: "r".to_owned(),
                },
            )
            .expect("put");
    }
    let found = discover(&rig, BASE).await.expect("discovered");
    assert_eq!(
        add(&rig, add_of("origin", &found)).await.map(drop),
        Err(Refusal::Capacity)
    );

    // A store that cannot commit.
    let (rig, _fake, store) = rig_serving("add-store-fails", &served);
    let found = discover(&rig, BASE).await.expect("discovered");
    store.refuse(move |_| Some(crate::store::store_failure()));
    assert_eq!(
        add(&rig, add_of("origin", &found)).await.map(drop),
        Err(Refusal::StoreFailed)
    );
}

/// Proves: a private repository is read with the credential's exact line as `Authorization`,
/// stored with its access and credential file, its suites the host groups that exist (a private
/// repository's suites are groups'), and no macOS platform; and the credential is in no log line
/// and no answer, whether the read succeeds or fails.
///
/// Catches: a credential not sent, sent to a public read, logged, or a private repository's
/// macOS format served.
#[tokio::test]
async fn add_private_reads_with_the_credential_and_never_logs_it() {
    let served = Served::of(1);
    let (rig, fake, store) = rig_serving("add-private", &served);
    let file = credential_in(rig.dir.path(), "private-credential", &format!("{SECRET}\n"));
    let log = Log::default();
    let _guard = log.install();
    // A private repository's suites are host groups': with none there is no suite to serve.
    let discovered = proto::DiscoverRepositoryRequest {
        access: "hosts".to_owned(),
        credential_file: file.clone(),
        ..discover_of(BASE)
    };
    let seen = rig
        .coordinator
        .repositories_discover(&operator(), discovered)
        .await
        .expect("discovered");
    let shown = seen.discovery.expect("a discovery");
    let none = proto::AddRepositoryRequest {
        access: "hosts".to_owned(),
        credential_file: file.clone(),
        ..add_of("priv", &shown)
    };
    assert_eq!(
        add(&rig, none).await.map(drop),
        Err(Refusal::InvalidRequest)
    );
    rig.coordinator
        .host_groups_put(
            &operator(),
            crate::host_groups_support::put_of(
                crate::host_groups_support::body(
                    "alpha",
                    &[crate::host_groups_support::SELF],
                    None,
                ),
                None,
            ),
        )
        .expect("a group");
    let request = proto::DiscoverRepositoryRequest {
        access: "hosts".to_owned(),
        credential_file: file.clone(),
        ..discover_of(BASE)
    };
    let found = rig
        .coordinator
        .repositories_discover(&operator(), request)
        .await
        .expect("discovered")
        .discovery
        .expect("a discovery");
    assert_eq!(found.platforms, ["linux-x86_64"]);
    for head in fake.heard() {
        assert!(head.contains(&format!("authorization: {SECRET}")), "{head}");
    }
    let private = proto::AddRepositoryRequest {
        access: "hosts".to_owned(),
        credential_file: file.clone(),
        ..add_of("priv", &found)
    };
    let done = add(&rig, private).await.expect("added");
    let held = done.repository.expect("held");
    let stored = held.repository.expect("an entry");
    assert_eq!(
        (stored.access.as_str(), stored.credential_file.as_str()),
        ("hosts", file.as_str())
    );
    assert_eq!(stored.suites, ["alpha"]);
    assert_eq!(held.platforms, ["linux-x86_64"]);
    assert_eq!(store.desired().repositories.generation, 1);

    // An unusable credential file, and an origin that refuses.
    let bad = proto::DiscoverRepositoryRequest {
        credential_file: "/nonexistent".to_owned(),
        access: "hosts".to_owned(),
        ..discover_of(BASE)
    };
    assert_eq!(
        rig.coordinator
            .repositories_discover(&operator(), bad)
            .await
            .map(drop),
        Err(Refusal::CredentialUnusable)
    );
    let refusing = rig_over(
        "add-private-refused",
        &Fake::new(),
        &fake,
        fake.dial_all(Then::Write(response("403 Forbidden", b"nope"))),
    );
    let denied = proto::DiscoverRepositoryRequest {
        access: "hosts".to_owned(),
        credential_file: file,
        ..discover_of(BASE)
    };
    assert_eq!(
        refusing
            .coordinator
            .repositories_discover(&operator(), denied)
            .await
            .map(drop),
        Err(Refusal::DiscoveryFailed)
    );
    assert!(!log.text().contains("s3cret"), "{}", log.text());
}

/// The digest a host is sent for `rig`'s entry `name` (a public one, as a host of any group has
/// it): the digest of its rendered files.
fn host_digest(rig: &Rig, name: &str) -> String {
    let entries = rig
        .coordinator
        .desired
        .with_repositories(|held| held.entries_of([name]))
        .expect("held");
    let sent: Vec<_> = entries
        .iter()
        .filter_map(|entry| registry::for_host(entry, "web"))
        .collect();
    registry::digest(&sent)
}

/// Proves: an entry put before descriptors (an apt entry with no descriptor) that is added again
/// with the same name and base URL gains the descriptor, the base URL and the macOS platform and
/// keeps the digest of what its hosts are sent, because the apt root the descriptor names is the
/// one it had; the legacy entry before the add serves the two Linux platforms and nothing else.
///
/// Catches: a migration that changes every host's repository entry, or one that leaves a legacy
/// entry with no way to gain macOS.
#[tokio::test]
async fn add_over_a_legacy_entry_with_the_same_apt_root_keeps_the_host_digest() {
    let served = Served::of(1);
    let (rig, _fake, _store) = rig_serving("add-over-legacy", &served);
    let legacy = crate::repositories_support::repository(
        "origin",
        "https://origin.test/apt",
        &["testing", "stable"],
        1,
    );
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(legacy),
                reason: "before descriptors".to_owned(),
            },
        )
        .expect("put");
    let before = host_digest(&rig, "origin");
    let shown = rig
        .coordinator
        .repositories_get(
            &operator(),
            &proto::GetRepositoryRequest {
                name: "origin".to_owned(),
            },
        )
        .expect("got")
        .repository
        .expect("held");
    assert_eq!(shown.platforms, ["linux-x86_64", "linux-arm64"]);
    assert!(shown.repository.expect("entry").descriptor.is_empty());

    let found = discover(&rig, BASE).await.expect("discovered");
    let done = add(&rig, add_of("origin", &found)).await.expect("added");
    let held = done.repository.expect("held");
    assert_eq!(held.platforms, ["linux-x86_64", "macos-arm64"]);
    assert_eq!(held.generation, 2);
    assert_eq!(host_digest(&rig, "origin"), before);
}
