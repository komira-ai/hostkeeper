//! [`JsonMtlsLink`] over the in-process transport: whom it trusts, what answer it takes, where
//! it dials, and what it does with an update of the JSON route. Each connection is a real TLS
//! session with the server of `tls`.

use std::sync::Arc;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_link::{
    ANNOUNCED, Acknowledged, ClientIdentity, CoordinatorLink, EndpointError, Feature, HEAD_MAX,
    Heartbeat, JsonMtlsLink, LinkError, Order, OrderId, OrderRef, Platform, REPLY_MAX, Reason,
    Reply, ReplyFault, Target, TrustFault, Verb,
};

use crate::tls::{
    COORDINATOR, Ca, Close, Farm, PipeTransport, Pki, config_text, http, link, serve, server_config,
};
use crate::{service, version};

/// An empty heartbeat of a daemon at 0.9.0.
fn beat() -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: version("0.9.0"),
        features: ANNOUNCED.to_vec(),
        services: Vec::new(),
    }
}

/// A link of `farm` over a pki directory holding `bundle`, its transport and its log.
struct Bench {
    farm: Farm,
    transport: Arc<PipeTransport>,
    log: Arc<RecordingLog>,
    link: JsonMtlsLink,
    pki: Pki,
}

impl Bench {
    fn new(name: &str) -> Bench {
        let farm = Farm::new();
        let bundle = farm.ca.pem.clone();
        Bench::with_bundle(name, farm, &bundle)
    }

    fn with_bundle(name: &str, farm: Farm, bundle: &str) -> Bench {
        let pki = Pki::with_bundle(name, bundle);
        let transport = Arc::new(PipeTransport::default());
        let log = Arc::new(RecordingLog::default());
        let link = link(&farm, &pki, &transport, &log);
        Bench {
            farm,
            transport,
            log,
            link,
            pki,
        }
    }

    /// The link's heartbeat to a coordinator that answers `raw` and closes as `close` says;
    /// and whether the coordinator read the request.
    fn answered(&self, raw: Vec<u8>, close: Close) -> (Result<Reply, LinkError>, bool) {
        let server = serve(&self.transport, self.farm.server(), close, move |_| raw);
        let reply = self.link.heartbeat(&self.farm.host.identity(), &beat());
        let read = server.join().expect("the server").is_some();
        (reply, read)
    }

    /// The link's heartbeat to a server of `config`.
    fn to_server(&self, config: Arc<rustls::ServerConfig>) -> (Result<Reply, LinkError>, bool) {
        let server = serve(&self.transport, config, Close::Notify, |_| {
            http(200, r#"{"reply":"nothing"}"#)
        });
        let reply = self.link.heartbeat(&self.farm.host.identity(), &beat());
        let read = server.join().expect("the server").is_some();
        (reply, read)
    }
}

/// Catches a link that trusts more than the CA the daemon config pins: a server certificate of
/// another CA (even one in the bundle, unpinned), of the pinned CA for another name, or one that
/// is a client's is refused before a byte of the heartbeat is written; a bundle that holds no
/// certificate with the pinned SHA-256, or no bundle, dials nothing; and the pinned CA among
/// others in the bundle is the one trusted.
#[test]
fn the_mtls_link_refuses_a_server_that_does_not_chain_to_the_pinned_ca() {
    let other = Ca::new("another ca");
    let refused = Err(LinkError::Untrusted(TrustFault::ServerCertificate));

    let bench = Bench::new("link-untrusted-server");
    let foreign = server_config(&other.server(COORDINATOR), &bench.farm.ca);
    assert_eq!(bench.to_server(foreign), (refused.clone(), false));
    let elsewhere = server_config(&bench.farm.ca.server("elsewhere.test"), &bench.farm.ca);
    assert_eq!(bench.to_server(elsewhere), (refused.clone(), false));
    let impostor = server_config(&bench.farm.ca.client_for_coordinator_name(), &bench.farm.ca);
    assert_eq!(bench.to_server(impostor), (refused.clone(), false));
    assert_eq!(
        bench.to_server(bench.farm.server()),
        (Ok(Reply::default()), true)
    );

    let farm = Farm::new();
    let both = format!("{}{}", other.pem, farm.ca.pem);
    let bench = Bench::with_bundle("link-bundle-of-two", farm, &both);
    let foreign = server_config(&other.server(COORDINATOR), &bench.farm.ca);
    assert_eq!(bench.to_server(foreign), (refused, false));
    assert_eq!(
        bench.to_server(bench.farm.server()),
        (Ok(Reply::default()), true)
    );

    let farm = Farm::new();
    let unpinned = other.pem.clone();
    let bench = Bench::with_bundle("link-unpinned-bundle", farm, &unpinned);
    let identity = bench.farm.host.identity();
    assert_eq!(
        bench.link.heartbeat(&identity, &beat()),
        Err(LinkError::Untrusted(TrustFault::NoPinnedCa))
    );
    assert_eq!(bench.transport.dialed(), Vec::<(String, u16)>::new());

    let farm = Farm::new();
    let pki = Pki::empty("link-no-bundle");
    let transport = Arc::new(PipeTransport::default());
    let log = Arc::new(RecordingLog::default());
    let link = link(&farm, &pki, &transport, &log);
    assert_eq!(
        link.heartbeat(&farm.host.identity(), &beat()),
        Err(LinkError::Untrusted(TrustFault::NoPinnedCa))
    );
    assert_eq!(transport.dialed(), Vec::<(String, u16)>::new());

    let cases = [
        (
            TrustFault::NoPinnedCa,
            "the heartbeat is not sent: no certificate of the CA bundle has the SHA-256 the \
             daemon config pins",
        ),
        (
            TrustFault::ServerCertificate,
            "the heartbeat is not sent: what answered is not the coordinator: its certificate \
             is refused",
        ),
        (
            TrustFault::ClientIdentity,
            "the heartbeat is not sent: the client certificate and key are not a TLS identity",
        ),
    ];
    for (fault, text) in cases {
        assert_eq!(LinkError::Untrusted(fault).to_string(), text);
    }
}

/// Catches a link that keeps the roots of its first heartbeat: the CA bundle is read again at
/// every heartbeat of one link, so once `current` names a generation whose bundle holds no
/// certificate with the pinned SHA-256 the next heartbeat dials nothing, and once it names one
/// that holds it again the heartbeat after that is sent.
#[test]
fn the_ca_bundle_is_read_again_at_every_heartbeat() {
    let farm = Farm::new();
    let pinned = farm.ca.pem.clone();
    let bench = Bench::with_bundle("link-bundle-reread", farm, &pinned);
    let nothing = || http(200, r#"{"reply":"nothing"}"#);
    assert_eq!(
        bench.answered(nothing(), Close::Notify),
        (Ok(Reply::default()), true)
    );
    assert_eq!(bench.transport.dialed().len(), 1);

    let other = Ca::new("a later ca");
    bench.pki.switch_to(2, &other.pem);
    assert_eq!(
        bench.link.heartbeat(&bench.farm.host.identity(), &beat()),
        Err(LinkError::Untrusted(TrustFault::NoPinnedCa))
    );
    assert_eq!(bench.transport.dialed().len(), 1, "nothing is dialed");

    bench.pki.switch_to(3, &pinned);
    assert_eq!(
        bench.answered(nothing(), Close::Notify),
        (Ok(Reply::default()), true)
    );
    assert_eq!(bench.transport.dialed().len(), 2);
}

/// A body of exactly `len` bytes: the answer `nothing`, padded with JSON white space.
fn padded(len: usize) -> String {
    let answer = r#"{"reply":"nothing"}"#;
    format!("{answer}{}", " ".repeat(len - answer.len()))
}

/// Catches a reply read loosely or without a bound: a key the wire does not have, at the top
/// level, is refused as an unknown key, and one inside an acknowledgement makes the answer
/// malformed, while an empty list of acknowledgements is read; a body over [`REPLY_MAX`] (by its length or by what was
/// sent), a head over [`HEAD_MAX`], or more bytes than both is over the bound, and a body of
/// exactly [`REPLY_MAX`] is read; an answer that is not one HTTP response with one length and a
/// body of that length, or not one wire answer, is malformed; the coordinator's refusal is
/// named by its code; an answer that ends without the TLS close is read by its length; and no
/// answer at all is a connection failure.
#[test]
fn the_mtls_link_refuses_a_reply_with_an_unknown_key_or_over_the_bound() {
    let bench = Bench::new("link-replies");
    let unknown = Err(LinkError::BadReply(ReplyFault::UnknownKey));
    let over = Err(LinkError::BadReply(ReplyFault::OverBound));
    let malformed = Err(LinkError::BadReply(ReplyFault::Malformed));
    let refused = |code: &str| Err(LinkError::Refused(Reason::new(code).expect("a reason")));
    let nothing = Ok(Reply::default());
    let raw = |text: &str| text.as_bytes().to_vec();
    let mut long_head = http(200, r#"{"reply":"nothing"}"#);
    let pad = format!("x-pad: {}\r\n", "a".repeat(HEAD_MAX));
    long_head.splice(16..16, pad.into_bytes());
    let mut too_much = http(200, r#"{"reply":"nothing"}"#);
    too_much.extend(vec![b' '; HEAD_MAX + REPLY_MAX]);
    let mut not_text = http(200, r#"{"reply":"nothing"}"#);
    not_text[10] = 0xff;
    let cases: Vec<(&str, Vec<u8>, Result<Reply, LinkError>)> = vec![
        (
            "an unknown key",
            http(200, r#"{"reply":"nothing","extra":1}"#),
            unknown.clone(),
        ),
        (
            "an empty list of acknowledgements",
            http(200, r#"{"reply":"nothing","acks":[]}"#),
            nothing.clone(),
        ),
        (
            "an acknowledgement with a key the wire does not have",
            http(
                200,
                r#"{"reply":"nothing","acks":[{"service":"web","order_id":"o-1","order_try":1,"extra":1}]}"#,
            ),
            malformed.clone(),
        ),
        (
            "an order id without its try and digest",
            http(
                200,
                r#"{"reply":"update","service":"web","version":"2.0","order_id":"o-1"}"#,
            ),
            malformed.clone(),
        ),
        (
            "a body one byte over",
            http(200, &padded(REPLY_MAX + 1)),
            over.clone(),
        ),
        (
            "a body at the bound",
            http(200, &padded(REPLY_MAX)),
            nothing.clone(),
        ),
        ("a head over its bound", long_head, over.clone()),
        ("more bytes than any answer", too_much, over),
        (
            "forbidden",
            http(403, r#"{"error":"forbidden"}"#),
            refused("forbidden"),
        ),
        (
            "a bad request",
            http(400, r#"{"error":"bad_request"}"#),
            refused("bad_request"),
        ),
        (
            "a refusal that is no token",
            http(500, r#"{"error":"Not Ok"}"#),
            malformed.clone(),
        ),
        (
            "a refusal that is not JSON",
            http(500, "oops"),
            malformed.clone(),
        ),
        (
            "no blank line",
            raw("HTTP/1.1 200 OK\r\ncontent-length: 2"),
            malformed.clone(),
        ),
        (
            "another HTTP",
            raw("HTTP/1.0 200 OK\r\ncontent-length: 2\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "no status",
            raw("HTTP/1.1 2x0 OK\r\ncontent-length: 2\r\n\r\n{}"),
            malformed.clone(),
        ),
        ("a head that is not text", not_text, malformed.clone()),
        (
            "a header with no colon",
            raw("HTTP/1.1 200 OK\r\nbroken\r\ncontent-length: 2\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "a chunked body",
            raw("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\ncontent-length: 2\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "two lengths",
            raw("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nContent-Length: 2\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "no length",
            raw("HTTP/1.1 200 OK\r\nx: y\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "a length that is no number",
            raw("HTTP/1.1 200 OK\r\ncontent-length: two\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "a short body",
            raw("HTTP/1.1 200 OK\r\ncontent-length: 20\r\n\r\n{}"),
            malformed.clone(),
        ),
        (
            "a body that is not JSON",
            http(200, "nothing"),
            malformed.clone(),
        ),
        (
            "a body that is no object",
            http(200, r#"["nothing"]"#),
            malformed.clone(),
        ),
        (
            "a key twice",
            http(200, r#"{"reply":"nothing","reply":"nothing"}"#),
            malformed.clone(),
        ),
        (
            "no answer of the wire",
            http(200, r#"{"reply":"maybe"}"#),
            malformed,
        ),
        (
            "no answer at all",
            Vec::new(),
            Err(LinkError::Transport(std::io::ErrorKind::UnexpectedEof)),
        ),
    ];
    for (what, answer, expected) in cases {
        let (reply, read) = bench.answered(answer, Close::Notify);
        assert_eq!(reply, expected, "{what}");
        assert!(read, "{what}");
    }

    let (reply, read) = bench.answered(http(200, r#"{"reply":"nothing"}"#), Close::Drop);
    assert_eq!((reply, read), (nothing, true), "no TLS close");
    assert_eq!(
        bench.link.heartbeat(&bench.farm.host.identity(), &beat()),
        Err(LinkError::Transport(std::io::ErrorKind::ConnectionRefused)),
        "nothing answers"
    );
}

/// A link to `coordinator`, and what its transport dialed when it sent a heartbeat (with no
/// one answering).
fn dialed(name: &str, coordinator: &str) -> Vec<(String, u16)> {
    let farm = Farm::new();
    let pki = Pki::with_bundle(name, &farm.ca.pem);
    let config = DaemonConfig::parse(&config_text(coordinator, &farm.ca.sha256_hex()))
        .expect("a daemon config");
    let transport = Arc::new(PipeTransport::default());
    let log = Arc::new(RecordingLog::default());
    let link = JsonMtlsLink::new(&config, &pki.root(), transport.clone(), log).expect("a link");
    assert_eq!(
        link.heartbeat(&farm.host.identity(), &beat()),
        Err(LinkError::Transport(std::io::ErrorKind::ConnectionRefused))
    );
    transport.dialed()
}

/// Catches a link that dials another host or port than the daemon config names, drops a port,
/// keeps the brackets of an address, or posts under a path it was not given: the host and port
/// of the config are dialed, 443 when it names none; a path is refused, and so is a host no
/// certificate can name.
#[test]
fn the_link_dials_the_host_and_port_of_the_daemon_config() {
    let cases = [
        ("https://coordinator.test:8443", ("coordinator.test", 8443)),
        ("https://coordinator.test", ("coordinator.test", 443)),
        ("https://coordinator.test/", ("coordinator.test", 443)),
        ("https://127.0.0.1:9443", ("127.0.0.1", 9443)),
        ("https://[::1]:7443", ("::1", 7443)),
        ("https://[::1]", ("::1", 443)),
    ];
    for (n, (coordinator, (host, port))) in cases.into_iter().enumerate() {
        assert_eq!(
            dialed(&format!("link-dial-{n}"), coordinator),
            [(host.to_owned(), port)],
            "{coordinator}"
        );
    }

    let ca = Ca::new("unused");
    let pki = Pki::empty("link-dial-refused");
    let refused = |coordinator: &str| {
        let config = DaemonConfig::parse(&config_text(coordinator, &ca.sha256_hex()))
            .expect("a daemon config");
        JsonMtlsLink::new(
            &config,
            &pki.root(),
            Arc::new(PipeTransport::default()),
            Arc::new(RecordingLog::default()),
        )
        .err()
    };
    let path = "https://coordinator.test/base";
    assert_eq!(refused(path), Some(EndpointError::Path(path.to_owned())));
    assert_eq!(
        EndpointError::Path(path.to_owned()).to_string(),
        "the coordinator address \"https://coordinator.test/base\" has a path: the heartbeat \
         goes to /v1/heartbeat at its root"
    );
    let host = "https://bad..example.com:8443";
    assert_eq!(refused(host), Some(EndpointError::Host(host.to_owned())));
    assert_eq!(
        EndpointError::Host(host.to_owned()).to_string(),
        "the coordinator address \"https://bad..example.com:8443\" has no host a certificate can be \
         checked against"
    );
}

/// Catches an update run without the terms every order must carry, or one that is dropped
/// without a word, or logged at every heartbeat: an update with no terms (a coordinator that
/// sends none) is not given to the loop and is logged once, again when it changes or comes back
/// after a reply without one; an update of the coordinator's legacy service names no service
/// and says so. Also what the reply does carry: the features the daemon knows, and the halt
/// generations of the services it could run.
#[test]
fn an_update_without_its_terms_is_not_run_and_is_logged_once_per_change() {
    let bench = Bench::new("link-updates");
    let update = |to: &str| {
        http(
            200,
            &format!(
                r#"{{"reply":"update","service":"web","version":"{to}","features":["deferred-v1","halt-v1","heartbeat-v2","forced-v1","verdict-acks-v1","order-v1","unknown-v9"],"halts":{{"web":3,"-x":1}}}}"#
            ),
        )
    };
    let expected = Reply {
        order: None,
        features: ANNOUNCED.to_vec(),
        halt_generations: vec![(service("web"), 3)],
        acknowledged: Vec::new(),
    };
    let line = |whom: &str, to: &str, why: &str| {
        format!("error: the coordinator's update of {whom} to {to} is not run: {why}")
    };
    let no_terms = "it carries no order id, try or package digest";
    let script = [
        (update("2.0"), Some(line("web", "2.0", no_terms))),
        (update("2.0"), None),
        (update("2.1"), Some(line("web", "2.1", no_terms))),
        (
            http(200, r#"{"reply":"nothing","features":[],"halts":{}}"#),
            None,
        ),
        (update("2.1"), Some(line("web", "2.1", no_terms))),
        (
            http(200, r#"{"reply":"update","version":"3.0"}"#),
            Some(line(
                "the coordinator's legacy service",
                "3.0",
                "it names no service",
            )),
        ),
    ];
    let mut lines = Vec::new();
    for (n, (answer, logged)) in script.into_iter().enumerate() {
        let carries_facts = n != 3 && n != 5;
        let (reply, read) = bench.answered(answer, Close::Notify);
        assert!(read);
        let want = if carries_facts {
            expected.clone()
        } else {
            Reply::default()
        };
        assert_eq!(reply, Ok(want), "answer {n}");
        lines.extend(logged);
        assert_eq!(bench.log.lines(), lines, "answer {n}");
    }
}

/// An update answer of `service` to `version` with the terms `id`, `order_try` and the digest
/// of the tests.
fn with_terms(service: &str, version: &str, id: &str, order_try: u64) -> Vec<u8> {
    http(
        200,
        &format!(
            r#"{{"reply":"update","service":"{service}","version":"{version}","order_id":"{id}","order_try":{order_try},"package_digest":"{}"}}"#,
            crate::digest().as_str()
        ),
    )
}

/// Catches an update with its terms not run, run under another id, try or digest, or run with a
/// word the daemon's grammars refuse: the answer's service, version, order id, try and package
/// digest are the loop's update order, with nothing logged; terms whose order id, version or
/// service the daemon cannot read (the wire's grammars are wider there) are not run, and are
/// logged with why.
#[test]
fn an_update_with_its_terms_is_the_loops_update_order() {
    let bench = Bench::new("link-update-terms");
    let (reply, read) = bench.answered(with_terms("web", "2.0", "update-0a1b", 3), Close::Notify);
    assert!(read);
    assert_eq!(
        reply,
        Ok(Reply {
            order: Some(Order {
                id: OrderId::new("update-0a1b").expect("an order id"),
                order_try: 3,
                verb: Verb::Update(Target {
                    service: service("web"),
                    version: version("2.0"),
                    digest: crate::digest(),
                }),
            }),
            ..Reply::default()
        })
    );
    assert_eq!(bench.log.lines(), Vec::<String>::new());

    let not_read = "its service, version, order id or digest is not one the daemon reads";
    let cases = [
        ("web", "2.0", ".hidden", "web", "2.0"),
        ("web", "2.0~rc1", "update-1", "web", "2.0~rc1"),
        ("-web", "2.0", "update-1", "-web", "2.0"),
    ];
    for (n, (name, to, id, whom, logged)) in cases.into_iter().enumerate() {
        let (reply, read) = bench.answered(with_terms(name, to, id, 1), Close::Notify);
        assert!(read);
        assert_eq!(reply, Ok(Reply::default()), "{id} {to} {name}");
        assert_eq!(
            bench.log.lines()[n],
            format!("error: the coordinator's update of {whom} to {logged} is not run: {not_read}")
        );
    }
}

/// Catches a JSON answer's acknowledgements lost on the way to the loop, which leaves a daemon
/// that a `verdict-acks-v1` coordinator answers resending its oldest verdict for ever: each ack
/// is the loop's acknowledgement of that service's order id and try, in the answer's order; an
/// ack whose service or order id the daemon's grammars refuse (the wire's are wider) names no
/// verdict the daemon sent and is dropped, the others kept; an answer with no acks has none.
#[test]
fn the_answers_acks_are_the_loops_acknowledgements() {
    let bench = Bench::new("link-acks");
    let (reply, read) = bench.answered(
        http(
            200,
            r#"{"reply":"nothing","features":["verdict-acks-v1"],"acks":[{"service":"web","order_id":"update-1","order_try":2},{"service":"web","order_id":".hidden","order_try":1},{"service":"-web","order_id":"update-1","order_try":1},{"service":"db","order_id":"force-1","order_try":1}]}"#,
        ),
        Close::Notify,
    );
    assert!(read);
    let ack = |name: &str, id: &str, order_try: u64| Acknowledged {
        service: service(name),
        order: OrderRef {
            id: OrderId::new(id).expect("an order id"),
            order_try,
        },
    };
    assert_eq!(
        reply,
        Ok(Reply {
            features: vec![Feature::VerdictAcksV1],
            acknowledged: vec![ack("web", "update-1", 2), ack("db", "force-1", 1)],
            ..Reply::default()
        })
    );

    let (reply, read) = bench.answered(
        http(200, r#"{"reply":"nothing","features":["verdict-acks-v1"]}"#),
        Close::Notify,
    );
    assert!(read);
    assert_eq!(
        reply,
        Ok(Reply {
            features: vec![Feature::VerdictAcksV1],
            ..Reply::default()
        })
    );
    assert_eq!(bench.log.lines(), Vec::<String>::new());
}

/// Catches a pair that is not a TLS identity sent anyway, and a refusal by the coordinator of
/// the host's certificate read as the coordinator not being trusted: a certificate or a key that
/// is not PEM, or a PEM key TLS cannot use, is refused before anything is dialed; a certificate of another CA reaches the
/// coordinator, which refuses it, and the link says the connection failed.
#[test]
fn a_client_pair_that_is_not_a_tls_identity_is_not_sent() {
    let bench = Bench::new("link-client-pair");
    let not_identity = Err(LinkError::Untrusted(TrustFault::ClientIdentity));
    let host = bench.farm.host.clone();
    let broken = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
    let pairs = [
        ClientIdentity::new(
            b"not a certificate".to_vec(),
            host.key_pem.clone().into_bytes(),
        ),
        ClientIdentity::new(
            broken.as_bytes().to_vec(),
            host.key_pem.clone().into_bytes(),
        ),
        ClientIdentity::new(host.cert_pem.clone().into_bytes(), b"not a key".to_vec()),
        ClientIdentity::new(
            host.cert_pem.clone().into_bytes(),
            b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".to_vec(),
        ),
    ];
    for pair in pairs {
        assert_eq!(bench.link.heartbeat(&pair, &beat()), not_identity);
    }
    assert_eq!(bench.transport.dialed(), Vec::<(String, u16)>::new());

    let stranger = Ca::new("a stranger's ca").host("d1", "daemon");
    let server = serve(&bench.transport, bench.farm.server(), Close::Notify, |_| {
        http(200, r#"{"reply":"nothing"}"#)
    });
    assert_eq!(
        bench.link.heartbeat(&stranger.identity(), &beat()),
        Err(LinkError::Transport(std::io::ErrorKind::InvalidData))
    );
    assert!(server.join().expect("the server").is_none());
}
