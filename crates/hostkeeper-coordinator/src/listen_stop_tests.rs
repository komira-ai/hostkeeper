//! The public `serve`, and the stop of a listener's connections ([`crate::graceful`]) as the
//! coordinator's stop drives it: a held call answered and its connection closed, and the count of
//! open connections back to zero. On the paused clock where a wait is held.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::WaitConvergedResponse;
use http_body_util::BodyExt;
use prost::Message as _;
use rustls::pki_types::ServerName;
use serde_json::Value;
use tonic::{Code, Status};

use super::tests::{connector, coordinator, http2, status_over_http1, tls_material, wait_request};
use super::{CLIENT_PATIENCE, Listener, accept_each, connection_with, serve};
use crate::graceful::Graceful;
use crate::http::Surface;
use crate::testutil::{Rig, ci_token};

/// Proves: [`serve`], the listener that is never told to stop, accepts a connection its accept
/// function yields and answers the operator's status on it over HTTP/1.1 (`200`, a JSON body).
///
/// Catches: the public `serve` left behind when the coordinator's own listeners moved to the
/// stopping variant, so a caller of it (a test rig of another crate) serves nothing.
#[tokio::test]
async fn serve_answers_a_connection_its_accept_yields() {
    let (coordinator, _dir) = coordinator("listen-serve");
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let mut connections = Some(server);
    let accept = move || {
        let next = connections.take();
        async move {
            match next {
                Some(stream) => Ok(stream),
                None => std::future::pending().await,
            }
        }
    };
    let listener = Listener {
        accept,
        tls: source,
        surface: Surface::Control,
    };
    let serving = tokio::spawn(serve(listener, coordinator, CLIENT_PATIENCE, 2));
    let tls = connector(&ca, &[b"http/1.1"])
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    let answer = status_over_http1(tls).await;
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("\r\n\r\n{"), "a JSON body: {answer}");
    serving.abort();
}

/// Proves: at the stop (the listeners told to stop and the held waits ended, as the stop does
/// once its drain delay is over), an HTTP/2 connection that carries a held `Releases.WaitConverged`
/// call for 30 seconds, as gRPC or as the gateway's JSON, has the call answered at once, five
/// seconds in, `DEADLINE` (no host runs kbf); the connection then ends, and the count of open
/// connections of the listener goes back to zero, so the stop's wait for them ends.
///
/// Catches: a held call that keeps a stopping coordinator's connection, and so its exit, for up to
/// half an hour, and a connection the listener never stops counting.
#[tokio::test(start_paused = true)]
async fn a_held_http2_connection_is_answered_and_closed_at_the_stop() {
    for gateway in [false, true] {
        let rig = Rig::new("listen-h2-held-stop");
        rig.coordinator
            .set_desired(Some(&ci_token()), br#"{"version":"2.0"}"#)
            .await
            .expect("set");
        let (coordinator, _dir) = (Arc::new(rig.coordinator), rig.dir);
        let (source, ca) = tls_material();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let mut connections = Some(server);
        let accept = move || {
            let next = connections.take();
            async move {
                match next {
                    Some(stream) => Ok(stream),
                    None => std::future::pending().await,
                }
            }
        };
        let graceful = Graceful::default();
        let served = Arc::clone(&coordinator);
        let accepting = tokio::spawn(accept_each(
            accept,
            Arc::new(tokio::sync::Semaphore::new(2)),
            graceful.clone(),
            move |stream, graceful| {
                connection_with(
                    stream,
                    source.config(),
                    Surface::Control,
                    Arc::clone(&served),
                    CLIENT_PATIENCE,
                    graceful,
                )
            },
        ));
        let tls = connector(&ca, &[b"h2"])
            .connect(ServerName::try_from("localhost").expect("a name"), client)
            .await
            .expect("a handshake");
        let mut send = http2(tls).await;
        let mut answer = tokio::spawn(send.send_request(wait_request(gateway)));
        tokio::select! {
            _ = &mut answer => panic!("answered before the stop ({gateway})"),
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        assert_eq!(graceful.count(), 1, "{gateway}");
        graceful.stop();
        coordinator.waits.stop();
        let answer = tokio::select! {
            answer = &mut answer => answer.expect("joined").expect("an answer"),
            () = tokio::time::sleep(Duration::from_secs(1)) => panic!("not answered at the stop ({gateway})"),
        };
        assert_eq!(answer.status(), 200, "{gateway}");
        let (parts, body) = answer.into_parts();
        let collected = body.collect().await.expect("the body");
        let outcome = if gateway {
            let reply: Value = serde_json::from_slice(&collected.to_bytes()).expect("JSON");
            reply["outcome"].as_str().expect("an outcome").to_owned()
        } else {
            let status = collected
                .trailers()
                .and_then(Status::from_header_map)
                .or_else(|| Status::from_header_map(&parts.headers))
                .expect("a grpc-status");
            assert_eq!(status.code(), Code::Ok);
            let bytes = collected.to_bytes();
            let reply = WaitConvergedResponse::decode(&bytes[5..]).expect("a message");
            reply.outcome().as_str_name().to_owned()
        };
        assert_eq!(outcome, "WAIT_OUTCOME_DEADLINE", "{gateway}");
        graceful.closed().await;
        assert_eq!(graceful.count(), 0, "{gateway}");
        drop(send);
        accepting.abort();
    }
}
