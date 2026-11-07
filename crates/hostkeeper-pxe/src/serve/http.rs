//! The HTTP server of a run (HTTP/1.1): `GET` and `HEAD` of the names [`Server::content`]
//! serves by HTTP, each at `/` and its name: every sealed file, the fixed `grub.cfg` and the
//! menus under `/grub/`, and the seeds under `/seed/<mac>/`. Any other path is answered 404, a
//! path with a query 404, and every other method 405 with `Allow: GET, HEAD`; nothing is ever
//! written. A sealed file is sent a chunk at a time from the descriptor checked at start.
//!
//! Header names are written in title case (`Content-Length`), the spelling GRUB's HTTP client
//! matches, byte for byte: a GRUB that reads no `Content-Length` takes the size as unknown, and its
//! `linux` and `initrd` then fail with "out of memory" (found by the network-install end-to-end
//! test, `crates/hostkeeper-privileged/tests/net_install.rs`).
//!
//! Each request is logged with the peer, the method, the path and the status.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use futures::{StreamExt as _, stream};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::serve::sealed::CHUNK;
use crate::serve::server::{Content, Protocol, Server};

/// The body of an answer.
type Body = BoxBody<Bytes, io::Error>;

/// Accepts the connections of `listener` until `stop` changes.
pub async fn serve(server: Arc<Server>, listener: TcpListener, mut stop: watch::Receiver<bool>) {
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stop.changed() => return,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(_) => continue,
        };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let server = Arc::clone(&server);
                async move { Ok::<_, Infallible>(answer(&server, request, peer).await) }
            });
            let _ = http1::Builder::new()
                .title_case_headers(true)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

/// The answer to `request` from `peer`.
async fn answer(server: &Server, request: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let (status, body, len) = match method {
        Method::GET | Method::HEAD => {
            // The path of an HTTP/1.1 request starts with `/`.
            let name = path.get(1..).unwrap_or_default();
            let content = match request.uri().query() {
                Some(_) => None,
                None => {
                    let protocol = match method == Method::HEAD {
                        true => Protocol::Head,
                        false => Protocol::Http,
                    };
                    server.content(name, protocol).await
                }
            };
            match content {
                Some(content) => {
                    let len = content.len();
                    let body = match method == Method::HEAD {
                        true => empty(),
                        false => body_of(content),
                    };
                    (StatusCode::OK, body, Some(len))
                }
                None => (StatusCode::NOT_FOUND, empty(), None),
            }
        }
        _ => (StatusCode::METHOD_NOT_ALLOWED, empty(), None),
    };
    server.log().line(format_args!(
        "http {peer}: {method} {path}: {}",
        status.as_u16()
    ));
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    if status == StatusCode::METHOD_NOT_ALLOWED {
        headers.insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
    }
    if let Some(len) = len {
        headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
    }
    response
}

/// An empty body.
fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed()
}

/// The body of `content`: a text at once, a sealed file a chunk at a time.
fn body_of(content: Content) -> Body {
    match content {
        Content::Text(text) => Full::new(Bytes::from(text))
            .map_err(|never| match never {})
            .boxed(),
        file @ Content::File(_) => {
            let file = Arc::new(file);
            let size = file.len();
            // One chunk from each offset; a read that fails ends the body with its error, so
            // the client sees a body shorter than its Content-Length, never a whole one.
            let chunks = stream::iter((0..size).step_by(CHUNK)).then(move |offset| {
                let file = Arc::clone(&file);
                let len = (size - offset).min(CHUNK as u64) as usize;
                async move {
                    tokio::task::spawn_blocking(move || file.read(offset, len))
                        .await
                        .map_err(io::Error::other)
                        .and_then(|read| read)
                        .map(|bytes| Frame::data(Bytes::from(bytes)))
                }
            });
            BodyExt::boxed(StreamBody::new(chunks))
        }
    }
}
