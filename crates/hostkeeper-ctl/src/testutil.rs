//! What the unit tests share: the fixture command table and descriptors `build.rs` generated
//! from `tests/fixtures`, a fake transport that records its calls, and a run of `ctl` on them.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use tonic::Status;
use tonic::metadata::MetadataMap;

use crate::transport::ConnectError;
use crate::{Connect, CtlTransport, Io, Target, run_with};

/// The table and descriptors of the fixture protos.
pub mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));

    /// The encoded descriptor set of the fixture protos, options kept.
    pub const DESCRIPTORS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fixture.binpb"));
}

/// The fixture descriptors, decoded.
pub fn pool() -> DescriptorPool {
    // One pool: descriptors of two pools never compare equal.
    static POOL: std::sync::LazyLock<DescriptorPool> = std::sync::LazyLock::new(|| {
        DescriptorPool::decode(fixture::DESCRIPTORS).expect("the fixture set decodes")
    });
    POOL.clone()
}

/// A fixture message type, by its name in `hostkeeper.fixture.v1`.
pub fn message(name: &str) -> MessageDescriptor {
    pool()
        .get_message_by_name(&format!("hostkeeper.fixture.v1.{name}"))
        .expect("a fixture message")
}

/// A fixture message from canonical JSON.
pub fn from_json(name: &str, json: &str) -> DynamicMessage {
    let mut de = serde_json::Deserializer::from_str(json);
    DynamicMessage::deserialize(message(name), &mut de).expect("fixture JSON")
}

/// One call a fake transport received.
#[derive(Debug, Clone)]
pub struct Call {
    pub path: String,
    pub request: Bytes,
    pub metadata: MetadataMap,
}

/// A transport that answers every call with one reply and records the calls and the targets
/// it was made for.
#[derive(Clone)]
pub struct Fake {
    reply: Result<Bytes, Status>,
    refuse_connect: bool,
    pub calls: Arc<Mutex<Vec<Call>>>,
    pub targets: Arc<Mutex<Vec<Target>>>,
}

impl Fake {
    /// Answers with `reply`, encoded.
    pub fn answering(reply: &DynamicMessage) -> Fake {
        Fake::with(Ok(Bytes::from(reply.encode_to_vec())))
    }

    /// Answers with `reply`, as given.
    pub fn with(reply: Result<Bytes, Status>) -> Fake {
        Fake {
            reply,
            refuse_connect: false,
            calls: Arc::default(),
            targets: Arc::default(),
        }
    }

    /// Refuses to make a transport.
    pub fn refusing_to_connect() -> Fake {
        Fake {
            refuse_connect: true,
            ..Fake::with(Ok(Bytes::new()))
        }
    }

    /// The calls made so far.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("lock").clone()
    }

    /// The targets transports were made for so far.
    pub fn targets(&self) -> Vec<Target> {
        self.targets.lock().expect("lock").clone()
    }
}

impl Connect for Fake {
    fn connect(&self, target: &Target) -> Result<Box<dyn CtlTransport>, ConnectError> {
        self.targets.lock().expect("lock").push(target.clone());
        if self.refuse_connect {
            return Err(ConnectError::Unusable {
                what: "the CA",
                path: target.ca_file.clone(),
                why: "refused by the test".to_owned(),
            });
        }
        Ok(Box::new(self.clone()))
    }
}

impl CtlTransport for Fake {
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status> {
        self.calls.lock().expect("lock").push(Call {
            path: path.to_owned(),
            request,
            metadata: metadata.clone(),
        });
        self.reply.clone()
    }
}

/// A writer, or a reader, that always fails.
pub struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("broken pipe, for the test"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("broken pipe, for the test"))
    }
}

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("unreadable, for the test"))
    }
}

/// What a run of `ctl` returned and printed.
#[derive(Debug)]
pub struct Ran {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}

/// The flags every call of the fixture needs.
pub const CALL: &[&str] = &[
    "--endpoint",
    "https://coordinator.example",
    "--ca-file",
    "ca.pem",
];

/// Runs `ctl` on the fixture table with `args` (after the program's name) and `stdin`.
pub fn run_fixture(args: &[&str], connect: &dyn Connect, stdin: &[u8]) -> Ran {
    run_on(
        fixture::COMMANDS,
        fixture::DESCRIPTORS,
        args,
        connect,
        stdin,
    )
}

/// Runs `ctl` on a table and descriptor set.
pub fn run_on(
    commands: &[hostkeeper_proto::ctl::Command],
    descriptors: &[u8],
    args: &[&str],
    connect: &dyn Connect,
    stdin: &[u8],
) -> Ran {
    let mut input = stdin;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_with(
        commands,
        descriptors,
        std::iter::once("ctl").chain(args.iter().copied()),
        connect,
        Io {
            stdin: &mut input,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    Ran {
        code,
        stdout: String::from_utf8(stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(stderr).expect("UTF-8 stderr"),
    }
}

/// `args` with the call flags after them.
pub fn with_call<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().chain(CALL.iter().copied()).collect()
}

/// A scratch directory of the unit tests, beside the test binary in the target directory.
pub fn scratch(name: &str) -> hostkeeper_testkit::Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base: PathBuf = exe
        .parent()
        .expect("its directory")
        .join("hostkeeper-ctl-unit");
    hostkeeper_testkit::Scratch::under(&base, name).expect("scratch")
}

/// Writes `contents` to `name` in `dir`, and returns its path as a string.
pub fn file(dir: &hostkeeper_testkit::Scratch, name: &str, contents: &[u8]) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write a fixture file");
    path.to_str().expect("a UTF-8 path").to_owned()
}

/// A test CA, a server certificate for 127.0.0.1 and a client certificate it signed, as PEM;
/// made at run time, so that no key is committed. Two more server certificates the same CA
/// signed are wrong in one way each: one is for another name, and one for 127.0.0.1 expired
/// in the past.
pub struct Pki {
    pub ca: String,
    pub server_cert: String,
    pub server_key: String,
    pub other_name_cert: String,
    pub other_name_key: String,
    pub expired_cert: String,
    pub expired_key: String,
    pub client_cert: String,
    pub client_key: String,
}

/// The parameters and key of a certificate named `name` for `san`, a CA when `ca` is set.
fn certificate(name: &str, san: &[&str], ca: bool) -> (rcgen::CertificateParams, rcgen::KeyPair) {
    let mut params =
        rcgen::CertificateParams::new(san.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
            .expect("params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    if ca {
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    }
    (params, rcgen::KeyPair::generate().expect("a key"))
}

/// A new PKI; two calls make two unrelated CAs.
pub fn pki() -> Pki {
    let (ca_params, ca_key) = certificate("hostkeeper test ca", &[], true);
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("the CA");
    let (server, server_key) = certificate("hostkeeper test server", &["127.0.0.1"], false);
    let server = server
        .signed_by(&server_key, &*ca)
        .expect("the server certificate");
    let (other_name, other_name_key) = certificate(
        "hostkeeper test server, other name",
        &["other.example"],
        false,
    );
    let other_name = other_name
        .signed_by(&other_name_key, &*ca)
        .expect("the other-name certificate");
    let (mut expired, expired_key) =
        certificate("hostkeeper test server, expired", &["127.0.0.1"], false);
    expired.not_before = rcgen::date_time_ymd(2026, 9, 2);
    expired.not_after = rcgen::date_time_ymd(2026, 9, 3);
    let expired = expired
        .signed_by(&expired_key, &*ca)
        .expect("the expired certificate");
    let (client, client_key) = certificate("host:test-client", &[], false);
    let client = client
        .signed_by(&client_key, &*ca)
        .expect("the client certificate");
    Pki {
        ca: ca.pem(),
        server_cert: server.pem(),
        server_key: server_key.serialize_pem(),
        other_name_cert: other_name.pem(),
        other_name_key: other_name_key.serialize_pem(),
        expired_cert: expired.pem(),
        expired_key: expired_key.serialize_pem(),
        client_cert: client.pem(),
        client_key: client_key.serialize_pem(),
    }
}

/// A target for the gRPC or gateway transport at `127.0.0.1:port`, trusting `ca_file`.
pub fn loopback_target(
    port: u16,
    transport: crate::TransportKind,
    ca_file: &str,
    client: Option<(&str, &str)>,
) -> Target {
    Target {
        endpoint: crate::Endpoint::parse(&format!("https://127.0.0.1:{port}")).expect("endpoint"),
        transport,
        ca_file: ca_file.into(),
        client_identity: client.map(|(c, k)| (c.into(), k.into())),
        timeout: std::time::Duration::from_secs(30),
    }
}
