//! The enrollment profile, read back with the property-list reader.

use hostkeeper_mdm_ade::profile::{Enrollment, render};
use hostkeeper_mdm_ade::roots;
use plist::Value;

fn sample() -> Enrollment<'static> {
    Enrollment {
        dns_name: "mdm.example.com",
        identifier: "dev.example.mdm",
        display_name: "Example MDM",
        organization: "Example",
        topic: "com.apple.mgmt.External.0123",
        challenge: "c0ffee",
    }
}

fn read(profile: &[u8]) -> plist::Dictionary {
    Value::from_reader(std::io::Cursor::new(profile))
        .expect("a plist")
        .into_dictionary()
        .expect("a dictionary")
}

fn payload(profile: &plist::Dictionary, kind: &str) -> plist::Dictionary {
    profile["PayloadContent"]
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|v| v.as_dictionary())
        .find(|d| d["PayloadType"].as_string() == Some(kind))
        .expect("the payload")
        .clone()
}

/// Proves: the profile is an unsigned XML property list ending in a newline, whose SCEP payload
/// carries the challenge and the address of `/scep`, and whose MDM payload names that identity, the
/// two URLs of the server, the topic, and signs each message.
///
/// Catches: an MDM payload without a check-in URL (the server answers `/checkin` and `/mdm` on
/// different routes), or an identity that is not the SCEP payload's.
#[test]
fn the_profile_names_the_challenge_the_urls_the_topic_and_the_identity() {
    let bytes = render(&sample()).expect("a profile");
    assert!(bytes.starts_with(b"<?xml"));
    assert_eq!(bytes.last(), Some(&b'\n'));
    let profile = read(&bytes);
    assert_eq!(profile["PayloadType"].as_string(), Some("Configuration"));
    assert_eq!(
        profile["PayloadIdentifier"].as_string(),
        Some("dev.example.mdm")
    );
    assert_eq!(
        profile["PayloadDisplayName"].as_string(),
        Some("Example MDM")
    );
    assert_eq!(profile["PayloadOrganization"].as_string(), Some("Example"));
    let scep = payload(&profile, "com.apple.security.scep");
    let content = scep["PayloadContent"].as_dictionary().expect("content");
    assert_eq!(
        content["URL"].as_string(),
        Some("https://mdm.example.com/scep")
    );
    assert_eq!(content["Challenge"].as_string(), Some("c0ffee"));
    assert_eq!(content["Key Type"].as_string(), Some("RSA"));
    assert_eq!(content["Keysize"].as_signed_integer(), Some(2048));
    let mdm = payload(&profile, "com.apple.mdm");
    assert_eq!(
        mdm["ServerURL"].as_string(),
        Some("https://mdm.example.com/mdm")
    );
    assert_eq!(
        mdm["CheckInURL"].as_string(),
        Some("https://mdm.example.com/checkin")
    );
    assert_eq!(
        mdm["Topic"].as_string(),
        Some("com.apple.mgmt.External.0123")
    );
    assert_eq!(mdm["SignMessage"].as_boolean(), Some(true));
    assert_eq!(mdm["CheckOutWhenRemoved"].as_boolean(), Some(true));
    assert_eq!(
        mdm["IdentityCertificateUUID"].as_string(),
        scep["PayloadUUID"].as_string()
    );
}

/// Proves: the payload UUIDs are the same for the same identifier and differ for another, and have
/// the shape of a version 5 UUID.
///
/// Catches: a profile that is a new profile to the Mac on every render.
#[test]
fn payload_uuids_follow_the_identifier() {
    let first = read(&render(&sample()).expect("a profile"));
    let again = read(&render(&sample()).expect("a profile"));
    assert_eq!(first["PayloadUUID"], again["PayloadUUID"]);
    let other = read(
        &render(&Enrollment {
            identifier: "dev.example.other",
            ..sample()
        })
        .expect("a profile"),
    );
    assert_ne!(first["PayloadUUID"], other["PayloadUUID"]);
    let uuid = first["PayloadUUID"].as_string().expect("a uuid");
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "5");
    assert!(uuid.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
    assert_eq!(uuid, uuid.to_ascii_uppercase());
}

/// Proves: the two roots Apple lists are shipped and read.
///
/// Catches: a root file that no longer parses, which would refuse every blob.
#[test]
fn both_apple_roots_are_shipped() {
    let roots = roots::apple();
    assert_eq!(roots.len(), 2);
    assert!(roots.iter().all(|der| der.first() == Some(&0x30)));
}
