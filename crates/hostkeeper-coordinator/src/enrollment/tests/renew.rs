//! `Renew`: a new certificate for the host a current certificate of this CA names and the key it
//! certifies, and every way a renewal is refused.

use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::agent::v1::{CoordinatorRefusal, RenewRequest, RenewResponse};
use http_body_util::Full;
use prost::Message;
use x509_parser::prelude::{FromDer, X509Certificate};

use super::support::{HostKey, NOW, PATIENCE, Rig, approve, body_of, enroll_request, enrolled};
use crate::enrollment::{Method, RpcError, call};

fn renewal(key: &HostKey) -> RenewRequest {
    RenewRequest { csr_der: key.csr() }
}

/// The subject, serial and validity of the certificate `der`.
fn read(der: &[u8]) -> (String, Vec<u8>, i64, i64) {
    let (_, certificate) = X509Certificate::from_der(der).expect("a certificate");
    let validity = certificate.validity();
    (
        certificate.subject().to_string(),
        certificate.raw_serial().to_vec(),
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
    )
}

/// Catches a renewal that names another host, keeps the old serial, is not 7 days from the
/// renewal, is not for the key the host holds, or is signed for a P-384 key or a CSR that is
/// no CSR; and a renewal without a certificate of this CA answered.
#[tokio::test]
async fn renew_returns_a_new_certificate_for_the_same_host_id() {
    let rig = Rig::new("renew");
    let enrollment = &rig.enrollment;
    let key = HostKey::new();
    let current = enrolled(enrollment, &key, "machine-1", "s1");
    let later = NOW + 3_600;
    rig.clock.set(later);

    let renewed = enrollment
        .renew(Some(current.as_slice()), renewal(&key))
        .expect("renewed");
    let (subject, serial, start, end) = read(&renewed.certificate_der);
    assert_eq!(subject, "CN=host:s1");
    assert_ne!(serial, read(&current).1);
    let later = i64::try_from(later).expect("a time");
    assert_eq!((start, end), (later, later + 7 * 24 * 3_600));
    let (_, ca) =
        x509_parser::pem::parse_x509_pem(renewed.ca_bundle_pem.as_bytes()).expect("a PEM bundle");
    assert_eq!(ca.contents, enrollment.ca_bundle().certificate_der());

    let peer = Some(Arc::from(current.as_slice()));
    let answer = call(
        enrollment,
        Method::Renew,
        &peer,
        body_of(&renewal(&key)),
        PATIENCE,
    )
    .await
    .expect("renewed by a call");
    let decoded = RenewResponse::decode(answer.as_slice()).expect("a RenewResponse");
    assert_eq!(read(&decoded.certificate_der).0, "CN=host:s1");

    let refusals = [
        (
            renewal(&HostKey::of(&rcgen::PKCS_ECDSA_P384_SHA384)),
            CoordinatorRefusal::KeyNotP256,
        ),
        (
            RenewRequest {
                csr_der: b"not a CSR".to_vec(),
            },
            CoordinatorRefusal::BadCsr,
        ),
        (
            renewal(&HostKey::new()),
            CoordinatorRefusal::RenewalNotAllowed,
        ),
    ];
    for (request, refusal) in refusals {
        assert_eq!(
            enrollment.renew(Some(current.as_slice()), request),
            Err(RpcError::Refused(refusal))
        );
    }

    let stranger = Rig::new("renew-stranger");
    let foreign = enrolled(&stranger.enrollment, &key, "machine-1", "s1");
    for peer in [
        None,
        Some(foreign.as_slice()),
        Some(b"not a certificate".as_slice()),
    ] {
        assert_eq!(
            enrollment.renew(peer, renewal(&key)),
            Err(RpcError::Refused(CoordinatorRefusal::Forbidden))
        );
    }
    let garbage = Full::new(Bytes::from_static(b"\xff\xff"));
    assert_eq!(
        call(enrollment, Method::Renew, &peer, garbage, PATIENCE).await,
        Err(RpcError::Malformed)
    );
}

/// Catches a key whose host id an approval with `replace` gave to another key still renewing,
/// and a host two live daemons speak for renewing at all.
#[test]
fn a_replaced_key_and_a_duplicate_host_cannot_renew() {
    let rig = Rig::new("renew-replaced");
    let enrollment = &rig.enrollment;
    let (first, second) = (HostKey::new(), HostKey::new());
    let old = enrolled(enrollment, &first, "machine-1", "s1");
    enrollment
        .enroll(enroll_request(&second, "machine-2"))
        .expect("enrolled");
    approve(enrollment, &second, "s1", true);
    assert_eq!(
        enrollment.renew(Some(old.as_slice()), renewal(&first)),
        Err(RpcError::Refused(CoordinatorRefusal::RenewalNotAllowed))
    );

    let other = HostKey::new();
    let current = enrolled(enrollment, &other, "machine-3", "s3");
    let host: hostkeeper_wire::HostId = "s3".parse().expect("a host id");
    enrollment.report_live(&host, "machine-3");
    enrollment.report_live(&host, "machine-4");
    assert_eq!(
        enrollment.renew(Some(current.as_slice()), renewal(&other)),
        Err(RpcError::Refused(CoordinatorRefusal::RenewalNotAllowed))
    );
}
