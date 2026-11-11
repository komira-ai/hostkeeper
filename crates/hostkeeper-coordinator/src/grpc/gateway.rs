//! The HTTP/JSON gateway of the operator API: `POST /<package>.<Service>/<Method>` on the
//! control listener, with the request as proto3 canonical JSON and the token as
//! `Authorization: Bearer <token>` (`docs/gateway.md`), or, from a signed-in browser, the session
//! cookie and the CSRF header (`docs/design/operator-login.md`, section 5.3).
//!
//! The gateway has one rule of its own, the null step of `Releases.SetDesired`
//! ([`without_null_targets`]). It checks the caller as a gRPC call of the same path is
//! checked ([`admit_via`]), before the body is read; the gateway is the one place the session
//! cookie of a signed-in browser is read; reads a body of at most the method's limit
//! ([`operator_limit`]); turns the JSON into the method's request message with the protobuf
//! descriptors; calls the same in-process gRPC service a gRPC call
//! reaches ([`serve_admitted`]); and turns the response message back into JSON. A refusal is
//! `{"code":"<gRPC code name>","reason":"<token>"}` with the HTTP status of [`STATUS_TABLE`].
//! The call has no deadline here beyond the read of its body: a held `Releases.WaitConverged`
//! ends by its own `wait_secs`.
//!
//! A path is a gateway path when it names a unary method of `hostkeeper.operator.v1`. The agent
//! and local packages are never served, and the gateway adds no other route.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::header::{CONTENT_TYPE, HeaderValue, TE};
use hyper::{Method, Request, Response, Version};
use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, MethodDescriptor, SerializeOptions,
};
use tonic::{Code, Status};

use super::{SESSION_ONLY, SET_DESIRED, Transport, admit_via, operator_limit, serve_admitted};
use crate::http::{BoxError, Peer, Surface, read_body};
use crate::service::Coordinator;

/// The one protobuf package the gateway serves.
const PACKAGE: &str = "hostkeeper.operator.v1";

/// The reason of the null step's refusal ([`without_null_targets`]): a set's `targets` that name
/// no platform, the token `Releases` refuses such targets with.
const TARGET_INVALID: &str = "target_invalid";

/// The reason of a refusal the gateway makes before any adapter runs: a body that is not the
/// method's request as JSON. It is a token of every refusal enum of the package.
const INVALID_REQUEST: &str = "invalid_request";

/// Each gRPC status code, in gRPC number order, with its name (the one `hkctl` and the web app
/// read) and the HTTP status the gateway answers it with.
pub const STATUS_TABLE: [(Code, &str, u16); 17] = [
    (Code::Ok, "OK", 200),
    (Code::Cancelled, "CANCELLED", 499),
    (Code::Unknown, "UNKNOWN", 500),
    (Code::InvalidArgument, "INVALID_ARGUMENT", 400),
    (Code::DeadlineExceeded, "DEADLINE_EXCEEDED", 504),
    (Code::NotFound, "NOT_FOUND", 404),
    (Code::AlreadyExists, "ALREADY_EXISTS", 409),
    (Code::PermissionDenied, "PERMISSION_DENIED", 403),
    (Code::ResourceExhausted, "RESOURCE_EXHAUSTED", 429),
    (Code::FailedPrecondition, "FAILED_PRECONDITION", 400),
    (Code::Aborted, "ABORTED", 409),
    (Code::OutOfRange, "OUT_OF_RANGE", 400),
    (Code::Unimplemented, "UNIMPLEMENTED", 501),
    (Code::Internal, "INTERNAL", 500),
    (Code::Unavailable, "UNAVAILABLE", 503),
    (Code::DataLoss, "DATA_LOSS", 500),
    (Code::Unauthenticated, "UNAUTHENTICATED", 401),
];

/// The descriptors of every message and service, decoded once. `None` when the embedded set does
/// not decode, and then no path is a gateway path.
static POOL: LazyLock<Option<DescriptorPool>> =
    LazyLock::new(|| pool(hostkeeper_proto::FILE_DESCRIPTOR_SET));

/// The descriptors of the encoded descriptor set `bytes`, if it decodes.
pub(crate) fn pool(bytes: &[u8]) -> Option<DescriptorPool> {
    DescriptorPool::decode(bytes).ok()
}

/// The method a gateway path names: `/<package>.<Service>/<Method>`.
pub(crate) fn is_method(path: &str) -> Option<MethodDescriptor> {
    find(POOL.as_ref()?, PACKAGE, path)
}

/// The unary method of `package` at `path` in `pool`.
pub(crate) fn find(pool: &DescriptorPool, package: &str, path: &str) -> Option<MethodDescriptor> {
    let (service, name) = path.strip_prefix('/')?.split_once('/')?;
    let method = pool
        .get_service_by_name(service)?
        .methods()
        .find(|m| m.name() == name)?;
    let served = method.parent_service().package_name() == package
        && !method.is_client_streaming()
        && !method.is_server_streaming();
    served.then_some(method)
}

/// Answers one gateway call from `peer`: the request is a `POST` of a path [`is_method`] names.
/// A body that has not arrived whole after `patience` is an invalid request.
pub(crate) async fn answer<B>(
    coordinator: &Arc<Coordinator>,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Response<Full<Bytes>>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let Some(method) = is_method(path) else {
        return refusal(&Status::not_found(""));
    };
    let transport = Transport::Gateway {
        authority: crate::login::authority(&parts.headers, &parts.uri),
    };
    let admitted = match admit_via(
        coordinator,
        Surface::Control,
        path,
        peer,
        &parts.headers,
        transport,
        SESSION_ONLY,
    )
    .await
    {
        Ok(admitted) => admitted,
        Err(status) => return refusal(&status),
    };
    let Some(bytes) = read_body(body, operator_limit(path), patience).await else {
        return refusal(&invalid());
    };
    let bytes = if path == SET_DESIRED {
        match without_null_targets(&bytes) {
            Ok(bytes) => bytes,
            Err(status) => return refusal(&status),
        }
    } else {
        bytes
    };
    let Some(message) = to_message(&method, &bytes) else {
        return refusal(&invalid());
    };

    let mut call = Request::new(Full::new(frame(&message.encode_to_vec())));
    *call.method_mut() = Method::POST;
    *call.uri_mut() = parts.uri.clone();
    *call.version_mut() = Version::HTTP_2;
    call.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    call.headers_mut()
        .insert(TE, HeaderValue::from_static("trailers"));
    // What the listener put in the request (the connection's held waits) goes to the call.
    *call.extensions_mut() = parts.extensions;
    let response = serve_admitted(coordinator, admitted, call).await;

    match reply(&method, response).await {
        Ok(json) => success(json),
        Err(status) => refusal(&status),
    }
}

/// The refusal for a body the method's request cannot be read from.
fn invalid() -> Status {
    Status::invalid_argument(INVALID_REQUEST)
}

/// The JSON body of a `Releases.SetDesired`, `bytes`, with each key of its `targets` object whose
/// value is `null` dropped: a platform with no list, which proto3 JSON cannot hold as a map's
/// value (docs/design/packages.md, section 3.2). A body that is no JSON object, or whose `targets`
/// is not an object holding a `null`, is passed on as it came.
///
/// # Errors
/// `target_invalid` (`INVALID_ARGUMENT`): a `targets` object that names no platform once its
/// `null`s are dropped, as a set with targets that names none is refused.
pub(crate) fn without_null_targets(bytes: &Bytes) -> Result<Bytes, Status> {
    let Ok(mut body) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(bytes.clone());
    };
    let Some(targets) = body
        .get_mut("targets")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(bytes.clone());
    };
    if targets.is_empty() {
        return Err(Status::invalid_argument(TARGET_INVALID));
    }
    if !targets.values().any(serde_json::Value::is_null) {
        return Ok(bytes.clone());
    }
    targets.retain(|_, list| !list.is_null());
    if targets.is_empty() {
        return Err(Status::invalid_argument(TARGET_INVALID));
    }
    Ok(Bytes::from(body.to_string()))
}

/// The request message of `method` in the JSON `bytes`: one document, nothing after it, and no
/// field the message does not have.
pub(crate) fn to_message(method: &MethodDescriptor, bytes: &[u8]) -> Option<DynamicMessage> {
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let message = DynamicMessage::deserialize_with_options(
        method.input(),
        &mut reader,
        &DeserializeOptions::new(),
    )
    .ok()?;
    reader.end().ok()?;
    Some(message)
}

/// `message` as one uncompressed gRPC message: a zero flag, its length as four big-endian
/// bytes, and the message.
pub(crate) fn frame(message: &[u8]) -> Bytes {
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(message.len() + 5);
    framed.push(0);
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(message);
    Bytes::from(framed)
}

/// The message inside one uncompressed gRPC message `framed`, if `framed` is exactly one.
pub(crate) fn unframe(framed: &[u8]) -> Option<&[u8]> {
    let (head, message) = framed.split_at_checked(5)?;
    let length = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
    (head[0] == 0 && usize::try_from(length).ok()? == message.len()).then_some(message)
}

/// The JSON of the response message of `method` in the gRPC `response`, or its status. The
/// status is read from the trailers, or from the headers of a response that has no body.
pub(crate) async fn reply(
    method: &MethodDescriptor,
    response: Response<tonic::body::Body>,
) -> Result<Vec<u8>, Status> {
    let (parts, body) = response.into_parts();
    let collected = body.collect().await?;
    let status = collected
        .trailers()
        .and_then(Status::from_header_map)
        .or_else(|| Status::from_header_map(&parts.headers))
        .unwrap_or_else(|| Status::internal(""));
    if status.code() != Code::Ok {
        return Err(status);
    }
    let bytes = collected.to_bytes();
    to_json(method, &bytes).ok_or_else(|| Status::internal(""))
}

/// The canonical JSON of the response message of `method` in the gRPC message `framed`.
pub(crate) fn to_json(method: &MethodDescriptor, framed: &[u8]) -> Option<Vec<u8>> {
    let message = DynamicMessage::decode(method.output(), unframe(framed)?).ok()?;
    let mut writer = serde_json::Serializer::new(Vec::new());
    message
        .serialize_with_options(&mut writer, &SerializeOptions::new())
        .ok()?;
    Some(writer.into_inner())
}

/// A 200 with `json`.
fn success(json: Vec<u8>) -> Response<Full<Bytes>> {
    respond(200, json)
}

/// The answer to a call refused with `status`: its code's name and HTTP status, and its message
/// as the reason.
pub(crate) fn refusal(status: &Status) -> Response<Full<Bytes>> {
    // The table is indexed by the code's number, 0 to 16, which is every code there is.
    let (_, name, http) = STATUS_TABLE[status.code() as usize];
    let body = serde_json::json!({ "code": name, "reason": status.message() });
    respond(http, body.to_string().into_bytes())
}

/// An answer of `http` status with the JSON `body`.
fn respond(http: u16, body: Vec<u8>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(http)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default()
}
