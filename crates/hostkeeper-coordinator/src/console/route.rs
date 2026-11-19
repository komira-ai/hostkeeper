//! `GET /console/v1/stream/<session>`: the WebSocket that attaches to a console session
//! (`docs/design/console-broker.md`, decisions D2, D3 and D12, sections 8.1 and 12). Any
//! WebSocket client can use it; a browser's page is one.
//!
//! The client offers `hostkeeper.console.v1` and, since a browser's WebSocket cannot set
//! `Authorization`, one credential in a subprotocol: `bearer.<the operator's token in base64url>`,
//! or, for the signed-in web UI (a session cookie, no token), `csrf.<the session's CSRF token>`
//! beside the cookie. A `bearer.` offer, if there is any, is the credential, and the cookie is
//! then not read. The `101` selects `hostkeeper.console.v1` alone, so neither the token nor the
//! CSRF token is echoed, and nothing here logs a request header. On the private listener, in this
//! order of checks, the first that fails answering `{"error":"<code>"}` before any upgrade and
//! with no subprotocol:
//!
//! | answer | code | when |
//! |---|---|---|
//! | 400 | `bad_request` | not an HTTP/1.1 WebSocket opening (`Upgrade`, `Connection`, version 13, key), or `hostkeeper.console.v1` is not offered |
//! | 401 | `unauthorized` | a `bearer.` offer that is not an operator's token in base64url; or, with none, no cookie of a live session (and always, on a coordinator with no sign-in) |
//! | 403 | `bad_origin` | `Origin` is missing or is not `https://<the authority asked>` |
//! | 403 | `csrf` | a session's cookie, and no `csrf.` offer that is the session's CSRF token |
//! | 403 | `password_change_required` | the session's user must choose a new password first |
//! | 403 | `forbidden` | the session's user is no operator's name |
//! | 403 | `no_console_access` | the console operators file does not name the operator (the token's name, or the session's user) |
//! | 404 | `no_session` | the path names no open session, or its window to be attached has passed |
//! | 403 | `forbidden` | another operator name opened the session |
//! | 403 | `already_attached` | a WebSocket is attached to the session already |
//! | 500 | `internal` | the listener did not hand the route its coordinator |
//!
//! Then the session is attached (a second attach is refused from now on), the connection is taken
//! off the listener's patience rule (it counts against `--console-max-sessions`, as its session
//! does), and `101 Switching Protocols` is answered; the stream ([`super::stream`]) takes the
//! upgraded connection and contacts the device. A failure after this is a close code. The stream
//! keeps the credential it attached with ([`Credential`]), which the sweep holds it to.

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
use crate::login::{Held, authority, held, same_origin};
use crate::service::Coordinator;
use crate::session::Sessions;
use crate::ssh_terminal_route::Shared;
use crate::ssh_tunnel::Credential;
use crate::tokens::TokenName;
use crate::websocket::handshake;

/// The path every stream is below: the session's id follows it.
pub const PREFIX: &str = "/console/v1/stream/";

/// The subprotocol of a console stream, the one the `101` selects.
pub const PROTOCOL: &str = "hostkeeper.console.v1";

/// The start of the subprotocol that carries the bearer.
const BEARER: &str = "bearer.";

/// The start of the subprotocol that carries a session's CSRF token.
const CSRF: &str = "csrf.";

/// The most bytes of an offered bearer or CSRF token read, in base64url.
const MAX_BEARER: usize = 2 * crate::service::MAX_BEARER_BYTES;

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

/// The CSRF token of the first `csrf.` offer of `headers`, if it is not longer than a bearer.
fn csrf_of(headers: &HeaderMap) -> Option<&str> {
    offers(headers)
        .find_map(|offer| offer.strip_prefix(CSRF))
        .filter(|token| token.len() <= MAX_BEARER)
}

/// What the client acts under, before its origin is checked.
enum Who {
    /// An operator's token, by its name.
    Token(TokenName),
    /// A signed-in session.
    Session(Held),
}

/// The credential `headers` offer: with a `bearer.` offer, the operator's token it is (and the
/// cookie is not read); otherwise the live session the cookie names, on a coordinator that has a
/// sign-in.
fn who(coordinator: &Coordinator, headers: &HeaderMap) -> Option<Who> {
    if offers(headers).any(|offer| offer.starts_with(BEARER)) {
        let token = bearer_of(headers)?;
        return coordinator.operator(Some(&token)).ok().map(Who::Token);
    }
    held(coordinator.login()?, headers).map(Who::Session)
}

/// The operator a session of the browser is, and the code of the refusal if it may not attach:
/// it offered the session's CSRF token, its user need not change the password, and its name is an
/// operator's.
fn session_operator(held: &Held, headers: &HeaderMap) -> Result<TokenName, &'static str> {
    if !csrf_of(headers).is_some_and(|token| held.view.csrf_matches(token)) {
        return Err("csrf");
    }
    if held.view.must_change {
        return Err("password_change_required");
    }
    held.view.username.parse().map_err(|_| "forbidden")
}

/// Answers a `GET` of the stream of the session `id` (the path after [`PREFIX`]): the refusal of
/// the first check that fails, or the `101` whose stream is spawned.
pub(crate) fn answer<B>(coordinator: &Coordinator, mut request: Request<B>, id: &str) -> Answer {
    // Taken first and dropped by a refusal; the checks then read the request's parts, so that
    // one copy of them is compiled whatever the request's body.
    let upgrade = hyper::upgrade::on(&mut request);
    let (parts, _) = request.into_parts();
    answer_parts(coordinator, &parts, upgrade, id)
}

/// [`answer`] over the request's `parts`, with its `upgrade`.
fn answer_parts(
    coordinator: &Coordinator,
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
    let Some(who) = who(coordinator, headers) else {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let own_origin =
        headers.contains_key(ORIGIN) && same_origin(headers, authority(headers, &request.uri));
    if !own_origin {
        return error(StatusCode::FORBIDDEN, "bad_origin");
    }
    let (name, credential) = match who {
        Who::Token(name) => (name, Credential::Token),
        Who::Session(held) => match session_operator(&held, headers) {
            Ok(name) => (name, Credential::Session(Sessions::key_of(&held.id))),
            Err(code) => return error(StatusCode::FORBIDDEN, code),
        },
    };
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
    let attached = match attach(&shared, &id, name.as_str(), credential, peer) {
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

/// Attaches the operator `by`, acting under `credential` and connected from `peer`, to the session
/// `id` and registers its stream, under the table's lock: what the stream starts with.
///
/// # Errors
/// [`AttachRefused`].
pub(crate) fn attach(
    coordinator: &Arc<Coordinator>,
    id: &SessionId,
    by: &str,
    credential: Credential,
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
            credential,
            used_at: 0,
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
