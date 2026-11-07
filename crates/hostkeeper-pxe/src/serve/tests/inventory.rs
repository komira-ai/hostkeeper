//! The read of the coordinator: the snapshot of its three lists, every page of each, with the
//! token, over TLS that trusts the coordinator's CA alone; and every way a read fails.

use std::collections::BTreeSet;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::fake::{self, Coordinator as FakeCoordinator, Fake, host, pending, pki, record};
use crate::serve::inventory::{
    ConnectError, Coordinator, MAX_PAGES, Pending, ReadError, Record, Snapshot,
};

const TOKEN: &str = "operator-token-0123456789abcdef-0123456789";

/// Runs `body` on a runtime, where a [`Coordinator`] may be made.
fn on_runtime<T>(body: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(body)
}

/// The snapshot `fake` answers, read with the CA `ca`.
fn read(endpoint: &str, ca: &str) -> Result<Snapshot, ReadError> {
    on_runtime(async {
        Coordinator::new(endpoint, ca.as_bytes(), TOKEN)
            .expect("a client")
            .snapshot()
            .await
    })
}

/// A snapshot keeps each record's host id, role (none when empty), MACs and enrolled mark,
/// with no platform (the field is not in the API yet); each host's node id; and, for each
/// pending request, the host ids of the records it matches: the candidates of AMBIGUOUS, then
/// the one of MATCHED.
///
/// Catches a pending request whose candidate records are forgotten, and a role read as `""`.
#[test]
fn a_snapshot_keeps_what_the_answer_set_needs() {
    let mut with_role = record("r-1", &["02:00:00:00:00:01"], true);
    with_role.role = "storage".to_owned();
    let ambiguous = proto::PendingEnrollment {
        request_id: "req-2".to_owned(),
        candidate_host_ids: vec!["a-1".to_owned(), "b-1".to_owned()],
        ..Default::default()
    };
    let snapshot = Snapshot::of(
        vec![with_role, record("m-1", &[], false)],
        vec![host("h-1"), host("h-2")],
        vec![pending("req-1", "r-1"), ambiguous],
    )
    .expect("a snapshot");
    assert_eq!(
        snapshot,
        Snapshot {
            records: vec![
                Record {
                    host_id: "r-1".parse().expect("an id"),
                    role: Some("storage".parse().expect("a role")),
                    data_macs: vec![MacAddress::parse("02:00:00:00:00:01").expect("a MAC")],
                    enrolled: true,
                    platform: None,
                },
                Record {
                    host_id: "m-1".parse().expect("an id"),
                    role: None,
                    data_macs: vec![],
                    enrolled: false,
                    platform: None,
                },
            ],
            hosts: BTreeSet::from(["h-1".to_owned(), "h-2".to_owned()]),
            pending: vec![
                Pending {
                    request_id: "req-1".to_owned(),
                    host_ids: vec!["r-1".to_owned()],
                },
                Pending {
                    request_id: "req-2".to_owned(),
                    host_ids: vec!["a-1".to_owned(), "b-1".to_owned()],
                },
            ],
        }
    );
}

/// A record whose host id, role or a data MAC is not one fails the whole read, naming it.
///
/// Catches a malformed answer read as a smaller inventory, which could leave an enrolled host's
/// record out of the checks.
#[test]
fn a_record_that_does_not_parse_fails_the_read() {
    let bad_id = record("Not An Id!", &[], false);
    let mut bad_role = record("r-1", &[], false);
    bad_role.role = "Not A Role!".to_owned();
    let bad_mac = record("r-2", &["zz"], false);
    let cases = [
        (bad_id, "the record host id \"Not An Id!\" is not a host id"),
        (
            bad_role,
            "the record r-1 names the role \"Not A Role!\", which is not a role",
        ),
        (bad_mac, "the record r-2 holds \"zz\", which is not a MAC"),
    ];
    for (record, what) in cases {
        let refused = Snapshot::of(vec![record], vec![], vec![]).expect_err(what);
        assert_eq!(
            refused,
            ReadError::Unparsable {
                rpc: "Enrollment.ListRecords",
                what: what.to_owned()
            }
        );
        assert_eq!(
            refused.to_string(),
            format!("Enrollment.ListRecords: {what}")
        );
    }
}

/// `--coordinator` must be `https://<host>[:<port>]` and nothing more, and a token that cannot
/// be a header is refused; a well-formed endpoint (an IP address or a name, with or without a
/// port) makes a client without connecting.
///
/// Catches a plain-HTTP coordinator that would carry the operator token in clear, and a path or
/// a user that would be silently dropped.
#[test]
fn the_endpoint_is_an_https_authority_and_nothing_more() {
    let ca = pki().ca;
    on_runtime(async {
        for endpoint in [
            "http://127.0.0.1:1",
            "https://127.0.0.1:1/v1",
            "https://user@127.0.0.1:1",
            "https://127.0.0.1:1?x=1",
            "not a url",
            "127.0.0.1:1",
        ] {
            assert_eq!(
                Coordinator::new(endpoint, ca.as_bytes(), TOKEN).map(|_| ()),
                Err(ConnectError::Endpoint(endpoint.to_owned())),
                "{endpoint}"
            );
        }
        assert_eq!(
            ConnectError::Endpoint("http://x".to_owned()).to_string(),
            "--coordinator \"http://x\" is not https://<host>[:<port>]"
        );
        for endpoint in [
            "https://127.0.0.1:8443",
            "https://coordinator.example",
            "https://[::1]:8443",
        ] {
            assert!(
                Coordinator::new(endpoint, ca.as_bytes(), TOKEN).is_ok(),
                "{endpoint}"
            );
        }
        assert_eq!(
            Coordinator::new("https://127.0.0.1:1", ca.as_bytes(), "a\ntoken").map(|_| ()),
            Err(ConnectError::Tls {
                endpoint: "https://127.0.0.1:1".to_owned(),
                why: "the token cannot be sent as a header".to_owned()
            })
        );
    });
}

/// Every page of each of the three lists is read, in order, each call with the token as
/// `Bearer <token>`: the records, then the hosts, then the pending requests.
///
/// Catches an inventory read that stops at its first page (a record past it would escape the
/// enrolled check) and calls sent without the operator token.
#[test]
fn every_page_of_each_list_is_read_with_the_token() {
    let pki = pki();
    let fake = Fake::default();
    fake.set(|s| {
        s.records = vec![
            record("a-1", &["02:00:00:00:00:01"], false),
            record("b-1", &["02:00:00:00:00:02"], false),
            record("c-1", &["02:00:00:00:00:03"], false),
        ];
        s.hosts = vec![host("h-1"), host("h-2")];
        s.pending = vec![pending("req-1", "a-1"), pending("req-2", "b-1")];
        s.page = 1;
    });
    let coordinator = FakeCoordinator::start(&pki, fake);
    let snapshot = read(&coordinator.endpoint(), &pki.ca).expect("a snapshot");
    let ids: Vec<&str> = snapshot
        .records
        .iter()
        .map(|r| r.host_id.as_str())
        .collect();
    assert_eq!(ids, ["a-1", "b-1", "c-1"]);
    assert_eq!(
        snapshot.hosts,
        BTreeSet::from(["h-1".to_owned(), "h-2".to_owned()])
    );
    let requests: Vec<&str> = snapshot
        .pending
        .iter()
        .map(|p| p.request_id.as_str())
        .collect();
    assert_eq!(requests, ["req-1", "req-2"]);
    let tokens = coordinator.fake.get(|s| s.tokens.clone());
    assert_eq!(tokens, ["", "1", "2", "", "1", "", "1"]);
    let bearers = coordinator.fake.get(|s| s.bearers.clone());
    assert_eq!(bearers, vec![format!("Bearer {TOKEN}"); 7]);
}

/// A call the coordinator refuses fails the read with the call's name, code and message, and
/// nothing after it is called; an answer that does not parse fails it too.
///
/// Catches a refused token read as an empty inventory that is then answered from.
#[test]
fn a_refused_call_or_an_unparsable_answer_fails_the_read() {
    let pki = pki();
    let fake = Fake::default();
    fake.set(|s| s.refuse = Some((Code::Unauthenticated, "unauthorized".to_owned())));
    let coordinator = FakeCoordinator::start(&pki, fake);
    let refused = read(&coordinator.endpoint(), &pki.ca).expect_err("refused");
    assert_eq!(
        refused,
        ReadError::Call {
            rpc: "Enrollment.ListRecords",
            code: Code::Unauthenticated,
            message: "unauthorized".to_owned()
        }
    );
    assert_eq!(
        refused.to_string(),
        "Enrollment.ListRecords: Unauthenticated: unauthorized"
    );
    assert_eq!(coordinator.fake.get(|s| s.tokens.len()), 1);
    coordinator.fake.set(|s| {
        s.refuse = None;
        s.records = vec![record("r-1", &["not-a-mac"], false)];
    });
    assert_eq!(
        read(&coordinator.endpoint(), &pki.ca),
        Err(ReadError::Unparsable {
            rpc: "Enrollment.ListRecords",
            what: "the record r-1 holds \"not-a-mac\", which is not a MAC".to_owned()
        })
    );
}

/// A list whose pages never end is refused after [`MAX_PAGES`] pages, naming it.
///
/// Catches a read that loops for ever on a coordinator whose page tokens never end.
#[test]
fn a_list_that_never_ends_is_refused() {
    let pki = pki();
    let fake = Fake::default();
    fake.set(|s| s.endless = true);
    let coordinator = FakeCoordinator::start(&pki, fake);
    let refused = read(&coordinator.endpoint(), &pki.ca).expect_err("endless");
    assert_eq!(
        refused,
        ReadError::TooManyPages {
            rpc: "Enrollment.ListRecords"
        }
    );
    assert_eq!(
        refused.to_string(),
        format!("Enrollment.ListRecords: no last page within {MAX_PAGES} pages")
    );
    assert_eq!(coordinator.fake.get(|s| s.tokens.len()), MAX_PAGES);
}

/// A coordinator that does not listen, and one whose certificate another CA signed, fail the
/// read at its first call, `Unavailable`; the second is never sent the token.
///
/// Catches a read that trusts any certificate, which would hand the operator token to whoever
/// answers on the address.
#[test]
fn no_answer_or_another_ca_fails_the_read() {
    let pki = pki();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = closed.local_addr().expect("its address").port();
    drop(closed);
    let refused = read(&format!("https://127.0.0.1:{port}"), &pki.ca).expect_err("no answer");
    assert!(
        matches!(
            refused,
            ReadError::Call {
                rpc: "Enrollment.ListRecords",
                code: Code::Unavailable,
                ..
            }
        ),
        "{refused:?}"
    );
    let coordinator = FakeCoordinator::start(&pki, Fake::default());
    let other = fake::pki();
    let refused = read(&coordinator.endpoint(), &other.ca).expect_err("another CA");
    assert!(
        matches!(
            refused,
            ReadError::Call {
                rpc: "Enrollment.ListRecords",
                code: Code::Unavailable,
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(coordinator.fake.get(|s| s.bearers.len()), 0);
}
