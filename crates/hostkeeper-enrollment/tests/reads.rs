//! What the coordinator's heartbeat and renewal paths read of the CA and the store: the host a
//! presented certificate names, only when this CA signed it as a host's client certificate; and
//! the role an approved host was given.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::ca::{CaBundle, HostCertificate, ServerNames, Signer};
use hostkeeper_enrollment::enroll::{EnrollmentStore, StoreError};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::Role;
use support::{HostKey, TestCa, decision, host_id, request, scratch};

/// Catches a certificate read as a host's when another CA signed it, when it is the
/// coordinator's server certificate, when it is not a certificate, or when the bundle is not a
/// CA certificate; and a host certificate of this CA read with the wrong host or key.
#[test]
fn a_host_certificate_is_read_back_only_when_this_ca_signed_it() {
    let ca = TestCa::new("reads-ca");
    let other = TestCa::new("reads-other-ca");
    let key = HostKey::new();
    let host = ca
        .ca
        .issue_host(&host_id("web-1"), &key.p256(), 7)
        .expect("a host certificate");
    let bundle = ca.ca.bundle();
    assert_eq!(
        bundle.host_certificate(&host.der),
        Some(HostCertificate {
            host_id: host_id("web-1"),
            key: key.p256(),
        })
    );

    let foreign = other
        .ca
        .issue_host(&host_id("web-1"), &key.p256(), 7)
        .expect("another CA's host certificate");
    assert_eq!(bundle.host_certificate(&foreign.der), None);

    let server = ca
        .ca
        .issue_server(
            &key.p256(),
            ServerNames {
                dns: &["coordinator.example".to_owned()],
                ips: &[],
            },
            3600,
            8,
        )
        .expect("a server certificate");
    assert_eq!(bundle.host_certificate(&server.der), None);
    assert_eq!(bundle.host_certificate(b"not a certificate"), None);

    let not_a_ca = CaBundle::of(b"not a certificate".to_vec());
    assert_eq!(not_a_ca.host_certificate(&host.der), None);
}

/// Catches the role of an approved host not read back, a host id never approved read as one,
/// and a host file whose role is outside the grammar read as valid.
#[test]
fn an_approved_host_has_its_role_and_a_broken_role_is_refused() {
    let dir = scratch("reads-role");
    let store = EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open");
    let key = HostKey::new();
    let posted = store.post(request(&key, "machine-1"), 1).expect("post");
    store
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect("approve");
    assert_eq!(
        store.host_role(&host_id("web-1")).expect("read"),
        Some(Role::parse("web").expect("a role"))
    );
    assert_eq!(store.host_role(&host_id("web-2")).expect("read"), None);

    // A later `role` (field 5) that is not a role: the last one read wins.
    let path = dir.path().join("hosts/web-1.pb");
    let mut bytes = std::fs::read(&path).expect("the host file");
    bytes.extend_from_slice(&[0x2a, 0x08]);
    bytes.extend_from_slice(b"Bad_Role");
    std::fs::write(&path, bytes).expect("write");
    let refused = store.host_role(&host_id("web-1")).expect_err("a bad role");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
}
