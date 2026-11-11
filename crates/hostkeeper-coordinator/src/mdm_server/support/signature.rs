//! The CMS messages of a Mac that signs its check-ins, for the tests of the signature check: the
//! digest and signature algorithms, the knobs (`Spec`) that bend a message into each way it can
//! be wrong, and the `Mdm-Signature` header built over one.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_mdm_push::testkit::{der, fixture_key, oid_der};
use ring::digest;
use rsa_legacy::Pkcs1v15Sign;

use super::Identity2;

// The CMS messages of a client that signs its messages.
pub(in crate::mdm_server) const SHA1: &str = "1.3.14.3.2.26";
pub(in crate::mdm_server) const SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub(in crate::mdm_server) const SHA384: &str = "2.16.840.1.101.3.4.2.2";
pub(in crate::mdm_server) const SHA512: &str = "2.16.840.1.101.3.4.2.3";
pub(in crate::mdm_server) const RSA: &str = "1.2.840.113549.1.1.1";
pub(in crate::mdm_server) const SHA256_RSA: &str = "1.2.840.113549.1.1.11";
pub(in crate::mdm_server) const BODY: &[u8] = b"<plist>a check-in</plist>";

/// The DER prefix of a `DigestInfo` for the digest `oid`, and the digest algorithm.
pub(in crate::mdm_server) fn digest_info(oid: &str) -> (&'static [u8], &'static digest::Algorithm) {
    match oid {
        SHA1 => (
            &[
                0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04,
                0x14,
            ],
            &digest::SHA1_FOR_LEGACY_USE_ONLY,
        ),
        SHA256 => (
            &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            &digest::SHA256,
        ),
        SHA384 => (
            &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            &digest::SHA384,
        ),
        _ => (
            &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
            &digest::SHA512,
        ),
    }
}

/// How a message is built.
#[derive(Clone)]
pub(in crate::mdm_server) struct Spec {
    pub(in crate::mdm_server) digest: &'static str,
    pub(in crate::mdm_server) signature: &'static str,
    pub(in crate::mdm_server) attributes: bool,
    pub(in crate::mdm_server) ski: Option<Vec<u8>>,
    /// The digest in the attributes is of this instead of the body, when set.
    pub(in crate::mdm_server) claimed: Option<Vec<u8>>,
    pub(in crate::mdm_server) leave_out_digest_attribute: bool,
    pub(in crate::mdm_server) certificates: usize,
    pub(in crate::mdm_server) names_other_serial: bool,
    pub(in crate::mdm_server) indefinite_attributes: bool,
    pub(in crate::mdm_server) signers: usize,
    pub(in crate::mdm_server) sid_tag: Option<u8>,
    pub(in crate::mdm_server) flip_signature: bool,
    pub(in crate::mdm_server) drop_certificates: bool,
    /// Keeps only this many of the six fields of the signer's info, when set.
    pub(in crate::mdm_server) info_fields: Option<usize>,
    /// Names the signer by its issuer alone, without a serial number.
    pub(in crate::mdm_server) short_sid: bool,
    /// Only the first this many fields of the signer info are sent, when set.
    pub(in crate::mdm_server) keep_fields: Option<usize>,
    /// The signer is named by a sequence of one part, not issuer and serial.
    pub(in crate::mdm_server) lone_sid: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            digest: SHA256,
            signature: RSA,
            attributes: true,
            ski: None,
            claimed: None,
            leave_out_digest_attribute: false,
            certificates: 1,
            names_other_serial: false,
            indefinite_attributes: false,
            signers: 1,
            sid_tag: None,
            flip_signature: false,
            drop_certificates: false,
            info_fields: None,
            short_sid: false,
            keep_fields: None,
            lone_sid: false,
        }
    }
}

fn null() -> Vec<u8> {
    vec![0x05, 0x00]
}

fn algorithm(oid: &str) -> Vec<u8> {
    der(0x30, &[oid_der(oid), null()].concat())
}

/// The cms message for `body` signed by `mac`, built as `spec` says.
pub(in crate::mdm_server) fn message(body: &[u8], mac: &Identity2, spec: &Spec) -> Vec<u8> {
    let key = fixture_key(mac.index);
    let (prefix, algorithm_of) = digest_info(spec.digest);
    let body_digest = digest::digest(algorithm_of, body).as_ref().to_vec();
    let claimed = spec.claimed.clone().unwrap_or_else(|| body_digest.clone());
    let attribute =
        |oid: &str, value: Vec<u8>| der(0x30, &[oid_der(oid), der(0x31, &value)].concat());
    let mut attributes = vec![attribute(
        "1.2.840.113549.1.9.3",
        oid_der("1.2.840.113549.1.7.1"),
    )];
    if !spec.leave_out_digest_attribute {
        attributes.push(attribute("1.2.840.113549.1.9.4", der(0x04, &claimed)));
    }
    let attributes = attributes.concat();
    let signed_over = if spec.attributes {
        der(0x31, &attributes)
    } else {
        body.to_vec()
    };
    let hashed = digest::digest(algorithm_of, &signed_over);
    let mut to_sign = prefix.to_vec();
    to_sign.extend_from_slice(hashed.as_ref());
    let mut signature = key
        .sign(Pkcs1v15Sign::new_unprefixed(), &to_sign)
        .expect("signs");
    if spec.flip_signature {
        signature[0] ^= 0xff;
    }
    let (_, parsed) = x509_parser::parse_x509_certificate(&mac.der).expect("a certificate");
    let sid = match (&spec.ski, spec.sid_tag) {
        _ if spec.lone_sid => der(0x30, &der(0x02, &[1])),
        (_, Some(tag)) => der(tag, b"x"),
        (Some(ski), None) => der(0x80, ski),
        (None, None) => {
            let issuer = parsed.tbs_certificate.issuer.as_raw().to_vec();
            let serial = if spec.names_other_serial {
                vec![0x7f, 1, 2, 3]
            } else {
                parsed.raw_serial().to_vec()
            };
            if spec.short_sid {
                der(0x30, &issuer)
            } else {
                der(0x30, &[issuer, der(0x02, &serial)].concat())
            }
        }
    };
    let attributes_field = if !spec.attributes {
        Vec::new()
    } else if spec.indefinite_attributes {
        hostkeeper_mdm_push::testkit::indefinite(0xa0, &attributes)
    } else {
        der(0xa0, &attributes)
    };
    let mut fields = vec![
        der(0x02, &[1]),
        sid,
        algorithm(spec.digest),
        attributes_field,
        algorithm(spec.signature),
        der(0x04, &signature),
    ];
    if let Some(keep) = spec.info_fields {
        fields.truncate(keep);
    }
    let info = der(
        0x30,
        &fields[..spec.keep_fields.unwrap_or(fields.len())].concat(),
    );
    let infos = vec![info; spec.signers].concat();
    let certificates = if spec.drop_certificates {
        Vec::new()
    } else {
        der(0xa0, &vec![mac.der.clone(); spec.certificates].concat())
    };
    let signed = der(
        0x30,
        &[
            der(0x02, &[1]),
            der(0x31, &algorithm(spec.digest)),
            der(0x30, &oid_der("1.2.840.113549.1.7.1")),
            certificates,
            der(0x31, &infos),
        ]
        .concat(),
    );
    der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.2"), der(0xa0, &signed)].concat(),
    )
}

pub(in crate::mdm_server) fn header(body: &[u8], mac: &Identity2, spec: &Spec) -> String {
    STANDARD.encode(message(body, mac, spec))
}
