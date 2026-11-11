//! The enrollment profile a Mac is answered `/enroll` with: a `Configuration` profile that holds a
//! SCEP payload (the one-time challenge, the address of `/scep`) and an MDM payload that names the
//! identity the SCEP payload will produce, the two URLs of the server and the push topic.
//!
//! The live profile of the stack this replaces is a plain unsigned property list and a Mac
//! enrolled through it, so this one is unsigned too (`docs/design/coordinator-mdm-cm12a.md`,
//! item 11, inferred from one Mac). The MDM payload sets `SignMessage`: the Mac signs each request
//! with its identity and sends the signature in the `Mdm-Signature` header, which is the mode that
//! has run on a real Mac.
use plist::{Dictionary, Value};
use sha1::{Digest, Sha1};

/// The access rights an MDM payload asks for: all of them (8191), as the live profile does.
const ACCESS_RIGHTS: i64 = 8191;
/// The SCEP key: RSA 2048 for signing and encryption (`Key Usage` 1 | 4).
const KEY_SIZE: i64 = 2048;
const KEY_USAGE: i64 = 5;
const CAPABILITIES: [&str; 3] = [
    "com.apple.mdm.per-user-connections",
    "com.apple.mdm.bootstraptoken",
    "com.apple.mdm.token",
];
/// The namespace the payload UUIDs are derived under (a random constant of this crate).
const NAMESPACE: [u8; 16] = [
    0x6b, 0xa7, 0xb8, 0x14, 0x5d, 0x1c, 0x4f, 0x37, 0x9e, 0x20, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
];

/// What the profile is made of.
#[derive(Clone, Debug)]
pub struct Enrollment<'a> {
    /// The name the Mac reaches the server by, as `mdm.example.com`.
    pub dns_name: &'a str,
    /// The profile's reverse-DNS identifier; the payloads are `<identifier>.scep` and `.mdm`.
    pub identifier: &'a str,
    /// What Settings shows as the profile's name.
    pub display_name: &'a str,
    /// The organisation named in the profile.
    pub organization: &'a str,
    /// The push topic of the push certificate (`com.apple.mgmt.External.<uuid>`).
    pub topic: &'a str,
    /// The one-time SCEP challenge.
    pub challenge: &'a str,
}

/// Why the profile could not be written.
#[derive(Debug, thiserror::Error)]
#[error("the enrollment profile could not be written: {0}")]
pub struct Error(String);

/// The profile as the bytes of an XML property list ending in a newline.
///
/// # Errors
/// [`Error`]: the property list writer failed.
pub fn render(e: &Enrollment<'_>) -> Result<Vec<u8>, Error> {
    let (profile, scep_uuid, mdm_uuid) = (
        uuid(e.identifier, "profile"),
        uuid(e.identifier, "scep"),
        uuid(e.identifier, "mdm"),
    );
    let scep = dict([
        ("Key Type", text("RSA")),
        ("Challenge", text(e.challenge)),
        ("Key Usage", Value::Integer(KEY_USAGE.into())),
        ("Keysize", Value::Integer(KEY_SIZE.into())),
        ("URL", text(&format!("https://{}/scep", e.dns_name))),
    ]);
    let scep = dict([
        ("PayloadContent", scep),
        ("PayloadIdentifier", text(&format!("{}.scep", e.identifier))),
        ("PayloadType", text("com.apple.security.scep")),
        ("PayloadUUID", text(&scep_uuid)),
        ("PayloadVersion", Value::Integer(1.into())),
    ]);
    let capabilities = CAPABILITIES.iter().map(|c| text(c)).collect();
    let mdm = dict([
        ("AccessRights", Value::Integer(ACCESS_RIGHTS.into())),
        (
            "CheckInURL",
            text(&format!("https://{}/checkin", e.dns_name)),
        ),
        ("CheckOutWhenRemoved", Value::Boolean(true)),
        ("IdentityCertificateUUID", text(&scep_uuid)),
        ("PayloadIdentifier", text(&format!("{}.mdm", e.identifier))),
        ("PayloadType", text("com.apple.mdm")),
        ("PayloadUUID", text(&mdm_uuid)),
        ("PayloadVersion", Value::Integer(1.into())),
        ("ServerCapabilities", Value::Array(capabilities)),
        ("ServerURL", text(&format!("https://{}/mdm", e.dns_name))),
        ("SignMessage", Value::Boolean(true)),
        ("Topic", text(e.topic)),
    ]);
    let profile = dict([
        ("PayloadContent", Value::Array(vec![scep, mdm])),
        ("PayloadDisplayName", text(e.display_name)),
        ("PayloadIdentifier", text(e.identifier)),
        ("PayloadOrganization", text(e.organization)),
        ("PayloadType", text("Configuration")),
        ("PayloadUUID", text(&profile)),
        ("PayloadVersion", Value::Integer(1.into())),
    ]);
    let mut out = Vec::new();
    profile
        .to_writer_xml(&mut out)
        .map_err(|why| Error(why.to_string()))?;
    out.push(b'\n');
    Ok(out)
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

fn dict<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut d = Dictionary::new();
    for (key, value) in pairs {
        d.insert(key.to_owned(), value);
    }
    Value::Dictionary(d)
}

/// The version 5 UUID (RFC 4122: SHA-1 of the namespace and the name) of `identifier/role`, in
/// upper case: the same profile always has the same payload UUIDs.
fn uuid(identifier: &str, role: &str) -> String {
    let mut hash = Sha1::new();
    hash.update(NAMESPACE);
    hash.update(format!("{identifier}/{role}").as_bytes());
    let digest = hash.finalize();
    let mut b = [0_u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|byte| format!("{byte:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}
