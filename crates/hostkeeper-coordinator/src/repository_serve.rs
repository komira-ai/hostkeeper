//! The coordinator's route that serves a private repository to the hosts of one group
//! (docs/design/hostgroups.md, section 6.6): `GET /repositories/<name>/dists/<group>/<rest>` and
//! `GET /repositories/<name>/pool/<group>/<rest>` on the heartbeat surface ([`PREFIX`]), which the
//! agent listener and the LAN listener both serve. The coordinator parses and signs nothing: the
//! bytes are the origin's ([`crate::repository_origin`]), read with the `Authorization` header the
//! repository's credential file holds, which is read again at every request and never leaves the
//! coordinator.
//!
//! The caller is a host: its client certificate names one, as for a heartbeat. No certificate or
//! one that names no host is 403 `forbidden`, a denied or retiring host 403 `denied`, a method
//! other than `GET` 405 `method_not_allowed`. Everything else a host may not read is 404
//! `not_found`, the answer of a path that does not exist: a name that is not a private repository
//! of the registry (public or unknown), a `<group>` that is not the host's own group
//! ([`group_of`]) or not one of the repository's suites, and a `<rest>` that is not 1 to 1024
//! bytes of segments with no empty, `.` or `..` segment and no `%` (the locator grammar). A read
//! the grammar passes but the host may not make, another group's path or a name that is not
//! private, is also logged at warn with the node id and the path. The group is looked up at every
//! request, so a host whose certificate names another role is served that group's paths only, and
//! once the store holds a host group a host is served the paths of the group its record holds.
//!
//! The origin's 200 is answered 200, the body streamed as it arrives and never held whole, with
//! the `Content-Length` the origin gave; its 404 is 404 `not_found`. Any other status, a failed
//! connection, an answer with no `Content-Length`, an unusable credential file and a coordinator
//! with no origin client ([`Coordinator::with_repository_origin`]) are 502 `origin_failed`, with
//! one warn line that names the repository, the path and the kind of failure, never the
//! credential.

use bytes::Bytes;
use hostkeeper_release::grammar::{check_locator, is_repository_name};
use hostkeeper_release::registry;
use hostkeeper_wire::HostId;
use http_body_util::{Either, Full};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};

use crate::enrollment::Admission;
use crate::grpc::RouteBody;
use crate::http::{DENIED, Peer, error};
use crate::identity::Identity;
use crate::repository_credential::{self, CredentialError};
use crate::repository_origin::{Origin, OriginError};
use crate::service::Coordinator;

/// The path every route of this module is under.
pub(crate) const PREFIX: &str = "/repositories/";

/// Why a read of the origin failed.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// The coordinator has no origin client.
    #[error("the coordinator has no origin client")]
    NoOrigin,
    /// The credential file is unusable.
    #[error("{0}")]
    Credential(#[from] CredentialError),
    /// The origin did not answer as the route needs.
    #[error("{0}")]
    Origin(#[from] OriginError),
}

/// The group of the host `id`, whose certificate names `role`: the role. A coordinator that
/// holds host groups judges a request by [`group_of`] instead.
pub(crate) fn host_group(_id: &HostId, role: &str) -> String {
    role.to_owned()
}

/// The group whose paths the host `identity` may read: in group mode the group of its enrollment
/// record, empty for a host in none (it reads nothing); else its certificate's role
/// ([`host_group`]). It is the group the host's repository set names the suite of
/// ([`crate::repository_push`]).
fn group_of(coordinator: &Coordinator, identity: &Identity) -> String {
    if !coordinator.desired.in_group_mode() {
        return host_group(&identity.host_id, identity.role.name());
    }
    coordinator
        .membership_of(&identity.host_id)
        .map(|(group, _)| group.to_string())
        .unwrap_or_default()
}

/// `answer` as a route's.
fn plain(answer: Response<Full<Bytes>>) -> Response<RouteBody> {
    answer.map(Either::Left)
}

/// The host the certificate of `peer` names, if it may read; else the code of the 403.
fn caller(coordinator: &Coordinator, peer: &Peer) -> Result<Identity, &'static str> {
    let Some(identity) = peer
        .as_deref()
        .and_then(|der| coordinator.identify(der).ok())
    else {
        tracing::warn!("refused a read of a repository: no certificate that names a host");
        return Err("forbidden");
    };
    if coordinator.admission(&identity) != Admission::Admitted {
        tracing::warn!(node_id = %identity.host_id, "refused a read of a repository: the host is denied");
        return Err(DENIED);
    }
    Ok(identity)
}

/// What a path under [`PREFIX`] asks for.
struct Wanted<'a> {
    name: &'a str,
    /// `dists` or `pool`.
    kind: &'a str,
    group: &'a str,
    rest: &'a str,
}

impl<'a> Wanted<'a> {
    /// The ask in `path`, if it is `<name>/<dists|pool>/<group>/<rest>` with a name and a rest
    /// that pass their grammar.
    fn of(path: &'a str) -> Option<Self> {
        let mut parts = path.strip_prefix(PREFIX)?.splitn(4, '/');
        let (name, kind, group, rest) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        let fits = is_repository_name(name)
            && ["dists", "pool"].contains(&kind)
            && check_locator(rest).is_ok();
        fits.then_some(Self {
            name,
            kind,
            group,
            rest,
        })
    }

    /// The path under the repository's `uri`.
    fn under_uri(&self) -> String {
        format!("{}/{}/{}", self.kind, self.group, self.rest)
    }
}

/// Answers the `GET` or other request for a path under [`PREFIX`] from `peer`.
pub(crate) async fn serve<B>(
    coordinator: &Coordinator,
    peer: &Peer,
    request: Request<B>,
) -> Response<RouteBody> {
    if request.method() != Method::GET {
        return plain(error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"));
    }
    let identity = match caller(coordinator, peer) {
        Ok(identity) => identity,
        Err(code) => return plain(error(StatusCode::FORBIDDEN, code)),
    };
    let path = request.uri().path();
    let not_found = || plain(error(StatusCode::NOT_FOUND, "not_found"));
    let Some(wanted) = Wanted::of(path) else {
        return not_found();
    };
    let group = group_of(coordinator, &identity);
    let found = coordinator.desired.with_repositories(|registry| {
        registry
            .get(wanted.name)
            .map(|kept| &kept.repository)
            .filter(|entry| {
                registry::is_private(entry)
                    && wanted.group == group
                    && entry.suites.contains(&group)
            })
            .map(|entry| (entry.uri.clone(), entry.credential_file.clone()))
    });
    let Some((uri, credential_file)) = found else {
        tracing::warn!(node_id = %identity.host_id, %path, "refused a read of a repository: it is not a private repository of the host's group");
        return not_found();
    };
    let under = wanted.under_uri();
    match read(coordinator, &uri, &credential_file, &under).await {
        Ok(Origin::Found { length, body }) => {
            let mut response = Response::new(Either::Right(tonic::body::Body::new(body)));
            let headers = response.headers_mut();
            headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            response
        }
        Ok(Origin::NotFound) => not_found(),
        Err(why) => {
            tracing::warn!(repository = %wanted.name, path = %under, %why, "the origin of a private repository failed");
            plain(error(StatusCode::BAD_GATEWAY, "origin_failed"))
        }
    }
}

/// The origin's answer to a read of `under` the repository at `uri`, with the credential of the
/// file at `credential_file`, read now.
async fn read(
    coordinator: &Coordinator,
    uri: &str,
    credential_file: &str,
    under: &str,
) -> Result<Origin, Failure> {
    let origin = coordinator
        .repository_origin
        .as_ref()
        .ok_or(Failure::NoOrigin)?;
    let line = repository_credential::read(credential_file)?;
    Ok(origin.get(uri, under, &line).await?)
}
