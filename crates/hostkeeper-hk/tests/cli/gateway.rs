//! The real gateway behind a loopback HTTPS server: the coordinator `hostkeeper_coordinator::
//! config::build` makes from `serve`'s flags, as the binary makes it, answering each request
//! `hk` sends through `hostkeeper_coordinator::grpc::route`, the entry point of its listeners, on
//! the private surface. The server's TLS is the rig's; the rest of the path (the bearer check,
//! the JSON body, the refusal body) is the coordinator's own.
//!
//! Nothing here reaches past the loopback: the issuer's keys are never fetched (no test sends
//! the coordinator a JWT), and the coordinator's own TLS files are read, never served.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use clap::Parser as _;
use hostkeeper_coordinator::config::{Cli, Command, build};
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt as _, Full};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};

use crate::rig::{Pki, Reply, Request, Server, TOKEN};

/// How long the router waits for a request's body; every body here is whole when it is sent.
const PATIENCE: Duration = Duration::from_secs(30);

/// A self-signed certificate for `localhost` and its key, as PEM, and the certificate as DER.
fn issue(ca: bool) -> (String, String, Vec<u8>) {
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
    if ca {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    }
    let key = KeyPair::generate().expect("a key");
    let cert = params.self_signed(&key).expect("a certificate");
    (cert.pem(), key.serialize_pem(), cert.der().to_vec())
}

/// Writes `text` at `path` with `mode`; answers the path as text.
fn put(path: PathBuf, text: &str, mode: u32) -> String {
    std::fs::write(&path, text).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    path.display().to_string()
}

/// The coordinator's state in a scratch directory, and the arguments of its `serve`: the tokens
/// file holds [`TOKEN`]'s SHA-256.
fn serve_args(dir: &Path) -> Vec<OsString> {
    let (server_pem, server_key, _) = issue(false);
    let (clients_pem, _, _) = issue(true);
    let digest: String = Sha256::digest(TOKEN.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let tokens = format!("[[token]]\nname = \"hk-test\"\nsha256 = \"{digest}\"\n");
    let pairs = [
        ("--heartbeat-listen", "192.0.2.1:1".to_owned()),
        ("--private-listen", "192.0.2.1:2".to_owned()),
        ("--tls-cert", put(dir.join("tls.pem"), &server_pem, 0o644)),
        ("--tls-key", put(dir.join("tls.key"), &server_key, 0o600)),
        ("--client-ca", put(dir.join("ca.pem"), &clients_pem, 0o644)),
        (
            "--state-file",
            dir.join("desired.json").display().to_string(),
        ),
        (
            "--oidc-config",
            put(dir.join("oidc.toml"), &config_text(STATIC_ISSUER), 0o644),
        ),
        (
            "--tokens-file",
            put(dir.join("tokens.toml"), &tokens, 0o600),
        ),
        ("--legacy-service", "kbf".to_owned()),
    ];
    let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
    for (flag, value) in pairs {
        args.push(flag.into());
        args.push(value.into());
    }
    args
}

/// The request `hk` sent, as the router takes it.
fn request_of(request: &Request) -> hyper::Request<Full<Bytes>> {
    let mut builder = hyper::Request::builder()
        .method(request.method.as_str())
        .uri(request.target.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(Full::new(Bytes::copy_from_slice(&request.body)))
        .expect("a request")
}

/// The real gateway on a loopback server of port 0, its state in a directory under `scratch`.
pub fn start(pki: &Pki, scratch: &Path) -> Server {
    let dir = Scratch::under(scratch, "coordinator").expect("the coordinator's directory");
    Server::serve(
        pki,
        move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            let Command::Serve(flags) = Cli::try_parse_from(serve_args(dir.path()))
                .expect("the flags parse")
                .command
            else {
                panic!("the verb is serve");
            };
            // The roots the issuer's keys would be fetched trusting: a certificate of the test,
            // so nothing reads the host's store. No key is fetched here.
            let roots = Roots::Given(vec![CertificateDer::from(issue(true).2)]);
            let parts = {
                let _entered = runtime.enter();
                build(&flags, roots).expect("the coordinator starts")
            };
            // The coordinator manages no service of its own: the calls of the tests name `kbf`,
            // a package of this group.
            runtime.block_on(async {
                let operator = parts.coordinator.caller(Some(TOKEN)).await;
                parts
                    .coordinator
                    .host_groups_put(
                        &operator,
                        proto::PutHostGroupRequest {
                            host_group: Some(proto::HostGroup {
                                name: "farm".to_owned(),
                                packages: vec!["hostkeeper".to_owned(), "kbf".to_owned()],
                                policy: None,
                            }),
                            if_generation: None,
                            reason: "the package the calls name".to_owned(),
                        },
                    )
                    .expect("the group is put");
            });
            (runtime, parts, dir)
        },
        |(runtime, parts, _), request| -> Reply {
            runtime.block_on(async {
                let response = route(
                    &parts.coordinator,
                    Surface::Control,
                    &None,
                    request_of(request),
                    PATIENCE,
                )
                .await;
                let status = response.status().as_u16();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .map(|c| c.to_bytes().to_vec())
                    .unwrap_or_default();
                Reply::new(status, bytes)
            })
        },
    )
}
