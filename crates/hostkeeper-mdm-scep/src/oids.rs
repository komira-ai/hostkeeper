//! The object identifiers of SCEP (draft-nourse-scep, RFC 8894), CMS and PKCS#9 this crate reads.
use const_oid::ObjectIdentifier;

pub(crate) const MESSAGE_TYPE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.113733.1.9.2");
pub(crate) const PKI_STATUS: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.113733.1.9.3");
pub(crate) const FAIL_INFO: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.113733.1.9.4");
pub(crate) const SENDER_NONCE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.113733.1.9.5");
pub(crate) const RECIPIENT_NONCE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.113733.1.9.6");
pub(crate) const TRANSACTION_ID: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.113733.1.9.7");
pub(crate) const CHALLENGE_PASSWORD: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.7");
pub(crate) const CONTENT_TYPE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.3");
pub(crate) const MESSAGE_DIGEST: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");

pub(crate) const DES_EDE3_CBC: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.3.7");
pub(crate) const AES128_CBC: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.1.2");
pub(crate) const AES192_CBC: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.1.22");
pub(crate) const AES256_CBC: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.1.42");

pub(crate) const RSA_ENCRYPTION: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
pub(crate) const SHA1_WITH_RSA: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.5");
pub(crate) const SHA256_WITH_RSA: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
pub(crate) const SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
pub(crate) const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
