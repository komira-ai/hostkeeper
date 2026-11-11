//! Who is calling (`docs/design/coordinator-mdm.md`, sections 4.1 and 4.2).

use hyper::HeaderMap;

use super::{Refused, SIGNATURE_HEADER, certificate, enrollment, fingerprint, identity};
use crate::mdm_server::support::{BODY, Rig, SERIAL, Spec, UDID, header};
use crate::store::mdm::MdmStore;

/// Proves: a certificate is told by the SHA-256 of its DER in lower-case hex.
///
/// Catches: a hash spelled differently from the tables', which would match no identity.
#[test]
fn a_certificate_is_told_by_the_sha256_of_its_der() {
    assert_eq!(
        fingerprint(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Proves: the certificate of the connection is used when there is one, whatever the header says; a
/// header is used when there is none; and neither is refused.
///
/// Catches: a signature naming one identity overriding the TLS certificate of another.
#[test]
fn the_connections_certificate_wins_over_the_header() {
    let rig = Rig::new("auth-priority");
    let mac = rig.mac_ca.issue(1, SERIAL);
    let mut headers = HeaderMap::new();
    headers.insert(
        SIGNATURE_HEADER,
        header(BODY, &mac, &Spec::default())
            .parse()
            .expect("a value"),
    );
    let leaf: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"the tls leaf"[..]);
    assert_eq!(
        certificate(&Some(leaf), &headers, BODY),
        Ok(fingerprint(b"the tls leaf"))
    );
    assert_eq!(certificate(&None, &headers, BODY), Ok(mac.sha256()));
    assert_eq!(
        certificate(&None, &HeaderMap::new(), BODY),
        Err(Refused::Unauthenticated)
    );
    let mut bad = HeaderMap::new();
    bad.insert(SIGNATURE_HEADER, "!!".parse().expect("a value"));
    assert_eq!(
        certificate(&None, &bad, BODY),
        Err(Refused::Unauthenticated)
    );
}

/// Proves: an identity is accepted only while it is issued, and its enrollment only while it is the
/// live one entered with the same certificate.
///
/// Catches: a retired identity, or the certificate of a replaced enrollment, taken as the live
/// Mac's.
#[test]
fn an_identity_is_accepted_while_issued_and_its_enrollment_while_live() {
    let rig = Rig::new("auth-states");
    let mac = rig.mac_ca.issue(1, SERIAL);
    assert_eq!(
        identity(&*rig.store, &mac.sha256()),
        Err(Refused::UnknownIdentity)
    );
    rig.issue(&mac, SERIAL, UDID, false);
    let issued = identity(&*rig.store, &mac.sha256()).expect("issued");
    assert_eq!(
        enrollment(&*rig.store, &issued, &mac.sha256()),
        Err(Refused::NotLive)
    );
    let mut entered = rig.live("e1", SERIAL, true);
    assert_eq!(
        enrollment(&*rig.store, &issued, &mac.sha256()),
        Err(Refused::NotLive),
        "another certificate"
    );
    entered.cert_sha256 = Some(mac.sha256());
    rig.store.replace_live(&entered).expect("entered");
    assert_eq!(
        enrollment(&*rig.store, &issued, &mac.sha256())
            .expect("live")
            .enrollment_id,
        "e1"
    );
    rig.store.kill_identity(&mac.sha256()).expect("killed");
    assert_eq!(
        identity(&*rig.store, &mac.sha256()),
        Err(Refused::UnknownIdentity)
    );
}
