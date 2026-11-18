//! `check_key_request`: the check of a service certificate's CSR, which is the host CSR's check
//! with no rule about the subject: only the key is read, and the CA names the subject.

mod support;

use hostkeeper_enrollment::ca::{
    CsrDefect, MAX_CSR_BYTES, ProfileError, check_key_request, check_request,
};
use rcgen::{DnType, KeyPair};
use support::{HostKey, csr_of, subject};

/// A request whose subject is no `host:<id>` (a service's, an empty one, two names) is read for its
/// key all the same, and `check_request` still refuses it for its subject.
#[test]
fn a_request_is_read_for_its_key_whatever_its_subject() {
    let host = HostKey::new();
    let mut two_names = subject("service:web");
    two_names
        .distinguished_name
        .push(DnType::OrganizationName, "fleet");
    for csr in [
        host.csr("service:web"),
        host.csr("host:pending"),
        csr_of(&host.pair, two_names),
        csr_of(&host.pair, rcgen::CertificateParams::default()),
    ] {
        assert_eq!(check_key_request(&csr), Ok(host.p256()));
    }
    assert_eq!(
        check_request(&host.csr("service:web")),
        Err(ProfileError::BadCsr(CsrDefect::Subject))
    );
}

/// A request with a key that is not P-256, a self-signature that does not verify, too many bytes or
/// no DER is refused as `check_request` refuses it.
#[test]
fn a_request_that_breaks_the_profile_otherwise_is_refused_for_its_key() {
    let host = HostKey::new();
    let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).expect("P-384");
    let mut tampered = host.csr("service:web");
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let cases = [
        (
            csr_of(&p384, subject("service:web")),
            ProfileError::KeyNotP256,
        ),
        (tampered, ProfileError::BadCsr(CsrDefect::Signature)),
        (
            vec![0u8; MAX_CSR_BYTES + 1],
            ProfileError::BadCsr(CsrDefect::TooLarge),
        ),
        (
            b"not a request".to_vec(),
            ProfileError::BadCsr(CsrDefect::NotDer),
        ),
    ];
    for (csr, refused) in cases {
        assert_eq!(check_key_request(&csr), Err(refused));
    }
}
