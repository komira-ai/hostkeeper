//! The operator client against a loopback gRPC server with TLS (127.0.0.1, port 0) and a CA
//! made at run time, and the canonical JSON of a message against a descriptor set built here.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf as _, BufMut as _, Bytes};
use prost::Message as _;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{
    DescriptorProto, EnumDescriptorProto, EnumValueDescriptorProto, FieldDescriptorProto,
    FileDescriptorProto, FileDescriptorSet,
};
use tonic::body::Body;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::Service;
use tonic::codegen::http::{self, uri::PathAndQuery};
use tonic::server::{NamedService, UnaryService};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};

use super::{Channel, JsonError, Operator, OperatorError, canonical_json};

/// The codec of a call whose messages are already encoded: bytes in, bytes out.
#[derive(Debug, Clone, Copy, Default)]
struct RawCodec;

struct RawEncoder;

struct RawDecoder;

impl Encoder for RawEncoder {
    type Item = Bytes;
    type Error = Status;

    fn encode(&mut self, item: Bytes, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl Decoder for RawDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

impl Codec for RawCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = RawEncoder;
    type Decoder = RawDecoder;

    fn encoder(&mut self) -> RawEncoder {
        RawEncoder
    }

    fn decoder(&mut self) -> RawDecoder {
        RawDecoder
    }
}

/// A service, `hostkeeper.fixture.v1.Echo`, that answers the request `auth` with the
/// `authorization` metadata it carried (`none` when it carried none), the request `refuse` with
/// PERMISSION_DENIED `forbidden`, and any other request with its own bytes.
#[derive(Clone)]
struct Echo;

impl NamedService for Echo {
    const NAME: &'static str = "hostkeeper.fixture.v1.Echo";
}

struct Answer;

impl UnaryService<Bytes> for Answer {
    type Response = Bytes;
    type Future = std::future::Ready<Result<Response<Bytes>, Status>>;

    fn call(&mut self, request: Request<Bytes>) -> Self::Future {
        let bearer = request
            .metadata()
            .get("authorization")
            .map_or(Bytes::from_static(b"none"), |value| {
                Bytes::copy_from_slice(value.as_encoded_bytes())
            });
        let body = request.into_inner();
        std::future::ready(match &body[..] {
            b"refuse" => Err(Status::permission_denied("forbidden")),
            b"auth" => Ok(Response::new(bearer)),
            _ => Ok(Response::new(body)),
        })
    }
}

impl Service<http::Request<Body>> for Echo {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<http::Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        Box::pin(async move {
            Ok(tonic::server::Grpc::new(RawCodec)
                .unary(Answer, request)
                .await)
        })
    }
}

/// A CA, and a server certificate it signed for `localhost` and `127.0.0.1`.
struct Pki {
    ca_pem: String,
    server_cert: String,
    server_key: String,
}

fn pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().expect("a CA key");
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("a CA");
    let server_key = rcgen::KeyPair::generate().expect("a server key");
    let server =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("server params")
            .signed_by(&server_key, &*ca)
            .expect("a server certificate");
    Pki {
        ca_pem: ca.pem(),
        server_cert: server.pem(),
        server_key: server_key.serialize_pem(),
    }
}

/// A loopback gRPC server with TLS serving [`Echo`] with the server certificate of `pki`; it
/// stops when dropped.
struct Loopback {
    _runtime: tokio::runtime::Runtime,
    port: u16,
}

impl Loopback {
    fn start(pki: &Pki) -> Loopback {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let tls =
            ServerTlsConfig::new().identity(Identity::from_pem(&pki.server_cert, &pki.server_key));
        let server = Server::builder()
            .tls_config(tls)
            .expect("TLS")
            .serve_with_incoming(
                Echo,
                tokio_stream::wrappers::TcpListenerStream::new(listener),
            );
        runtime.spawn(server);
        Loopback {
            _runtime: runtime,
            port,
        }
    }
}

/// One call of `Echo/Say` with `body` through `operator`.
fn say(operator: &Operator, body: &'static [u8]) -> Result<Bytes, Status> {
    operator.call(|channel: Channel| async move {
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;
        let path = PathAndQuery::from_static("/hostkeeper.fixture.v1.Echo/Say");
        grpc.unary(Request::new(Bytes::from_static(body)), path, RawCodec)
            .await
    })
}

/// Proves: a call reaches the server over TLS, by its DNS name and by its IP address, with the
/// operator's token as `Bearer <token>`, and a status the server answers comes back as given.
///
/// Catches: a call sent without the token or with it spelled otherwise, a channel that trusts
/// another root than the CA given or checks another name than the endpoint's host, and a
/// refusal turned into another code or message.
#[test]
fn a_call_carries_the_bearer_over_tls_and_a_status_comes_back_as_given() {
    let pki = pki();
    let server = Loopback::start(&pki);
    for host in ["localhost", "127.0.0.1"] {
        let endpoint = format!("https://{host}:{}", server.port);
        let operator = Operator::connect(&endpoint, pki.ca_pem.as_bytes(), "op-token").expect(host);
        assert_eq!(
            say(&operator, b"auth").expect(host),
            Bytes::from_static(b"Bearer op-token")
        );
        assert_eq!(
            say(&operator, b"\x0a\x01x").expect(host),
            Bytes::from_static(b"\x0a\x01x")
        );
        let refused = say(&operator, b"refuse").expect_err(host);
        assert_eq!(
            (refused.code(), refused.message()),
            (Code::PermissionDenied, "forbidden")
        );
    }
}

/// Proves: a server whose certificate another CA signed gets no request; the call fails
/// UNAVAILABLE.
///
/// Catches: a channel that trusts any certificate, or the host's own roots.
#[test]
fn a_server_another_ca_signed_is_not_trusted() {
    let server = Loopback::start(&pki());
    let other = pki();
    let operator = Operator::connect(
        &format!("https://localhost:{}", server.port),
        other.ca_pem.as_bytes(),
        "op-token",
    )
    .expect("an operator");
    let failed = say(&operator, b"auth").expect_err("an untrusted server");
    assert_eq!(failed.code(), Code::Unavailable, "{failed:?}");
}

/// Proves: an endpoint that is not exactly `https://<host>[:<port>]`, a token that cannot be a
/// bearer, and a CA that is not a certificate are refused when the operator is made, by name.
///
/// Catches: a plain-text, user-bearing, path-bearing or query-bearing endpoint accepted (the
/// token would go elsewhere than the coordinator's root, or in the clear), a token with a
/// space or a line break sent, and a broken CA found only at the first call.
#[test]
fn a_bad_endpoint_token_or_ca_is_refused_when_the_operator_is_made() {
    let ca = pki().ca_pem;
    for endpoint in [
        "http://localhost:1",
        "https://user@localhost:1",
        "https://localhost:1/path",
        "https://localhost:1/",
        "https://localhost:1?q=1",
        "localhost:1",
        "not a url",
    ] {
        let refused = Operator::connect(endpoint, ca.as_bytes(), "t")
            .err()
            .expect(endpoint);
        assert_eq!(
            refused.to_string(),
            format!("{endpoint:?} is not https://<host>[:<port>]")
        );
    }
    for token in ["", "two words", "line\nbreak", "é"] {
        let refused = Operator::connect("https://localhost:1", ca.as_bytes(), token)
            .err()
            .expect(token);
        assert!(matches!(refused, OperatorError::Token), "{token:?}");
        assert_eq!(
            refused.to_string(),
            "the token is not a bearer token of visible ASCII"
        );
    }
    let broken = b"-----BEGIN CERTIFICATE-----\nnot base64\n-----END CERTIFICATE-----\n";
    let refused = Operator::connect("https://localhost:1", broken, "t")
        .err()
        .expect("a broken CA");
    assert!(
        refused.to_string().starts_with("the CA is not usable: "),
        "{refused}"
    );
    // An IPv6 host keeps its brackets in the URI and is checked without them.
    Operator::connect("https://[::1]:1", ca.as_bytes(), "t").expect("an IPv6 endpoint");
}

/// A descriptor set of one file, `fixture.proto`, package `fixture`: an enum `State`
/// (`STATE_UNSPECIFIED`, `STATE_ON`) and a message `Host` (`node_id` string 1, `enrolled_unix`
/// uint64 2, `state` State 3, `count` uint32 4); and `Other` with `name` uint64 1.
fn descriptors() -> Vec<u8> {
    fn field(name: &str, number: i32, kind: Type, json: &str) -> FieldDescriptorProto {
        FieldDescriptorProto {
            name: Some(name.to_owned()),
            number: Some(number),
            label: Some(Label::Optional as i32),
            r#type: Some(kind as i32),
            json_name: Some(json.to_owned()),
            ..FieldDescriptorProto::default()
        }
    }
    let mut state = field("state", 3, Type::Enum, "state");
    state.type_name = Some(".fixture.State".to_owned());
    let value = |name: &str, number| EnumValueDescriptorProto {
        name: Some(name.to_owned()),
        number: Some(number),
        options: None,
    };
    let file = FileDescriptorProto {
        name: Some("fixture.proto".to_owned()),
        package: Some("fixture".to_owned()),
        syntax: Some("proto3".to_owned()),
        enum_type: vec![EnumDescriptorProto {
            name: Some("State".to_owned()),
            value: vec![value("STATE_UNSPECIFIED", 0), value("STATE_ON", 1)],
            ..EnumDescriptorProto::default()
        }],
        message_type: vec![
            DescriptorProto {
                name: Some("Host".to_owned()),
                field: vec![
                    field("node_id", 1, Type::String, "nodeId"),
                    field("enrolled_unix", 2, Type::Uint64, "enrolledUnix"),
                    state,
                    field("count", 4, Type::Uint32, "count"),
                ],
                ..DescriptorProto::default()
            },
            DescriptorProto {
                name: Some("Other".to_owned()),
                field: vec![field("name", 1, Type::Uint64, "name")],
                ..DescriptorProto::default()
            },
        ],
        ..FileDescriptorProto::default()
    };
    FileDescriptorSet { file: vec![file] }.encode_to_vec()
}

/// `fixture.Host`, as the generated code of `descriptors` would be.
#[derive(Clone, PartialEq, prost::Message)]
struct Host {
    #[prost(string, tag = "1")]
    node_id: String,
    #[prost(uint64, tag = "2")]
    enrolled_unix: u64,
    #[prost(int32, tag = "3")]
    state: i32,
    #[prost(uint32, tag = "4")]
    count: u32,
}

/// Proves: a message's JSON names its fields by their JSON names, its enums by their value
/// names and its 64-bit integers as strings, and leaves out a field at its default.
///
/// Catches: proto field names (`node_id`) where the gateway answers `nodeId`, enum numbers, a
/// 64-bit integer as a JSON number, and a default field printed.
#[test]
fn a_message_is_its_canonical_json() {
    let host = Host {
        node_id: "h1".to_owned(),
        enrolled_unix: 1_700_000_000,
        state: 1,
        count: 0,
    };
    assert_eq!(
        canonical_json(&descriptors(), "fixture.Host", &host).expect("JSON"),
        serde_json::json!({"nodeId": "h1", "enrolledUnix": "1700000000", "state": "STATE_ON"})
    );
    assert_eq!(
        canonical_json(&descriptors(), "fixture.Host", &Host::default()).expect("JSON"),
        serde_json::json!({})
    );
}

/// Proves: a descriptor set that does not decode, a name it lacks, and bytes of another message
/// are each refused by name.
///
/// Catches: a reply read against the wrong message turned into JSON of nonsense, or an empty
/// object, instead of an error.
#[test]
fn a_bad_descriptor_set_an_unknown_name_and_bytes_of_another_message_are_refused() {
    let host = Host {
        node_id: "h1".to_owned(),
        ..Host::default()
    };
    let refused = canonical_json(b"\xff", "fixture.Host", &host).expect_err("not a set");
    assert!(matches!(refused, JsonError::Descriptors(_)), "{refused}");
    assert!(
        refused
            .to_string()
            .starts_with("the descriptor set does not decode: "),
        "{refused}"
    );
    let refused = canonical_json(&descriptors(), "fixture.Gone", &host).expect_err("unknown");
    assert_eq!(
        refused.to_string(),
        "the descriptor set has no message fixture.Gone"
    );
    // Field 1 of `Other` is a varint; `Host` sends a length-delimited string in it.
    let refused = canonical_json(&descriptors(), "fixture.Other", &host).expect_err("another");
    assert!(
        refused
            .to_string()
            .starts_with("the bytes are not a fixture.Other: "),
        "{refused}"
    );
}
