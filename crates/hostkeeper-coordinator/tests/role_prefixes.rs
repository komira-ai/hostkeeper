//! The certificate namespaces: the URIs a certificate names its role and its host's role by are
//! `--role-uri-prefix` and `--host-role-uri-prefix`, which default to the project's own and are
//! set to kbf's by a farm whose certificates are kbf's.

mod support;

use std::ffi::OsString;
use std::sync::Arc;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, Command};
use hostkeeper_coordinator::http::{Peer, Surface};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, SanPrefixes,
};
use support::rig::{Rig, Spec, request};
use support::{BEAT, kbf_prefixes, self_signed, updater_der};

fn peer(der: Vec<u8>) -> Peer {
    Some(Arc::from(der.as_slice()))
}

async fn answer(rig: &Rig, who: Peer) -> (u16, String) {
    rig.call(
        Surface::Heartbeat,
        &who,
        request("POST", "/v1/heartbeat", None, BEAT),
    )
    .await
}

/// The flags of a `serve` with the files it needs named and `extra` besides.
fn serve(extra: &[&str]) -> hostkeeper_coordinator::config::Serve {
    let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
    for (flag, value) in [
        ("--heartbeat-listen", "192.0.2.1:1"),
        ("--private-listen", "192.0.2.1:2"),
        ("--tls-cert", "tls.pem"),
        ("--tls-key", "tls.key"),
        ("--client-ca", "ca.pem"),
        ("--state-file", "desired.json"),
        ("--oidc-config", "oidc.toml"),
        ("--operator-token-file", "operator.token"),
    ] {
        args.push(flag.into());
        args.push(value.into());
    }
    args.extend(extra.iter().map(OsString::from));
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags
}

/// Proves: the namespaces are flags. A coordinator run with kbf's namespaces (the recorded
/// flags) answers a kbf certificate and refuses a `urn:hostkeeper` one with 403, a coordinator
/// run with the defaults does the reverse, and a certificate that mixes the two refuses in both.
/// The flags are the ones `config::Cli` reads: with neither given the namespaces are the
/// defaults, with both they are what was given.
#[tokio::test]
async fn role_prefix_flags_accept_old_certificates_and_the_new_default_refuses_them() {
    let defaults = SanPrefixes::default();
    assert_eq!(defaults.role(), DEFAULT_ROLE_URI_PREFIX);
    assert_eq!(defaults.host_role(), DEFAULT_HOST_ROLE_URI_PREFIX);
    let kbf = kbf_prefixes();
    let forbidden = r#"{"error":"forbidden"}"#;
    let mixed = || {
        peer(self_signed(&[
            "urn:kbf:role:updater",
            &format!("{}storage", defaults.host_role()),
            "dns:s1",
        ]))
    };

    let old = Rig::new("prefixes-kbf", Spec::recorded());
    old.at(1_100);
    let (code, body) = answer(&old, peer(updater_der("s1", "storage", &kbf))).await;
    assert_eq!((code, body.as_str()), (200, r#"{"reply":"nothing"}"#));
    let (code, body) = answer(&old, peer(updater_der("s1", "storage", &defaults))).await;
    assert_eq!(
        (code, body.as_str()),
        (403, forbidden),
        "the new default's certificate"
    );
    let (code, body) = answer(&old, mixed()).await;
    assert_eq!((code, body.as_str()), (403, forbidden), "half of each");
    assert_eq!(old.status().await["hosts"][0]["node_id"], "s1");
    assert_eq!(
        old.status().await["hosts"].as_array().map(Vec::len),
        Some(1)
    );

    let new = Rig::new(
        "prefixes-default",
        Spec::recorded().with_prefixes(DEFAULT_ROLE_URI_PREFIX, DEFAULT_HOST_ROLE_URI_PREFIX),
    );
    new.at(1_100);
    let (code, body) = answer(&new, peer(updater_der("s1", "storage", &defaults))).await;
    assert_eq!((code, body.as_str()), (200, r#"{"reply":"nothing"}"#));
    let (code, body) = answer(&new, peer(updater_der("s1", "storage", &kbf))).await;
    assert_eq!((code, body.as_str()), (403, forbidden), "kbf's certificate");
    let (code, body) = answer(&new, mixed()).await;
    assert_eq!((code, body.as_str()), (403, forbidden), "half of each");
    assert_eq!(
        new.status().await["hosts"].as_array().map(Vec::len),
        Some(1)
    );

    let flags = serve(&[]);
    assert_eq!(
        SanPrefixes::new(&flags.role_uri_prefix, &flags.host_role_uri_prefix).expect("prefixes"),
        defaults,
        "neither flag given"
    );
    let flags = serve(&[
        "--role-uri-prefix",
        "urn:kbf:role:",
        "--host-role-uri-prefix",
        "urn:kbf:host-role:",
    ]);
    assert_eq!(
        SanPrefixes::new(&flags.role_uri_prefix, &flags.host_role_uri_prefix).expect("prefixes"),
        kbf,
        "both flags given"
    );
}
