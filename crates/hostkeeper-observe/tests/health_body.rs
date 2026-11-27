//! The first bytes of the body of a health URL's answer that is not 2xx, from the probe to the
//! observation: the HTTP probe reading them over the in-memory transport, and the observer
//! keeping the body of the first failing instance as bounded text. No socket is opened.

use std::io::ErrorKind;
use std::time::Duration;

use hostkeeper_observe::fake::{
    Chunk, FakeConnect, FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::health::{MAX_BODY_BYTES, MAX_REPLY_BYTES};
use hostkeeper_observe::{
    ActiveState, Current, Deadline, HealthClient, HealthReply, HealthUrl, HealthView,
    HttpHealthClient, ObserveOptions, Observer, Ports, ServiceInput, ServiceState, UnhealthyReason,
};
use hostkeeper_proto::health::MAX_HEALTH_BODY;

const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(1),
};
const A: &str = "js-agent@9301.service";
const B: &str = "js-agent@9302.service";
const URL_A: &str = "http://127.0.0.1:9301/healthz";
const URL_B: &str = "http://127.0.0.1:9302/healthz";

fn bytes(text: &str) -> Chunk {
    Chunk::Bytes(text.as_bytes().to_vec())
}

/// What the probe answers for a peer that plays `chunks`.
fn probed(chunks: Vec<Chunk>) -> (Result<HealthReply, hostkeeper_observe::ObserveError>, usize) {
    let peer = FakeConnect::default();
    peer.reply(chunks);
    let url = HealthUrl::parse(URL_A).expect("a valid URL");
    let answer = HttpHealthClient::new(peer.clone()).get(&url, DEADLINE);
    (answer, peer.served())
}

/// Proves the probe keeps the body of an answer that is not 2xx, read past the header block
/// even when it comes in pieces, until the peer closes: a job agent's `503 not placeable`.
///
/// Catches: the body left unread because the probe stops at the end of the header block, which
/// keeps the reason of an unhealthy service from the coordinator.
#[test]
fn the_probe_keeps_the_body_of_a_503() {
    let (answer, served) = probed(vec![
        bytes("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 31\r\n\r\n"),
        bytes("503 not placeable: "),
        bytes("memory short"),
    ]);
    assert_eq!(
        answer,
        Ok(HealthReply {
            status: 503,
            body_bytes_read: 31,
            body: b"503 not placeable: memory short".to_vec(),
        })
    );
    assert_eq!(
        served,
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 31\r\n\r\n".len() + 31
    );
}

/// Proves the probe reads a body that is not 2xx to [`MAX_BODY_BYTES`] (the heartbeat's bound,
/// 160) and no further, keeps those bytes only, and reads a 2xx answer's body not at all; a 3xx's
/// body is kept like any other that is not 2xx.
///
/// Catches: a body read without bound, or one past the heartbeat's bound kept.
#[test]
fn the_probe_reads_at_most_160_bytes_of_a_body_and_none_of_a_2xx() {
    assert_eq!(MAX_BODY_BYTES, 160);
    assert_eq!(MAX_BODY_BYTES, MAX_HEALTH_BODY);
    let head = "HTTP/1.1 500 Internal Server Error\r\n\r\n";
    let (answer, served) = probed(vec![
        bytes(head),
        Chunk::Bytes(vec![b'a'; 100]),
        Chunk::Bytes(vec![b'b'; 100]),
        bytes("never read"),
    ]);
    let mut kept = vec![b'a'; 100];
    kept.extend_from_slice(&[b'b'; 60]);
    assert_eq!(
        answer,
        Ok(HealthReply {
            status: 500,
            body_bytes_read: 200,
            body: kept,
        })
    );
    assert_eq!(served, head.len() + 200);

    let (answer, served) = probed(vec![
        bytes("HTTP/1.1 200 OK\r\n\r\n"),
        bytes("ok, and more"),
    ]);
    assert_eq!(
        answer,
        Ok(HealthReply {
            status: 200,
            body_bytes_read: 0,
            body: Vec::new(),
        })
    );
    assert_eq!(served, "HTTP/1.1 200 OK\r\n\r\n".len());

    let (answer, _) = probed(vec![bytes(
        "HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\n\r\nmoved",
    )]);
    assert_eq!(answer.map(|reply| reply.body), Ok(b"moved".to_vec()));

    // One chunk that holds the header block and a long body: what is kept is still 160 bytes.
    let mut flood = b"HTTP/1.1 503 Busy\r\n\r\n".to_vec();
    flood.resize(64 * 1024, b'x');
    let (answer, served) = probed(vec![Chunk::Bytes(flood)]);
    assert_eq!(
        answer.map(|reply| reply.body),
        Ok(vec![b'x'; MAX_BODY_BYTES])
    );
    assert_eq!(served, MAX_REPLY_BYTES);
}

/// Proves a body that ends early is kept as far as it came: a peer that closes, or a read that
/// fails, after part of the body leaves the answer and the part; one that fails before any
/// body keeps none; and a header block that never ends keeps none.
///
/// Catches: a failed read after the status line turned into a failed probe, which would make a
/// service that answered 503 read as not answering at all.
#[test]
fn a_body_that_ends_early_is_kept_as_far_as_it_came() {
    let head = "HTTP/1.1 503 Service Unavailable\r\n\r\n";
    let cases = [
        (
            vec![bytes(head), bytes("not placea")],
            b"not placea".to_vec(),
        ),
        (
            vec![bytes(head), bytes("not"), Chunk::Fail(ErrorKind::TimedOut)],
            b"not".to_vec(),
        ),
        (
            vec![bytes(head), Chunk::Fail(ErrorKind::ConnectionReset)],
            Vec::new(),
        ),
        (vec![bytes(head)], Vec::new()),
        (
            vec![
                bytes(head),
                Chunk::Fail(ErrorKind::Interrupted),
                bytes("again"),
            ],
            b"again".to_vec(),
        ),
        (
            vec![
                bytes("HTTP/1.1 503 Busy\r\nRetry-After: 5\r\n"),
                bytes("body?"),
            ],
            Vec::new(),
        ),
    ];
    for (n, (chunks, body)) in cases.into_iter().enumerate() {
        let (answer, _) = probed(chunks);
        assert_eq!(
            answer.map(|reply| (reply.status, reply.body)),
            Ok((503, body)),
            "case {n}"
        );
    }
}

/// An observer over fakes whose health client is `health`.
fn observer(manager: &FakeServiceManager, health: &FakeHealthClient) -> Observer {
    let ports = Ports {
        manager: Box::new(manager.clone()),
        process: Box::new(FakeProcessProbe::default()),
        health: Box::new(health.clone()),
        expected: Box::new(FakeExpectedExe::default()),
    };
    Observer::new(ports, ObserveOptions::default())
}

fn js_agent(run: &[&str]) -> ServiceInput {
    ServiceInput {
        service: "js-agent".to_owned(),
        current: Some(Current {
            run: run.iter().map(|unit| (*unit).to_owned()).collect(),
            health_url: Some("http://127.0.0.1:{port}/healthz".to_owned()),
        }),
        recorded_version: Some("3.2.0".to_owned()),
    }
}

/// Proves a live service whose health URL answered 503 carries the body of that answer as text,
/// with its trailing newline trimmed, beside `status_non_2xx`; with two instances failing, the
/// first one's in run order; a service healthy again carries none.
///
/// Catches: the body dropped between the probe and the observation, or the second instance's
/// body standing for the service.
#[test]
fn a_live_service_answering_503_carries_its_body() {
    let (manager, health) = (FakeServiceManager::default(), FakeHealthClient::default());
    manager.set(A, ActiveState::Active, Some(11));
    manager.set(B, ActiveState::Active, Some(12));
    health.answer_body(URL_B, 500, b"second");
    health.answer_body(URL_A, 503, b"503 not placeable: memory short\n");
    let observer = observer(&manager, &health);
    let seen = observer.observe(&js_agent(&[A, B]), DEADLINE);
    assert_eq!(seen.state, ServiceState::Live);
    assert_eq!(
        seen.health,
        HealthView::Unhealthy {
            reason: UnhealthyReason::StatusNon2xx
        }
    );
    assert_eq!(seen.reason_token(), Some("status_non_2xx"));
    assert_eq!(
        seen.health_body.as_deref(),
        Some("503 not placeable: memory short")
    );

    health.answer_body(URL_A, 200, b"ok");
    let seen = observer.observe(&js_agent(&[A, B]), DEADLINE);
    assert_eq!(seen.health_body.as_deref(), Some("second"));

    health.answer_body(URL_B, 204, b"");
    let seen = observer.observe(&js_agent(&[A, B]), DEADLINE);
    assert_eq!(seen.health, HealthView::Healthy);
    assert_eq!(seen.health_body, None);
}

/// Proves the body is text the heartbeat can carry: invalid UTF-8 replaced, every control
/// character a space, white space trimmed, cut to 160 bytes at a character's boundary (a
/// two-byte character that would end at byte 161 is left out whole); an empty or blank body is
/// none, and so is the body of a failure that was not an answer (no URL could be made, no answer).
///
/// Catches: a body over the heartbeat's 160 bytes (which the coordinator would refuse), a cut
/// inside a character, or control characters sent on.
#[test]
fn the_body_is_bounded_text() {
    let long = "x".repeat(200);
    let accent = format!("{}é", "a".repeat(159));
    let cases: [(&[u8], Option<&str>); 7] = [
        (b"  503 busy\r\n", Some("503 busy")),
        (b"a\tb\x00c\x1bd", Some("a b c d")),
        (b"bad \xff byte", Some("bad \u{fffd} byte")),
        (long.as_bytes(), Some(&long[..160])),
        (accent.as_bytes(), Some(&accent[..159])),
        (b"", None),
        (b" \r\n\t ", None),
    ];
    for (body, want) in cases {
        let (manager, health) = (FakeServiceManager::default(), FakeHealthClient::default());
        manager.set(A, ActiveState::Active, Some(11));
        health.answer_body(URL_A, 503, body);
        let seen = observer(&manager, &health).observe(&js_agent(&[A]), DEADLINE);
        assert_eq!(seen.health_body.as_deref(), want, "{body:?}");
        if let Some(text) = &seen.health_body {
            assert!(text.len() <= MAX_HEALTH_BODY, "{text:?}");
        }
    }

    let (manager, health) = (FakeServiceManager::default(), FakeHealthClient::default());
    manager.set(A, ActiveState::Active, Some(11));
    let seen = observer(&manager, &health).observe(&js_agent(&[A]), DEADLINE);
    assert_eq!(
        (seen.health, seen.health_body),
        (
            HealthView::Unhealthy {
                reason: UnhealthyReason::Unreachable
            },
            None
        )
    );
    let unnumbered = "js-agent.service";
    manager.set(unnumbered, ActiveState::Active, Some(13));
    let seen = observer(&manager, &health).observe(&js_agent(&[unnumbered]), DEADLINE);
    assert_eq!(
        (seen.health, seen.health_body),
        (
            HealthView::Unhealthy {
                reason: UnhealthyReason::UrlInvalid
            },
            None
        )
    );
}

/// Proves a service with no run units whose health URL answers 503 carries the body as a live
/// one does.
///
/// Catches: the body kept only for a service observed through its units.
#[test]
fn an_unmonitored_service_answering_503_carries_its_body() {
    let (manager, health) = (FakeServiceManager::default(), FakeHealthClient::default());
    health.answer_body(URL_A, 503, b"draining");
    let svc = ServiceInput {
        current: Some(Current {
            run: Vec::new(),
            health_url: Some(URL_A.to_owned()),
        }),
        ..js_agent(&[])
    };
    let seen = observer(&manager, &health).observe(&svc, DEADLINE);
    assert_eq!(seen.state, ServiceState::Unmonitored);
    assert_eq!(seen.health_body.as_deref(), Some("draining"));
}
