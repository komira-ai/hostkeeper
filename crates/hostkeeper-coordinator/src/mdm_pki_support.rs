//! Certificates and keys for the tests of the MDM status: self-signed, made with rcgen.

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use time::OffsetDateTime;

/// The subject attribute an APNs certificate names its topic in: `UID`.
const UID: [u64; 7] = [0, 9, 2342, 19_200_300, 100, 1, 1];

/// The push topic of the test push certificates.
pub(crate) const TOPIC: &str = "com.apple.mgmt.External.0a1b2c3d-4e5f-6789-abcd-0123456789ab";

/// A certificate and its key, as PEM text.
#[derive(Clone)]
pub(crate) struct Pair {
    pub(crate) cert: String,
    pub(crate) key: String,
}

/// A self-signed certificate with subject UID `uid` (none when `None`) and the DNS names
/// `names`, ending at the unix time `not_after`, and its key.
pub(crate) fn pair(uid: Option<&str>, names: &[&str], not_after: i64) -> Pair {
    let key = KeyPair::generate().expect("a key");
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let mut params = CertificateParams::new(names).expect("a certificate request");
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, "test");
    if let Some(uid) = uid {
        params
            .distinguished_name
            .push(DnType::CustomDnType(UID.to_vec()), uid);
    }
    params.not_before = OffsetDateTime::from_unix_timestamp(1_000_000_000).expect("a time");
    params.not_after = OffsetDateTime::from_unix_timestamp(not_after).expect("a time");
    let cert = params.self_signed(&key).expect("a certificate");
    Pair {
        cert: cert.pem(),
        key: key.serialize_pem(),
    }
}

/// A key that is no certificate's here.
pub(crate) fn other_key() -> String {
    KeyPair::generate().expect("a key").serialize_pem()
}
