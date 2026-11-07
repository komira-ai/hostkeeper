//! The MDM commands of the test and the root's profile, as XML property lists.
//!
//! - `InstallProfile` ([`install_profile`]) carries the root's profile as its `Payload`.
//! - `InstallEnterpriseApplication` ([`install_application`]) carries an inline `Manifest`, as
//!   design section 7.1 has the gate send it: one `software-package` asset whose `url` is
//!   `https://<MDM host>/pkg/<sha256>.pkg` and whose `sha256` is the pkg's whole-file digest, which
//!   the Mac checks, and the `metadata` `bundle-identifier`, `bundle-version`, `kind` `software`
//!   and `title`.
//! - `RemoveProfile` ([`remove_profile`]) names the root's profile by its identifier.
//!
//! The root's profile ([`root_profile`]) is one `Configuration` profile, scoped to the system,
//! with one `com.apple.security.root` payload: the root certificate, DER.
//!
//! A command's UUID ([`command_uuid`]) is derived from the kit's root profile digest, the step and
//! the number of earlier sends of that step, so every send of a kit has its own UUID and no
//! random source is read. The payload UUIDs of the profile are derived from the root's digest.

use std::str::FromStr;

use plist::{Dictionary, Value};
use sha2::{Digest, Sha256};

use super::{Kit, KitPkg, PkgSpec, Step};

/// The farm's MDM profile identifier: a reverse-DNS name of at least two labels, each of ASCII
/// letters, digits and `-`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileIdentifier(String);

impl FromStr for ProfileIdentifier {
    type Err = String;

    fn from_str(s: &str) -> Result<ProfileIdentifier, String> {
        let labels: Vec<&str> = s.split('.').collect();
        let good = labels.len() >= 2
            && labels.iter().all(|l| {
                !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if good {
            Ok(ProfileIdentifier(s.to_owned()))
        } else {
            Err(format!(
                "{s:?} is not a reverse-DNS identifier (labels of letters, digits and -, \
                 joined by dots)"
            ))
        }
    }
}

impl ProfileIdentifier {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The root profile's identifier: `<it>.signing-test.root`.
    pub(crate) fn root_profile(&self) -> String {
        format!("{}.signing-test.root", self.0)
    }
}

/// The MDM host's DNS name, which its proxy's certificate carries: labels of ASCII letters,
/// digits and `-`, joined by dots, with no port, path or scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MdmHost(String);

impl FromStr for MdmHost {
    type Err = String;

    fn from_str(s: &str) -> Result<MdmHost, String> {
        let labels: Vec<&str> = s.split('.').collect();
        let good = s.len() <= 253
            && labels.len() >= 2
            && labels.iter().all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && !l.starts_with('-')
                    && !l.ends_with('-')
                    && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if good {
            Ok(MdmHost(s.to_ascii_lowercase()))
        } else {
            Err(format!(
                "{s:?} is not a DNS name (the MDM host's name alone, with no scheme, port or \
                 path)"
            ))
        }
    }
}

impl MdmHost {
    /// The URL the proxy serves the pkg of digest `sha256` at (design section 7.1).
    pub(crate) fn pkg_url(&self, sha256: &str) -> String {
        format!("https://{}/pkg/{sha256}.pkg", self.0)
    }
}

/// The UUID-shaped text of the first 16 bytes of `digest`, upper case.
fn uuid_of(digest: &[u8]) -> String {
    let h = hex::encode_upper(&digest[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The UUID of the send of `step` that follows `earlier` sends of it, for `kit`.
pub(crate) fn command_uuid(kit: &Kit, step: Step, earlier: usize) -> String {
    let mut h = Sha256::new();
    h.update(b"hostkeeper-mdm-signing-test\0");
    h.update(kit.root_profile.sha256.as_bytes());
    h.update(b"\0");
    h.update(step.name().as_bytes());
    h.update(b"\0");
    h.update(earlier.to_string().as_bytes());
    uuid_of(&h.finalize())
}

/// The XML text of `value`.
fn xml(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    // Writing to a `Vec` fails only for a value XML cannot hold (a date out of range, a real
    // that is not finite), which these builders never make; were it to fail, the text would end
    // early and NanoHUB or the Mac would refuse it as a malformed command, never act on it.
    let _ = value.to_writer_xml(&mut out);
    out
}

/// A dictionary of `pairs`, in order.
fn dict<const N: usize>(pairs: [(&str, Value); N]) -> Dictionary {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// The command plist `{Command: <command>, CommandUUID: <uuid>}`, with `RequestType` first.
fn command(request_type: &str, fields: Dictionary, uuid: &str) -> Vec<u8> {
    let mut inner = dict([("RequestType", Value::from(request_type))]);
    inner.extend(fields);
    xml(&Value::Dictionary(dict([
        ("Command", Value::Dictionary(inner)),
        ("CommandUUID", Value::from(uuid)),
    ])))
}

/// `InstallProfile` of the profile `profile`.
pub(crate) fn install_profile(profile: &[u8], uuid: &str) -> Vec<u8> {
    command(
        "InstallProfile",
        dict([("Payload", Value::Data(profile.to_vec()))]),
        uuid,
    )
}

/// `RemoveProfile` of the profile `identifier`.
pub(crate) fn remove_profile(identifier: &str, uuid: &str) -> Vec<u8> {
    command(
        "RemoveProfile",
        dict([("Identifier", Value::from(identifier))]),
        uuid,
    )
}

/// `InstallEnterpriseApplication` of the kit's pkg `pkg` (of `spec`), served by `host`.
pub(crate) fn install_application(
    spec: &PkgSpec,
    pkg: &KitPkg,
    host: &MdmHost,
    uuid: &str,
) -> Vec<u8> {
    let asset = dict([
        ("kind", Value::from("software-package")),
        ("sha256", Value::from(pkg.sha256.as_str())),
        ("url", Value::from(host.pkg_url(&pkg.sha256))),
    ]);
    let metadata = dict([
        ("bundle-identifier", Value::from(pkg.identifier.as_str())),
        ("bundle-version", Value::from(pkg.version.as_str())),
        ("kind", Value::from("software")),
        (
            "title",
            Value::from(format!(
                "hostkeeper signing test pkg {}: {}",
                spec.number, spec.what
            )),
        ),
    ]);
    let item = dict([
        ("assets", Value::Array(vec![Value::Dictionary(asset)])),
        ("metadata", Value::Dictionary(metadata)),
    ]);
    let manifest = dict([("items", Value::Array(vec![Value::Dictionary(item)]))]);
    command(
        "InstallEnterpriseApplication",
        dict([("Manifest", Value::Dictionary(manifest))]),
        uuid,
    )
}

/// The profile that installs the root certificate `root_der` system-wide, with the identifier
/// `identifier`.
pub(crate) fn root_profile(identifier: &str, root_der: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(root_der);
    let payload_uuid = |tag: &[u8]| {
        let mut h = Sha256::new();
        h.update(tag);
        h.update(digest);
        uuid_of(&h.finalize())
    };
    let cert = dict([
        ("PayloadCertificateFileName", Value::from("root-a.cer")),
        ("PayloadContent", Value::Data(root_der.to_vec())),
        (
            "PayloadDisplayName",
            Value::from("hostkeeper pkg signing test root A"),
        ),
        (
            "PayloadIdentifier",
            Value::from(format!("{identifier}.cert")),
        ),
        ("PayloadType", Value::from("com.apple.security.root")),
        ("PayloadUUID", Value::from(payload_uuid(b"cert\0"))),
        ("PayloadVersion", Value::from(1_u64)),
    ]);
    xml(&Value::Dictionary(dict([
        (
            "PayloadContent",
            Value::Array(vec![Value::Dictionary(cert)]),
        ),
        (
            "PayloadDescription",
            Value::from(
                "The root of the farm installer identity of hostkeeper's pkg signing test. \
                 Remove it when the test is done.",
            ),
        ),
        (
            "PayloadDisplayName",
            Value::from("hostkeeper pkg signing test root"),
        ),
        ("PayloadIdentifier", Value::from(identifier)),
        ("PayloadScope", Value::from("System")),
        ("PayloadType", Value::from("Configuration")),
        ("PayloadUUID", Value::from(payload_uuid(b"profile\0"))),
        ("PayloadVersion", Value::from(1_u64)),
    ])))
}
