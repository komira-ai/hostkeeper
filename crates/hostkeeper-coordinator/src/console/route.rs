//! `GET /console/v1/stream/<session>`: the WebSocket that attaches to a console session
//! (`docs/design/console-broker.md`, decisions D2, D3 and D12, sections 8.1 and 12). Any
//! WebSocket client can use it; a browser's page is one.
//!
//! The client offers `hostkeeper.console.v1` and one credential as a second subprotocol, since a
//! browser's WebSocket cannot set `Authorization`: `bearer.<the operator's token in base64url>`,
//! or, from the web app, `csrf.<the CSRF token of its browser session>` beside the session's
//! cookie. A request that offers any `bearer.` is judged by the bearer alone, whatever cookie it
//! carries; one that offers none is judged by the cookie and its CSRF token, and the session's
//! user is then the operator. The `101` selects `hostkeeper.console.v1` alone, so neither
//! credential is ever echoed, and nothing here logs a request header. On the private listener,
//! in this order of checks, the first that fails answering `{"error":"<code>"}` before any
//! upgrade and with no subprotocol:
//!
//! | answer | code | when |
//! |---|---|---|
//! | 400 | `bad_request` | not an HTTP/1.1 WebSocket opening (`Upgrade`, `Connection`, version 13, key), or `hostkeeper.console.v1` is not offered |
//! | 401 | `unauthorized` | a `bearer.` offer that is not an operator's token in base64url; or no `bearer.` offer and no sign-in, or a cookie that names no live session |
//! | 403 | `csrf` | no `bearer.` offer, a live session, and no `csrf.` offer or one that is not the session's CSRF token |
//! | 403 | `password_change_required` | no `bearer.` offer, and the session's user must choose a new password first |
//! | 403 | `forbidden` | no `bearer.` offer, and the session's user is no operator's name |
//! | 403 | `bad_origin` | `Origin` is missing or is not `https://<the authority asked>` |
//! | 403 | `no_console_access` | the console operators file does not name the token's name |
//! | 404 | `no_session` | the path names no open session, or its window to be attached has passed |
//! | 403 | `forbidden` | another operator name opened the session |
//! | 403 | `already_attached` | a WebSocket is attached to the session already |
//! | 500 | `internal` | the listener did not hand the route its coordinator |
//!
//! Then the session is attached (a second attach is refused from now on), the connection is taken
//! off the listener's patience rule (it counts against `--console-max-sessions`, as its session
//! does), and `101 Switching Protocols` is answered; the stream ([`super::stream`]) takes the
//! upgraded connection and contacts the device. A failure after this is a close code.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use hostkeeper_console::bridge::Moved;
use hostkeeper_console::operators::Need;
use hostkeeper_console::session::{AttachRefused, SessionId};
use http_body_util::Full;
use hyper::header::{CACHE_CONTROL, CONNECTION, HeaderMap, ORIGIN, UPGRADE};
use hyper::http::request::Parts;
use hyper::upgrade::OnUpgrade;
use hyper::{Request, Response, StatusCode, Version};

use super::Live;
use super::stream::{Attached, run};
use crate::http::{PeerAddr, error};
use crate::listen::{Hold, Patience};
use crate::login::{Login, authority, held, same_origin};
use crate::service::Coordinator;
use crate::ssh_terminal_route::Shared;
use crate::tokens::TokenName;
use crate::websocket::handshake;

/// The path every stream is below: the session's id follows it.
pub const PREFIX: &str = "/console/v1/stream/";

/// The subprotocol of a console stream, the one the `101` selects.
pub const PROTOCOL: &str = "hostkeeper.console.v1";

/// The start of the subprotocol that carries the bearer.
const BEARER: &str = "bearer.";

/// The start of the subprotocol that carries a browser session's CSRF token.
const CSRF: &str = "csrf.";

/// The most bytes of an offered bearer read, in base64url.
const MAX_BEARER: usize = 2 * crate::service::MAX_BEARER_BYTES;

/// The most bytes of an offered CSRF token read.
const MAX_CSRF: usize = 256;

type Answer = Response<Full<Bytes>>;

/// The subprotocols `headers` offer, in order.
fn offers(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
}

/// The token of the first `bearer.` offer of `headers`, decoded from base64url (with or without
/// its padding), if it is UTF-8.
fn bearer_of(headers: &HeaderMap) -> Option<String> {
    let encoded = offers(headers).find_map(|offer| offer.strip_prefix(BEARER))?;
    if encoded.len() > MAX_BEARER {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')).ok()?;
    String::from_utf8(bytes).ok()
}

/// The token of the first `csrf.` offer of `headers`, unless it is longer than [`MAX_CSRF`].
fn csrf_of(headers: &HeaderMap) -> Option<&str> {
    offers(headers)
        .find_map(|offer| offer.strip_prefix(CSRF))
        .filter(|token| token.len() <= MAX_CSRF)
}

/// The operator name the request's credential gives, or the refusal: the bearer's token name when
/// the request offers any `bearer.`, else the user of its browser session.
fn credential(
    coordinator: &Coordinator,
    login: Option<&Login>,
    headers: &HeaderMap,
) -> Result<TokenName, (StatusCode, &'static str)> {
    if offers(headers).any(|offer| offer.starts_with(BEARER)) {
        return bearer_of(headers)
            .and_then(|token| coordinator.operator(Some(&token)).ok())
            .ok_or((StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    let Some(session) = login.and_then(|login| held(login, headers)) else {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    if !csrf_of(headers).is_some_and(|token| session.view.csrf_matches(token)) {
        return Err((StatusCode::FORBIDDEN, "csrf"));
    }
    if session.view.must_change {
        return Err((StatusCode::FORBIDDEN, "password_change_required"));
    }
    session
        .view
        .username
        .parse()
        .map_err(|_| (StatusCode::FORBIDDEN, "forbidden"))
}

/// Answers a `GET` of the stream of the session `id` (the path after [`PREFIX`]): the refusal of
/// the first check that fails, or the `101` whose stream is spawned. `login` is the coordinator's
/// sign-in, if it has one.
pub(crate) fn answer<B>(
    coordinator: &Coordinator,
    login: Option<&Login>,
    mut request: Request<B>,
    id: &str,
) -> Answer {
    // Taken first and dropped by a refusal; the checks then read the request's parts, so that
    // one copy of them is compiled whatever the request's body.
    let upgrade = hyper::upgrade::on(&mut request);
    let (parts, _) = request.into_parts();
    answer_parts(coordinator, login, &parts, upgrade, id)
}

/// [`answer`] over the request's `parts`, with its `upgrade`.
fn answer_parts(
    coordinator: &Coordinator,
    login: Option<&Login>,
    request: &Parts,
    upgrade: OnUpgrade,
    id: &str,
) -> Answer {
    let headers = &request.headers;
    let accept = match handshake(headers) {
        Ok(accept) if request.version == Version::HTTP_11 => accept,
        _ => return error(StatusCode::BAD_REQUEST, "bad_request"),
    };
    if !offers(headers).any(|offer| offer == PROTOCOL) {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    }
    let name = match credential(coordinator, login, headers) {
        Ok(name) => name,
        Err((status, code)) => return error(status, code),
    };
    let own_origin =
        headers.contains_key(ORIGIN) && same_origin(headers, authority(headers, &request.uri));
    if !own_origin {
        return error(StatusCode::FORBIDDEN, "bad_origin");
    }
    if coordinator
        .console
        .files
        .grant(name.as_str(), Need::Console)
        .is_err()
    {
        return error(StatusCode::FORBIDDEN, "no_console_access");
    }
    let Ok(id) = SessionId::parse(id) else {
        return error(StatusCode::NOT_FOUND, "no_session");
    };
    let Some(Shared(shared)) = request.extensions.get::<Shared>().cloned() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal");
    };
    let peer = request
        .extensions
        .get::<PeerAddr>()
        .map(|PeerAddr(from)| *from);
    let attached = match attach(&shared, &id, name.as_str(), peer) {
        Ok(attached) => attached,
        Err(AttachRefused::NoSession) => return error(StatusCode::NOT_FOUND, "no_session"),
        Err(AttachRefused::NotOpener) => return error(StatusCode::FORBIDDEN, "forbidden"),
        Err(AttachRefused::Attached) => return error(StatusCode::FORBIDDEN, "already_attached"),
    };
    if let Some(patience) = request.extensions.get::<Patience>() {
        patience.release();
    }
    let attached = Attached {
        slot: request.extensions.get::<Hold>().cloned(),
        ..attached
    };
    tokio::spawn(run(attached, upgrade));
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "Upgrade")
        .header(UPGRADE, "websocket")
        .header("sec-websocket-accept", accept)
        .header("sec-websocket-protocol", PROTOCOL)
        .header(CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::new()))
        .unwrap_or_default()
}

/// Attaches the operator `by`, connected from `peer`, to the session `id` and registers its
/// stream, under the table's lock: what the stream starts with.
///
/// # Errors
/// [`AttachRefused`].
pub(crate) fn attach(
    coordinator: &Arc<Coordinator>,
    id: &SessionId,
    by: &str,
    peer: Option<SocketAddr>,
) -> Result<Attached, AttachRefused> {
    let now = (coordinator.clock)();
    let mut table = coordinator.console.lock_table();
    let session = table.sessions.attach(id, by, now)?.clone();
    let (stop, stopped) = tokio::sync::watch::channel(None);
    let moved = Arc::new(Moved::default());
    table.live.insert(
        session.host_id.clone(),
        Live {
            id: *id,
            stop,
            moved: Arc::clone(&moved),
        },
    );
    Ok(Attached {
        coordinator: Arc::clone(coordinator),
        session,
        peer,
        stopped,
        moved,
        slot: None,
    })
}
