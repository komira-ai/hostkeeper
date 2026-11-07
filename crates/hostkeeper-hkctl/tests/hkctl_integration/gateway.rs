//! The real gateway behind a loopback listener: the coordinator `hostkeeper_coordinator::config::
//! build` makes from `serve`'s flags, as the binary makes it, answering each request the fake
//! curl forwards through `hostkeeper_coordinator::grpc::route`, the entry point of its listeners,
//! on the private surface. The fake speaks plain HTTP/1.1, so no TLS is served; the rest of the
//! path (the bearer check, the JSON body, the refusal body) is the coordinator's own.
//!
//! Nothing here reaches past the loopback: the issuer's keys are never fetched (no test sends
//! the coordinator a JWT), and the TLS files are read, never served.

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
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt as _, Full};
use hyper::Request;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};

use crate::rig::{Answer, Loopback, TOKEN};

/// How long the router waits for a request's body; every body here is whole when it is sent.
const PATIENCE: Duration = Duration::from_secs(30);

/// The services the coordinator manages.
const ROLLOUT: &str = r#"
[[service]]
name = "kbf"
roles = ["frontend", "storage"]
"#;

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
/// file holds [`TOKEN`]'s SHA-256, the rollout file [`ROLLOUT`].
fn serve_args(dir: &Path) -> Vec<OsString> {
    let (server_pem, server_key, _) = issue(false);
    let (clients_pem, _, _) = issue(true);
    let digest: String = Sha256::digest(TOKEN.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let tokens = format!("[[token]]\nname = \"hkctl-test\"\nsha256 = \"{digest}\"\n");
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
        (
            "--rollout-file",
            put(dir.join("rollout.toml"), ROLLOUT, 0o600),
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

/// The request a fake curl sent, as the router takes it.
fn request_of(head: &str, body: &[u8]) -> Request<Full<Bytes>> {
    let mut lines = head.lines();
    let mut start = lines.next().unwrap_or_default().split(' ');
    let method = start.next().unwrap_or_default();
    let target = start.next().unwrap_or_default();
    let mut builder = Request::builder().method(method).uri(target);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            builder = builder.header(name.trim(), value.trim());
        }
    }
    builder
        .body(Full::new(Bytes::copy_from_slice(body)))
        .expect("a request")
}

/// The real gateway on a loopback listener of port 0, its state in a directory under `scratch`.
pub fn start(scratch: &Path) -> Loopback {
    let dir = Scratch::under(scratch, "coordinator").expect("the coordinator's directory");
    Loopback::serve(
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
            (runtime, parts, dir)
        },
        |(runtime, parts, _), head, body| -> Answer {
            runtime.block_on(async {
                let response = route(
                    &parts.coordinator,
                    Surface::Control,
                    &None,
                    request_of(head, body),
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
                (status, bytes)
            })
        },
    )
}
