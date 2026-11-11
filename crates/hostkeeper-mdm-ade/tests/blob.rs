//! The device blob of `/enroll`, against a fake Apple root. Each refusal is asserted by its
//! variant and its text, because the coordinator audits the text.

mod common;

use common::*;
use der::{Decode, Encode};
use hostkeeper_mdm_ade::blob::{claims, verify};
use hostkeeper_mdm_ade::{Device, Error};

fn check(spec: &Spec) -> Result<Device, Error> {
    verify(&blob(spec), &roots(), now())
}

fn device() -> Device {
    Device {
        serial: SERIAL.to_owned(),
        udid: UDID.to_owned(),
        product: Some("Mac14,13".to_owned()),
        version: Some("26A434".to_owned()),
    }
}

/// Proves: a blob signed by a device identity that chains through an intermediate of the blob to
/// the root given is verified, and the serial, UDID, product and OS build are the plist's.
///
/// Catches: a verifier that refuses the one shape of blob that is good.
#[test]
fn a_blob_signed_by_a_device_under_the_root_is_verified() {
    assert_eq!(check(&Spec::default()), Ok(device()));
}

/// Proves: every digest of the signer info is read, with the signature algorithm that names the
/// hash and with `rsaEncryption`, which names none.
///
/// Catches: a digest that is hashed one way and checked another.
#[test]
fn the_digests_of_rsa_signers_are_all_verified() {
    for (digest, algorithm) in [
        (SHA1, SHA1_RSA),
        (SHA256, SHA256_RSA),
        (SHA384, SHA384_RSA),
        (SHA512, SHA512_RSA),
        (SHA256, RSA),
        (SHA384, RSA),
        (SHA512, RSA),
        (SHA1, RSA),
    ] {
        let spec = Spec {
            digest_alg: digest,
            signature_alg: algorithm,
            ..Spec::default()
        };
        assert_eq!(check(&spec), Ok(device()), "{digest} {algorithm}");
    }
}

/// Proves: a signer whose key is ECDSA P-256 is verified, named by `ecdsa-with-SHA256` or by
/// `id-ecPublicKey` with a SHA-256 digest, and the chain may be signed by an ECDSA key too.
///
/// Catches: a verifier that reads only RSA, which Apple's newer devices may not use.
#[test]
fn an_ecdsa_signer_and_an_ecdsa_issuer_are_verified() {
    let pki = pki();
    let device_key = ec();
    let ec_device = cert(
        "CN=Fake EC Device",
        "CN=Fake Device CA",
        &device_key,
        &pki.inter_key,
        valid(),
    );
    for algorithm in [ECDSA_SHA256, EC_KEY] {
        let spec = Spec {
            signer: device_key.clone(),
            signer_cert: ec_device.clone(),
            certs: vec![ec_device.clone(), pki.inter.clone()],
            signature_alg: algorithm,
            ..Spec::default()
        };
        assert_eq!(check(&spec), Ok(device()), "{algorithm}");
    }
    let ca_key = ec();
    let ca = cert(
        "CN=Fake EC CA",
        "CN=Fake Apple Root",
        &ca_key,
        &pki.root_key,
        valid(),
    );
    let leaf = cert(
        "CN=Fake EC Leaf",
        "CN=Fake EC CA",
        &device_key,
        &ca_key,
        valid(),
    );
    let spec = Spec {
        signer: device_key,
        signer_cert: leaf.clone(),
        certs: vec![leaf, ca],
        signature_alg: ECDSA_SHA256,
        ..Spec::default()
    };
    assert_eq!(check(&spec), Ok(device()));
}

/// Proves: an ECDSA signature other than P-256 with SHA-256 is refused as unsupported, and a
/// signature that is not DER is a bad signature.
///
/// Catches: a hash taken for another than the one the key was used with.
#[test]
fn ecdsa_with_another_hash_is_unsupported_and_a_garbled_one_is_bad() {
    let pki = pki();
    let device_key = ec();
    let ec_device = cert(
        "CN=Fake EC Device",
        "CN=Fake Device CA",
        &device_key,
        &pki.inter_key,
        valid(),
    );
    let base = Spec {
        signer: device_key,
        signer_cert: ec_device.clone(),
        certs: vec![ec_device, pki.inter.clone()],
        signature_alg: ECDSA_SHA256,
        ..Spec::default()
    };
    let sha384 = Spec {
        signature_alg: EC_KEY,
        digest_alg: SHA384,
        ..base.clone()
    };
    assert_eq!(
        check(&sha384),
        Err(Error::Unsupported("ECDSA digest other than SHA-256"))
    );
    let named = Spec {
        signature_alg: ECDSA_SHA384,
        ..base.clone()
    };
    assert_eq!(
        check(&named),
        Err(Error::Unsupported("signature algorithm"))
    );
    let garbled = Spec {
        signature_der: Some(vec![1, 2, 3]),
        ..base.clone()
    };
    assert_eq!(
        check(&garbled),
        Err(Error::BadSignature("a signature does not verify"))
    );
    let rsa_key_for_ec = Spec {
        signature_alg: ECDSA_SHA256,
        ..Spec::default()
    };
    assert_eq!(
        check(&rsa_key_for_ec),
        Err(Error::Unsupported("the signer's key is not P-256"))
    );
    let ec_key_for_rsa = Spec {
        signature_alg: SHA256_RSA,
        ..base
    };
    assert_eq!(
        check(&ec_key_for_rsa),
        Err(Error::Unsupported("the signer's key is not RSA"))
    );
}

/// Proves: a blob with no signed attributes is verified over its content directly.
///
/// Catches: a verifier that insists on attributes that RFC 5652 makes optional.
#[test]
fn a_blob_without_signed_attributes_is_verified_over_its_content() {
    let spec = Spec {
        signed_attributes: false,
        ..Spec::default()
    };
    assert_eq!(check(&spec), Ok(device()));
    let broken = Spec {
        signed_attributes: false,
        break_signature: true,
        ..Spec::default()
    };
    assert_eq!(
        check(&broken),
        Err(Error::BadSignature("a signature does not verify"))
    );
}

/// Proves: the attributes are bound to the content (the `messageDigest`), declare the content
/// type `data`, and are what the signature covers.
///
/// Catches: content swapped under a signature that still verifies over the attributes.
#[test]
fn the_signed_attributes_must_bind_the_content() {
    let wrong = Spec {
        wrong_digest: true,
        ..Spec::default()
    };
    assert_eq!(
        check(&wrong),
        Err(Error::BadSignature("messageDigest attribute"))
    );
    let no_type = Spec {
        content_type: None,
        ..Spec::default()
    };
    assert_eq!(
        check(&no_type),
        Err(Error::BadSignature("contentType attribute"))
    );
    let other_type = Spec {
        content_type: Some(SIGNED_DATA),
        ..Spec::default()
    };
    assert_eq!(
        check(&other_type),
        Err(Error::BadSignature("contentType attribute"))
    );
    let broken = Spec {
        break_signature: true,
        ..Spec::default()
    };
    assert_eq!(
        check(&broken),
        Err(Error::BadSignature("a signature does not verify"))
    );
    let swapped = {
        let mut bytes = blob(&Spec::default());
        let at = bytes
            .windows(SERIAL.len())
            .position(|window| window == SERIAL.as_bytes())
            .expect("the serial is in the blob");
        bytes[at] = b'Q';
        bytes
    };
    assert_eq!(
        verify(&swapped, &roots(), now()),
        Err(Error::BadSignature("messageDigest attribute"))
    );
}

/// Proves: a digest or a signature algorithm that is not implemented is refused by name, as
/// unsupported and not as a bad signature.
///
/// Catches: an algorithm treated as a failed signature, which hides that the blob may be good.
#[test]
fn algorithms_that_are_not_implemented_are_unsupported() {
    let sha224 = Spec {
        digest_alg: SHA224,
        ..Spec::default()
    };
    assert_eq!(check(&sha224), Err(Error::Unsupported("digest algorithm")));
    let plain_sha224 = Spec {
        digest_alg: SHA224,
        signature_alg: RSA,
        signed_attributes: false,
        ..Spec::default()
    };
    assert_eq!(
        check(&plain_sha224),
        Err(Error::Unsupported("digest algorithm"))
    );
    let unknown = Spec {
        signature_alg: "1.2.3.4",
        ..Spec::default()
    };
    assert_eq!(
        check(&unknown),
        Err(Error::Unsupported("signature algorithm"))
    );
}

/// Proves: the signer's certificate must be in the blob and be the one the signer info names by
/// issuer and serial.
///
/// Catches: a signer taken from the wrong certificate of the set.
#[test]
fn the_signers_certificate_must_be_in_the_blob() {
    let pki = pki();
    let absent = Spec {
        certs: vec![pki.inter.clone()],
        ..Spec::default()
    };
    assert_eq!(
        check(&absent),
        Err(Error::Untrusted(
            "the signer's certificate is not in the blob"
        ))
    );
    let other_serial = Spec {
        sid_serial_off: true,
        ..Spec::default()
    };
    assert_eq!(
        check(&other_serial),
        Err(Error::Untrusted(
            "the signer's certificate is not in the blob"
        ))
    );
    let no_certificates = Spec {
        certs: Vec::new(),
        ..Spec::default()
    };
    assert_eq!(
        check(&no_certificates),
        Err(Error::Untrusted(
            "the signer's certificate is not in the blob"
        ))
    );
}

/// Proves: the chain must reach a root given: a blob under another root, with the intermediate
/// missing, or whose intermediate was signed by a key that is not the root's, is not trusted.
///
/// Catches: a blob signed by anyone with a certificate of their own.
#[test]
fn a_chain_that_does_not_reach_the_root_is_not_trusted() {
    let pki = pki();
    let none = Err(Error::Untrusted(
        "a certificate has no issuer that signed it",
    ));
    let no_intermediate = Spec {
        certs: vec![pki.device.clone()],
        ..Spec::default()
    };
    assert_eq!(check(&no_intermediate), none);
    let other_root = cert("CN=Other Root", "CN=Other Root", &rsa(3), &rsa(3), valid());
    assert_eq!(
        verify(&blob(&Spec::default()), &[der(&other_root)], now()),
        none
    );
    assert_eq!(verify(&blob(&Spec::default()), &[], now()), none);
    let forged_inter = cert(
        "CN=Fake Device CA",
        "CN=Fake Apple Root",
        &pki.inter_key,
        &rsa(3),
        valid(),
    );
    let forged = Spec {
        certs: vec![pki.device.clone(), forged_inter],
        ..Spec::default()
    };
    assert_eq!(check(&forged), none);
    // The right root among garbage that is not a certificate.
    let mixed = vec![vec![1, 2, 3], der(&pki.root)];
    assert_eq!(verify(&blob(&Spec::default()), &mixed, now()), Ok(device()));
}

/// Proves: a signer that is the root itself, and a device issued directly by the root, are
/// verified.
///
/// Catches: a walk that always needs an intermediate.
#[test]
fn a_device_under_the_root_and_the_root_itself_are_verified() {
    let pki = pki();
    let direct = cert(
        "CN=Direct Device",
        "CN=Fake Apple Root",
        &pki.device_key,
        &pki.root_key,
        valid(),
    );
    let spec = Spec {
        signer_cert: direct.clone(),
        certs: vec![direct],
        ..Spec::default()
    };
    assert_eq!(check(&spec), Ok(device()));
    let root_signs = Spec {
        signer: pki.root_key.clone(),
        signer_cert: pki.root.clone(),
        certs: vec![pki.root.clone()],
        ..Spec::default()
    };
    assert_eq!(check(&root_signs), Ok(device()));
}

/// Proves: every certificate of the chain must be inside its validity at the time given.
///
/// Catches: an expired device identity, or an intermediate not yet valid, accepted.
#[test]
fn every_certificate_of_the_chain_is_checked_for_its_validity() {
    let pki = pki();
    let outside = Err(Error::Untrusted("a certificate is outside its validity"));
    let expired = cert(
        "CN=Fake Device",
        "CN=Fake Device CA",
        &pki.device_key,
        &pki.inter_key,
        validity(-200, -100),
    );
    let spec = Spec {
        signer_cert: expired.clone(),
        certs: vec![expired, pki.inter.clone()],
        ..Spec::default()
    };
    assert_eq!(check(&spec), outside);
    let early = cert(
        "CN=Fake Device CA",
        "CN=Fake Apple Root",
        &pki.inter_key,
        &pki.root_key,
        validity(100, 200),
    );
    let spec = Spec {
        certs: vec![pki.device.clone(), early],
        ..Spec::default()
    };
    assert_eq!(check(&spec), outside);
    let later = now() + std::time::Duration::from_secs(86_400 * 366);
    assert_eq!(verify(&blob(&Spec::default()), &roots(), later), outside);
}

/// Proves: a chain that goes round in a circle, and one that is longer than the limit, ends in a
/// refusal and not in a loop.
///
/// Catches: a verifier that a pair of cross-signed certificates holds forever.
#[test]
fn a_circular_chain_is_refused() {
    let (key_a, key_b) = (rsa(0), rsa(1));
    let a = cert("CN=A", "CN=B", &key_a, &key_b, valid());
    let b = cert("CN=B", "CN=A", &key_b, &key_a, valid());
    let spec = Spec {
        signer: key_a,
        signer_cert: a.clone(),
        certs: vec![a, b],
        ..Spec::default()
    };
    assert_eq!(check(&spec), Err(Error::Untrusted("the chain is too long")));
}

/// Proves: a certificate that names `rsaEncryption` as its signature algorithm, which says no
/// hash, cannot be an issuer's proof.
///
/// Catches: a hash guessed for a certificate that names none.
#[test]
fn a_certificate_signature_without_a_hash_is_not_proof() {
    let pki = pki();
    let mut bytes = der(&pki.device);
    let from = oid_der(SHA256_RSA);
    let to = oid_der(RSA);
    let mut patched = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(&from) {
            patched.extend_from_slice(&to);
            at += from.len();
        } else {
            patched.push(bytes[at]);
            at += 1;
        }
    }
    bytes = patched;
    let patched_device = x509_cert::Certificate::from_der(&bytes).expect("a certificate");
    let spec = Spec {
        signer_cert: patched_device.clone(),
        certs: vec![patched_device, pki.inter.clone()],
        ..Spec::default()
    };
    assert_eq!(
        check(&spec),
        Err(Error::Untrusted(
            "a certificate has no issuer that signed it"
        ))
    );
}

/// Proves: bytes that are not a `SignedData` of the layout read are malformed, each saying which
/// part, and nothing is panicked on.
///
/// Catches: a parser that indexes past the end of a short message.
#[test]
fn a_blob_of_another_layout_is_malformed() {
    let malformed = |bytes: &[u8]| match verify(bytes, &roots(), now()) {
        Err(Error::Malformed(what)) => what,
        other => panic!("{other:?}"),
    };
    assert_eq!(malformed(&[]), "message");
    assert_eq!(malformed(&[0x30, 0x84, 0xff]), "message");
    assert_eq!(malformed(&seq(&[])), "content info");
    assert_eq!(malformed(&int(1)), "content info");
    assert_eq!(malformed(&[seq(&[]), seq(&[])].concat()), "message");
    assert_eq!(malformed(&seq(&[oid_der(DATA)])), "content info");
    assert_eq!(
        malformed(&seq(&[oid_der(DATA), context(0, true, &[])])),
        "not signed data"
    );
    assert_eq!(
        malformed(&blob(&Spec {
            outer_type: DATA,
            ..Spec::default()
        })),
        "not signed data"
    );
    assert_eq!(
        malformed(&seq(&[int(1), context(0, true, &[])])),
        "content type"
    );
    let signed =
        |children: &[Vec<u8>]| seq(&[oid_der(SIGNED_DATA), context(0, true, &seq(children))]);
    assert_eq!(
        malformed(&seq(&[oid_der(SIGNED_DATA), context(0, true, &[])])),
        "signed data"
    );
    assert_eq!(
        malformed(&seq(&[
            oid_der(SIGNED_DATA),
            context(0, true, &[int(1), int(2)].concat())
        ])),
        "signed data"
    );
    assert_eq!(
        malformed(&seq(&[oid_der(SIGNED_DATA), context(0, true, &int(1))])),
        "signed data"
    );
    assert_eq!(
        malformed(&signed(&[int(1), set(&[]), int(3)])),
        "encapsulated content"
    );
    assert_eq!(
        malformed(&signed(&[int(1), set(&[]), seq(&[oid_der(DATA)])])),
        "no content"
    );
    assert_eq!(
        malformed(&signed(&[int(1), set(&[]), seq(&[oid_der(DATA), int(2)])])),
        "content"
    );
    assert_eq!(
        malformed(&signed(&[
            int(1),
            set(&[]),
            seq(&[
                oid_der(DATA),
                context(0, true, &[octets(b"a"), octets(b"b")].concat())
            ])
        ])),
        "content"
    );
    assert_eq!(
        malformed(&signed(&[
            int(1),
            set(&[]),
            seq(&[oid_der(DATA), context(0, true, &int(7))])
        ])),
        "content"
    );
    assert_eq!(
        malformed(&blob(&Spec {
            detached: true,
            ..Spec::default()
        })),
        "no content"
    );
}

/// Proves: signer infos of the wrong shape are malformed: none, two, too short, a signature that
/// is not an octet string, attributes that are not attributes.
///
/// Catches: a blob with a second signer read as if it had one.
#[test]
fn signer_infos_of_another_shape_are_malformed() {
    let malformed = |bytes: &[u8]| match verify(bytes, &roots(), now()) {
        Err(Error::Malformed(what)) => what,
        other => panic!("{other:?}"),
    };
    for count in [0, 2] {
        let spec = Spec {
            signer_count: count,
            ..Spec::default()
        };
        assert_eq!(malformed(&blob(&spec)), "not one signer");
    }
    let content = seq(&[
        oid_der(DATA),
        context(0, true, &octets(&plist(SERIAL, UDID))),
    ]);
    let with_signers = |signers: Vec<u8>| {
        let signed = seq(&[int(1), set(&[]), content.clone(), signers]);
        seq(&[oid_der(SIGNED_DATA), context(0, true, &signed)])
    };
    assert_eq!(malformed(&with_signers(context(0, true, &[]))), "signers");
    assert_eq!(malformed(&with_signers(set(&[int(1)]))), "signer");
    assert_eq!(
        malformed(&with_signers(set(&[seq(&[int(1), int(2)])]))),
        "signer"
    );
    assert_eq!(
        malformed(&with_signers(set(&[seq(&[
            int(1),
            int(2),
            alg(SHA256),
            alg(RSA)
        ])]))),
        "signer"
    );
    let sid = seq(&[
        pki()
            .device
            .tbs_certificate()
            .issuer()
            .to_der()
            .expect("DER"),
        pki()
            .device
            .tbs_certificate()
            .serial_number()
            .to_der()
            .expect("DER"),
    ]);
    let signer = |fields: Vec<Vec<u8>>| with_signers(set(&[seq(&fields)]));
    assert_eq!(
        malformed(&signer(vec![
            int(1),
            sid.clone(),
            alg(SHA256),
            alg(RSA),
            int(5)
        ])),
        "signature"
    );
    assert_eq!(
        malformed(&signer(vec![
            int(1),
            int(5),
            alg(SHA256),
            alg(RSA),
            octets(b"s")
        ])),
        "signer identifier"
    );
    assert_eq!(
        malformed(&signer(vec![
            int(1),
            sid.clone(),
            int(5),
            alg(RSA),
            octets(b"s")
        ])),
        "algorithm"
    );
    assert_eq!(
        malformed(&signer(vec![
            int(1),
            sid.clone(),
            seq(&[]),
            alg(RSA),
            octets(b"s")
        ])),
        "algorithm"
    );
    let attributes = |inner: Vec<u8>| {
        signer(vec![
            int(1),
            sid.clone(),
            alg(SHA256),
            context(0, true, &inner),
            alg(RSA),
            octets(b"s"),
        ])
    };
    assert_eq!(malformed(&attributes(int(1))), "attribute");
    assert_eq!(
        malformed(&attributes(seq(&[oid_der(CONTENT_TYPE)]))),
        "attribute"
    );
    assert_eq!(
        malformed(&attributes(seq(&[oid_der(CONTENT_TYPE), int(1)]))),
        "attribute values"
    );
    assert_eq!(
        malformed(&attributes(seq(&[oid_der(CONTENT_TYPE), set(&[])]))),
        "attribute values"
    );
    assert_eq!(
        malformed(&attributes(seq(&[int(1), set(&[int(1)])]))),
        "attribute"
    );
    // A contentType that is no OID is read as absent and refused at verification.
    let odd = attributes(attr(CONTENT_TYPE, int(1)));
    assert_eq!(
        verify(&odd, &roots(), now()),
        Err(Error::Untrusted(
            "the signer's certificate is not in the blob"
        ))
    );
}

/// Proves: `claims` reads the serial and UDID of a blob that was never verified, and says what is
/// wrong with one that names no device.
///
/// Catches: the unverified read trusted as if it were the verified one (the coordinator never
/// does), and a plist that is a different shape accepted.
#[test]
fn claims_read_the_plist_of_an_unverified_blob() {
    let forged = Spec {
        break_signature: true,
        certs: Vec::new(),
        ..Spec::default()
    };
    assert_eq!(claims(&blob(&forged)), Ok(device()));
    let content = |bytes: &[u8]| {
        claims(&blob(&Spec {
            content: bytes.to_vec(),
            ..Spec::default()
        }))
    };
    assert_eq!(
        content(b"not a plist"),
        Err(Error::NoDevice("the content is not a property list"))
    );
    assert_eq!(
        content(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><array/></plist>"),
        Err(Error::NoDevice("the plist is not a dictionary"))
    );
    assert_eq!(content(&plist("", UDID)), Err(Error::NoDevice("no SERIAL")));
    assert_eq!(content(&plist(SERIAL, "")), Err(Error::NoDevice("no UDID")));
    let minimal = content(
        b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>SERIAL</key><string>A1</string>\
          <key>UDID</key><string>U1</string><key>PRODUCT</key><integer>3</integer></dict></plist>",
    );
    assert_eq!(
        minimal,
        Ok(Device {
            serial: "A1".to_owned(),
            udid: "U1".to_owned(),
            product: None,
            version: None
        })
    );
    assert_eq!(claims(&[]), Err(Error::Malformed("message")));
}

/// Proves: a blob that verifies but whose plist names no device is refused as not naming one, after
/// verification.
///
/// Catches: a verified blob with an empty serial passed on as a device.
#[test]
fn a_verified_blob_that_names_no_device_is_refused() {
    let spec = Spec {
        content: plist("", UDID),
        ..Spec::default()
    };
    assert_eq!(check(&spec), Err(Error::NoDevice("no SERIAL")));
}
