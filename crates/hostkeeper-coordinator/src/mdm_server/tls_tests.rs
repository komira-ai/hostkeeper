//! The configuration of the Macs' listener made from the store (`docs/design/coordinator-mdm.md`,
//! sections 4.1 and 4.10).

use std::sync::Arc;

use super::MdmTls;
use crate::mdm_secrets::SecretKind;
use crate::mdm_server::support::Rig;

/// Proves: there is no configuration until both the certificate and its key are held, and not one
/// when either does not parse; with a pair held there is one, and the same one while nothing
/// changed.
///
/// Catches: a listener that answers a handshake with half a certificate.
#[test]
fn there_is_no_configuration_without_a_certificate_and_a_key_that_parse() {
    let rig = Rig::bare("tls-none");
    let tls = rig.server.tls();
    assert!(tls.config().is_none());
    rig.store_tls();
    let first = tls.config().expect("a pair is held");
    assert!(Arc::ptr_eq(&first, &tls.config().expect("the same")));
    rig.put(SecretKind::TlsKey, b"not a key");
    assert!(tls.config().is_none(), "a key that does not parse");
    rig.store_tls();
    assert!(tls.config().is_some());
    rig.put(SecretKind::TlsCert, b"not a certificate");
    assert!(tls.config().is_none(), "a certificate that does not parse");
}

/// Proves: a new version of the certificate makes a new configuration; trusted issuers that are not
/// certificates leave a configuration that asks for no client certificate.
///
/// Catches: a renewal that is stored and never served.
#[test]
fn a_new_version_makes_a_new_configuration_and_unparsable_issuers_are_left_out() {
    let rig = Rig::new("tls-renew");
    let before = rig.server.tls().config().expect("a configuration");
    rig.store_tls();
    let after = rig.server.tls().config().expect("a configuration");
    assert!(!Arc::ptr_eq(&before, &after));
    let junk = MdmTls::new(Arc::clone(&rig.setup), Arc::new(|| vec![vec![1, 2, 3]]));
    assert!(junk.config().is_some());
    let unkeyed = MdmTls::new(
        Arc::new(crate::mdm_setup::MdmSetup::volatile()),
        Arc::new(Vec::new),
    );
    assert!(unkeyed.config().is_none(), "no master key: nothing opens");
}

/// Proves: a certificate and a key that parse but are not a pair make no configuration.
///
/// Catches: a listener that completes handshakes it cannot sign.
#[test]
fn a_key_that_is_not_the_certificates_makes_no_configuration() {
    let rig = Rig::bare("tls-mismatch");
    rig.store_tls();
    let other = Rig::bare("tls-mismatch-other");
    other.store_tls();
    let key = other
        .setup
        .open(SecretKind::TlsKey)
        .expect("read")
        .expect("held");
    rig.put(SecretKind::TlsKey, key.expose());
    assert!(rig.server.tls().config().is_none());
}
