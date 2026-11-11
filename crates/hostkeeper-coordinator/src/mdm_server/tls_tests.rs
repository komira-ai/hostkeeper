//! The configuration of the Macs' listener made from the store (`docs/design/coordinator-mdm.md`,
//! sections 4.1 and 4.10).

use std::sync::Arc;

use super::MdmTls;
use crate::mdm_secrets::SecretKind;
use crate::mdm_server::support::Rig;

/// Proves: there is no configuration until both the certificate and its key are held, and not one
/// when the only ones held do not parse; with a pair held there is one, and the same one while
/// nothing changed.
///
/// Catches: a listener that answers a handshake with half a certificate.
#[test]
fn there_is_no_configuration_without_a_certificate_and_a_key_that_parse() {
    let rig = Rig::bare("tls-none");
    let tls = rig.server.tls();
    assert!(tls.config().is_none());
    rig.put(SecretKind::TlsKey, b"not a key");
    assert!(tls.config().is_none(), "a key and no certificate");
    rig.put(SecretKind::TlsCert, b"not a certificate");
    assert!(tls.config().is_none(), "neither parses");
    rig.store_tls();
    let first = tls.config().expect("a pair is held");
    assert!(Arc::ptr_eq(&first, &tls.config().expect("the same")));
}

/// Proves: a key or a certificate that does not parse, stored over a pair in use, leaves that pair
/// serving.
///
/// Catches: a refused or garbled upload that stops the listener.
#[test]
fn a_newest_key_or_certificate_that_does_not_parse_leaves_the_pair_in_use_serving() {
    let rig = Rig::bare("tls-garbage");
    rig.store_tls();
    rig.put(SecretKind::TlsKey, b"not a key");
    assert!(
        rig.server.tls().config().is_some(),
        "a key that does not parse"
    );
    rig.put(SecretKind::TlsCert, b"not a certificate");
    assert!(
        rig.server.tls().config().is_some(),
        "a certificate that does not parse"
    );
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

/// Reads the held `kind` of `rig`.
fn held(rig: &Rig, kind: SecretKind) -> Vec<u8> {
    rig.setup
        .open(kind)
        .expect("read")
        .expect("held")
        .expose()
        .to_vec()
}

/// Proves: a key stored before its certificate (a rotation is two uploads) leaves the pair in use
/// serving, and the certificate for that key then takes over; a certificate and a key that are not
/// a pair and have no earlier pair behind them make no configuration.
///
/// Catches: a certificate rotation that stops every handshake between its two uploads.
#[test]
fn a_key_that_is_not_the_certificates_leaves_the_pair_in_use_serving() {
    let rig = Rig::bare("tls-mismatch");
    rig.store_tls();
    let next = Rig::bare("tls-mismatch-next");
    next.store_tls();
    let (next_cert, next_key) = (
        held(&next, SecretKind::TlsCert),
        held(&next, SecretKind::TlsKey),
    );
    let old_key = held(&rig, SecretKind::TlsKey);
    let tls = rig.server.tls();
    let serving = tls.config().expect("the pair in use");
    rig.put(SecretKind::TlsKey, &next_key);
    let between = tls.config().expect("the pair in use still serves");
    assert!(
        !Arc::ptr_eq(&serving, &between),
        "made again from the same pair"
    );
    rig.put(SecretKind::TlsCert, &next_cert);
    assert!(tls.config().is_some(), "the new key has its certificate");

    let bare = Rig::bare("tls-mismatch-none");
    bare.put(SecretKind::TlsCert, &next_cert);
    bare.put(SecretKind::TlsKey, &old_key);
    assert!(
        bare.server.tls().config().is_none(),
        "a pair that does not fit and nothing behind it"
    );
}
