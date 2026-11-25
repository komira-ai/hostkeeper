//! `GET /v1/ssh/terminal?host=<host id>`: the opening of the UI's Remote SSH
//! (docs/design/ssh-through-coordinator.md, section 13). A browser asks for a WebSocket upgrade;
//! what happens after the `101` is [`crate::ssh_terminal`].
//!
//! The route is on the private surface of a coordinator that has a sign-in, and answers before
//! any byte is relayed, in the style of the other `/v1` routes: `{"error":"<code>"}`, in this
//! order of checks, the first that fails answering:
//!
//! | answer | code | when |
//! |---|---|---|
//! | 403 | `forbidden` | the request has an `Authorization` header (a bearer is not taken here, and the cookie beside it is not read) |
//! | 401 | `sign_in_required` | no cookie of a live session |
//! | 403 | `csrf` | `Origin` is missing or is not `https://<authority>`, or `Sec-Fetch-Site` is present and not `same-origin` |
//! | 403 | `password_change_required` | the session's user must choose a new password first |
//! | 403 | `forbidden` | the session's user is no operator's name |
//! | 400 | `bad_request` | the request is not an HTTP/1.1 WebSocket opening (`Upgrade`, `Connection`, version 13, key), or has no `host` in its query |
//! | 404 | `not_configured` | the coordinator holds no SSH CA or no tunnels |
//! | 500 | `internal` | the listener did not hand the route its coordinator (a request that came in by no listener) |
//!
//! Then `101 Switching Protocols`, with the accept key, and the terminal task takes the upgraded
//! connection, which keeps the listener's slot of it until the terminal ends. The CSRF token cannot travel in a header a browser's WebSocket can set, so the
//! terminal's first message carries it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{AUTHORIZATION, CACHE_CONTROL, CONNECTION, ORIGIN, UPGRADE};
use hyper::{Request, Response, StatusCode, Version};

use crate::http::error;
use crate::listen::Hold;
use crate::login::{Login, authority, held, same_origin};
use crate::service::Coordinator;
use crate::ssh_terminal::{Start, run};
use crate::websocket::handshake;

/// The path of the route.
pub const PATH: &str = "/v1/ssh/terminal";

type Answer = Response<Full<Bytes>>;

/// The coordinator a listener serves, carried in the extensions of each request it hands to the
/// router: the terminal outlives the request that opened it, so it needs the coordinator itself
/// and not a borrow.
#[derive(Clone)]
pub(crate) struct Shared(pub(crate) Arc<Coordinator>);

/// The host the query names: `host=<id>`, among `&`-separated pairs. It is not decoded: a host
/// id is plain, and what is not one is refused by the call that reads the host.
fn host_of(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("host="))
        .filter(|host| !host.is_empty())
}

/// Answers a `GET` of the route: the refusal of the first check that fails, or the `101` that
/// opens the terminal, whose task is spawned.
pub(crate) fn answer<B>(
    coordinator: &Coordinator,
    login: &Login,
    mut request: Request<B>,
    patience: Duration,
) -> Answer {
    let headers = request.headers();
    if headers.contains_key(AUTHORIZATION) {
        return error(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(session) = held(login, headers) else {
        return error(StatusCode::UNAUTHORIZED, "sign_in_required");
    };
    let own_origin =
        headers.contains_key(ORIGIN) && same_origin(headers, authority(headers, request.uri()));
    if !own_origin {
        return error(StatusCode::FORBIDDEN, "csrf");
    }
    if session.view.must_change {
        return error(StatusCode::FORBIDDEN, "password_change_required");
    }
    let Ok(operator) = session.view.username.parse() else {
        return error(StatusCode::FORBIDDEN, "forbidden");
    };
    let accept = match handshake(headers) {
        Ok(accept) if request.version() == Version::HTTP_11 => accept,
        _ => return error(StatusCode::BAD_REQUEST, "bad_request"),
    };
    let Some(host) = host_of(request.uri().query()).map(str::to_owned) else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    if coordinator.ssh_tunnels().is_none() {
        return error(StatusCode::NOT_FOUND, "not_configured");
    }
    let Some(Shared(shared)) = request.extensions().get::<Shared>().cloned() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal");
    };
    // The socket the upgrade hands over keeps the listener's slot of its connection.
    let slot = request.extensions().get::<Hold>().cloned();
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(run(
        Start {
            coordinator: shared,
            operator,
            session: crate::session::Sessions::key_of(&session.id),
            view: session.view,
            host,
            patience,
            slot,
        },
        upgrade,
    ));
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "Upgrade")
        .header(UPGRADE, "websocket")
        .header("sec-websocket-accept", accept)
        .header(CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::new()))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "ssh_terminal_route_tests.rs"]
mod tests;
