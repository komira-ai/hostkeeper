//! The chains of the device blob of `/enroll` that need an issuer of another shape: a path-length
//! limit and a P-384 root. Each refusal is asserted by its variant and its text.

mod common;

use common::*;
use hostkeeper_mdm_ade::blob::verify;
use hostkeeper_mdm_ade::{Device, Error};

fn check(spec: &Spec) -> Result<Device, Error> {
    verify(&blob(spec), &roots(), now())
}

/// Proves: an issuer's `pathLenConstraint` bounds the CAs under it: zero allows the device
/// directly beneath it and no CA between, and one allows one.
///
/// Catches: a CA with a path length limit that signs further CAs.
#[test]
fn a_path_length_limit_of_an_issuer_is_kept() {
    let pki = pki();
    let (top_key, mid_key) = (rsa(3), rsa(0));
    let make_leaf = |issuer: &str, key: &Key| {
        cert_of(
            Kind::Device,
            "CN=Fake Device",
            issuer,
            &pki.device_key,
            key,
            valid(),
        )
    };
    let chain_with = |limit: Option<u8>| {
        let top = cert_of(
            Kind::Ca(limit),
            "CN=Top CA",
            "CN=Fake Apple Root",
            &top_key,
            &pki.root_key,
            valid(),
        );
        let mid = ca_cert("CN=Mid CA", "CN=Top CA", &mid_key, &top_key, valid());
        let leaf = make_leaf("CN=Mid CA", &mid_key);
        under(&pki.device_key, &leaf, &[&mid, &top])
    };
    assert_eq!(check(&chain_with(Some(1))), Ok(device()));
    assert_eq!(check(&chain_with(None)), Ok(device()));
    assert_eq!(
        check(&chain_with(Some(0))),
        Err(Error::Untrusted("an issuer's path length is exceeded"))
    );
}

/// Proves: a chain whose CAs are P-384 keys signing with SHA-384 (the shape of Apple Root CA - G3)
/// verifies, for a P-384 device and for a P-256 device under a P-384 CA.
///
/// Catches: a verifier that cannot walk a chain through the G3 hierarchy and holds every Mac
/// under it for an operator, with a reason that says no issuer.
#[test]
fn a_chain_through_a_p384_root_is_verified() {
    let (root_key, ca_key, device_key) = (ec384(5), ec384(6), ec384(7));
    let root = ca_cert(
        "CN=P384 Root",
        "CN=P384 Root",
        &root_key,
        &root_key,
        valid(),
    );
    let ca = ca_cert("CN=P384 CA", "CN=P384 Root", &ca_key, &root_key, valid());
    let leaf = cert_of(
        Kind::Device,
        "CN=P384 Device",
        "CN=P384 CA",
        &device_key,
        &ca_key,
        valid(),
    );
    let spec = Spec {
        signature_alg: ECDSA_SHA384,
        digest_alg: SHA384,
        ..under(&device_key, &leaf, &[&ca])
    };
    assert_eq!(verify(&blob(&spec), &[der(&root)], now()), Ok(device()));
    let p256_leaf = cert_of(
        Kind::Device,
        "CN=P256 Device",
        "CN=P384 CA",
        &ec(),
        &ca_key,
        valid(),
    );
    let spec = Spec {
        signature_alg: ECDSA_SHA256,
        ..under(&ec(), &p256_leaf, &[&ca])
    };
    assert_eq!(verify(&blob(&spec), &[der(&root)], now()), Ok(device()));
}

/// Proves: a P-384 signature under SHA-256, a P-384 signature that is not DER and one that is
/// wrong are refused for what they are, and an issuer's algorithm that is not implemented is
/// named, not reported as a missing issuer.
///
/// Catches: a P-384 key used with the wrong hash, or a refusal that misleads the operator.
#[test]
fn p384_signatures_of_another_hash_or_shape_are_refused() {
    let (root_key, device_key) = (ec384(5), ec384(7));
    let root = ca_cert(
        "CN=P384 Root",
        "CN=P384 Root",
        &root_key,
        &root_key,
        valid(),
    );
    let leaf = cert_of(
        Kind::Device,
        "CN=P384 Device",
        "CN=P384 Root",
        &device_key,
        &root_key,
        valid(),
    );
    let base = Spec {
        signature_alg: ECDSA_SHA384,
        digest_alg: SHA384,
        ..under(&device_key, &leaf, &[])
    };
    let roots = [der(&root)];
    assert_eq!(verify(&blob(&base), &roots, now()), Ok(device()));
    let sha256 = Spec {
        signature_alg: EC_KEY,
        digest_alg: SHA256,
        ..base.clone()
    };
    assert_eq!(
        verify(&blob(&sha256), &roots, now()),
        Err(Error::Unsupported("ECDSA digest other than SHA-384"))
    );
    let garbled = Spec {
        signature_der: Some(vec![1, 2, 3]),
        ..base.clone()
    };
    assert_eq!(
        verify(&blob(&garbled), &roots, now()),
        Err(Error::BadSignature("a signature does not verify"))
    );
    let broken = Spec {
        break_signature: true,
        ..base
    };
    assert_eq!(
        verify(&blob(&broken), &roots, now()),
        Err(Error::BadSignature("a signature does not verify"))
    );
}
