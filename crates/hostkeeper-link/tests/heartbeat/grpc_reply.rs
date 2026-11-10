//! The size of the reply the gRPC link reads: 1 MiB, so that an order carrying the largest
//! entry fits, and so does a repository set of the most repositories with the largest keyrings. The coordinator's side here is a server that answers every call with one fixed
//! message (the real coordinator never builds a reply of an exact size), over the same
//! in-memory streams and TLS as the other gRPC tests.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_link::{
    CoordinatorLink, GRPC_REPLY_MAX, LinkError, Order, REPLY_MAX, ReplyFault, Verb,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::release::v1 as release;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::HeaderMap;
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use tokio::io::duplex;
use tokio_rustls::TlsAcceptor;

use crate::grpc::{DuplexDial, grpc_link, web_beat};
use crate::tls::{Farm, Pki};
use crate::{digest, order_id, service, version};

/// The most bytes of an entry a release may list: `hostkeeper_release::MAX_ENTRY_BYTES`, 24 KiB
/// encoded. Written out here because the layering rules keep this crate off `hostkeeper-release`.
const MAX_ENTRY_BYTES: usize = 24 * 1024;

/// The `update` of `web` to 2.0 with an entry whose one locator is `padding` bytes long.
fn response_with(padding: usize) -> pb::HeartbeatResponse {
    pb::HeartbeatResponse {
        order: Some(pb::Order {
            order_id: "o-1".to_owned(),
            r#try: 1,
            verb: Some(pb::order::Verb::Update(pb::Update {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: digest().as_str().to_owned(),
                index_digest: String::new(),
                target: Some(entry_with(padding)),
            })),
        }),
        ..pb::HeartbeatResponse::default()
    }
}

fn entry_with(padding: usize) -> release::Target {
    release::Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: vec![release::Location {
            store: "lan".to_owned(),
            locator: "x".repeat(padding),
        }],
        digest: "ab".repeat(32),
        bytes: 4096,
        attestation: None,
        manifest: None,
        repository: None,
        package_version: None,
    }
}

/// The padding that makes `measure` of the message exactly `size` bytes.
fn padded_to(size: usize, measure: impl Fn(usize) -> usize) -> usize {
    let mut padding = size / 2;
    while measure(padding) != size {
        padding = padding + size - measure(padding);
    }
    padding
}

/// What the link answers a heartbeat to a coordinator that replies with `response`.
fn answered(
    name: &str,
    response: &pb::HeartbeatResponse,
) -> Result<hostkeeper_link::Reply, LinkError> {
    let farm = Farm::new();
    let pki = Pki::with_bundle(name, &farm.ca.pem);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let link = grpc_link(&farm, &pki, &dial, &log);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime");
    let (client, server) = duplex(1 << 20);
    dial.queue(client);
    let mut config = (*farm.server()).clone();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let message = response.encode_to_vec();
    let mut framed = vec![0_u8];
    framed.extend_from_slice(
        &u32::try_from(message.len())
            .expect("a length")
            .to_be_bytes(),
    );
    framed.extend_from_slice(&message);
    runtime.spawn(async move {
        let tls = TlsAcceptor::from(Arc::new(config))
            .accept(server)
            .await
            .expect("the handshake");
        let service = service_fn(move |request: Request<Incoming>| {
            let framed = framed.clone();
            async move {
                let _ = request.into_body().collect().await;
                let mut trailers = HeaderMap::new();
                trailers.insert("grpc-status", "0".parse().expect("a header value"));
                let body = Full::new(Bytes::from(framed))
                    .with_trailers(async move { Some(Ok::<_, Infallible>(trailers)) });
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-type", "application/grpc")
                        .body(body)
                        .expect("a response"),
                )
            }
        });
        let _ = http2::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(tls), service)
            .await;
    });
    link.heartbeat(&farm.host.identity(), &web_beat())
}

/// Proves the gRPC route's reply bound is 1 MiB and the JSON route's stays 16 KiB: a reply of
/// exactly [`GRPC_REPLY_MAX`] bytes is read whole, one byte more is refused as over the bound,
/// and an order carrying the largest valid entry (`MAX_ENTRY_BYTES`) is read with that entry
/// unchanged.
///
/// Catches: a reply limit left at 16 KiB, so that an order with a large entry or a repository set
/// is refused, and
/// a limit raised past 1 MiB.
#[test]
fn a_grpc_reply_of_1_mib_is_read_and_one_byte_over_is_refused() {
    assert_eq!(GRPC_REPLY_MAX, 1024 * 1024);
    assert_eq!(REPLY_MAX, 16 * 1024);

    let at = padded_to(GRPC_REPLY_MAX, |n| response_with(n).encoded_len());
    let response = response_with(at);
    assert_eq!(response.encoded_len(), GRPC_REPLY_MAX);
    let reply = answered("grpc-reply-at", &response).expect("a reply");
    let order: Order = reply.order.expect("an order");
    let Verb::Update(target) = &order.verb else {
        panic!("{order:?}");
    };
    assert_eq!(
        target.entry.as_ref().map(|entry| entry.as_target()),
        Some(&entry_with(at))
    );
    assert_eq!(order.id, order_id("o-1"));
    assert_eq!(target.service, service("web"));
    assert_eq!(target.version, version("2.0"));

    let over = response_with(at + 1);
    assert_eq!(over.encoded_len(), GRPC_REPLY_MAX + 1);
    assert_eq!(
        answered("grpc-reply-over", &over),
        Err(LinkError::BadReply(ReplyFault::OverBound))
    );
}

/// Proves an order carrying an entry of the largest size a release may list
/// (`MAX_ENTRY_BYTES`, 24 KiB encoded) is read whole.
///
/// Catches: a reply bound too small for the entry the coordinator is allowed to pick.
#[test]
fn an_order_carrying_the_largest_valid_entry_is_read_whole() {
    let padding = padded_to(MAX_ENTRY_BYTES, |n| entry_with(n).encoded_len());
    let entry = entry_with(padding);
    assert_eq!(entry.encoded_len(), MAX_ENTRY_BYTES);
    let response = response_with(padding);
    assert!(response.encoded_len() <= GRPC_REPLY_MAX);
    let reply = answered("grpc-reply-entry", &response).expect("a reply");
    let order = reply.order.expect("an order");
    assert_eq!(
        order
            .target()
            .and_then(|target| target.entry.clone())
            .map(hostkeeper_link::Entry::into_target),
        Some(entry)
    );
}

/// Proves a repository set of the most repositories a host takes (8), each with a keyring of the
/// most bytes (64 KiB), is read whole and mapped with every field and keyring byte unchanged.
///
/// Catches: a reply bound too small for the biggest set the coordinator may send.
#[test]
fn a_repository_set_of_eight_largest_keyrings_is_read_whole() {
    let entries: Vec<release::Repository> = (0..8u8)
        .map(|n| release::Repository {
            name: format!("repo{n}"),
            kind: "apt".to_owned(),
            uri: "https://repo.example.com/debian".to_owned(),
            suites: vec!["stable".to_owned()],
            components: vec!["main".to_owned()],
            fingerprints: vec!["AB".repeat(20)],
            keyring: vec![n; 64 * 1024],
        })
        .collect();
    let response = pb::HeartbeatResponse {
        repositories: Some(pb::RepositorySet {
            generation: 7,
            digest: "cd".repeat(32),
            entries: entries.clone(),
        }),
        ..pb::HeartbeatResponse::default()
    };
    assert!(response.encoded_len() > 512 * 1024);
    assert!(response.encoded_len() <= GRPC_REPLY_MAX);
    let reply = answered("grpc-reply-set", &response).expect("a reply");
    assert_eq!(
        reply.repositories,
        Some(hostkeeper_link::PushedSet {
            generation: 7,
            digest: "cd".repeat(32),
            entries,
        })
    );
    assert_eq!(reply.order, None);
}
