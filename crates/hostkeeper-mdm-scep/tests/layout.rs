//! Messages whose layout is wrong: no reply can be made, and each says which part is.

mod common;

use common::*;
use hostkeeper_mdm_scep::Error;

/// A signed-data message from the given children of its `SignedData`.
fn message(signed_data_children: &[Vec<u8>]) -> Vec<u8> {
    seq(&[
        oid_der(SIGNED_DATA),
        context(true, &seq(signed_data_children)),
    ])
}

fn malformed(bytes: &[u8]) -> &'static str {
    match pki(bytes, &mut Store::default()) {
        Err(Error::Malformed(what)) => what,
        other => panic!("{:?}", other.map(|r| r.verdict)),
    }
}

/// The parts of a valid `SignedData`, to change one of.
fn parts() -> [Vec<u8>; 3] {
    [
        int(1),
        set(&[alg(SHA256)]),
        seq(&[oid_der(DATA), context(true, &octets(b"x"))]),
    ]
}

fn signer(attributes: &[u8], signature: Vec<u8>) -> Vec<u8> {
    let client = Client::new(1024);
    let tbs = client.cert.tbs_certificate();
    let sid = seq(&[
        der::Encode::to_der(tbs.issuer()).expect("DER"),
        der::Encode::to_der(tbs.serial_number()).expect("DER"),
    ]);
    seq(&[
        int(1),
        sid,
        alg(SHA256),
        attributes.to_vec(),
        alg(RSA),
        signature,
    ])
}

fn attributes() -> Vec<u8> {
    context(
        true,
        &[
            attr(TRANSACTION_ID, printable("T")),
            attr(SENDER_NONCE, octets(&[1; 16])),
        ]
        .concat(),
    )
}

fn with_signers(signers: &[Vec<u8>]) -> Vec<u8> {
    let [a, b, c] = parts();
    message(&[a, b, c, set(signers)])
}

#[test]
fn a_signed_data_that_is_not_laid_out_as_rfc_5652_says_has_no_reply() {
    let [a, b, c] = parts();
    assert_eq!(
        malformed(&seq(&[
            oid_der(SIGNED_DATA),
            context(true, &[seq(&[]), seq(&[])].concat())
        ])),
        "signed data"
    );
    assert_eq!(malformed(&message(&[int(1)])), "signed data");
    let no_content = message(&[a.clone(), b.clone(), seq(&[oid_der(DATA)])]);
    assert_eq!(malformed(&no_content), "no content");
    let empty = seq(&[oid_der(DATA), context(true, &[])]);
    assert_eq!(
        malformed(&message(&[a.clone(), b.clone(), empty])),
        "content"
    );
    let not_octets = seq(&[oid_der(DATA), context(true, &int(1))]);
    assert_eq!(
        malformed(&message(&[a.clone(), b.clone(), not_octets])),
        "content"
    );
    assert_eq!(
        malformed(&message(&[a.clone(), b.clone(), c.clone()])),
        "no signer"
    );
}

#[test]
fn a_message_must_have_exactly_one_signer_with_the_six_fields() {
    let one = signer(&attributes(), octets(&[0; 4]));
    assert_eq!(
        malformed(&with_signers(&[one.clone(), one.clone()])),
        "not one signer"
    );
    assert_eq!(malformed(&with_signers(&[])), "not one signer");
    assert_eq!(malformed(&with_signers(&[seq(&[int(1)])])), "signer");
}

#[test]
fn a_signer_with_no_signed_attributes_or_a_signature_that_is_not_octets_has_no_reply() {
    let not_octets = signer(&attributes(), int(1));
    assert_eq!(malformed(&with_signers(&[not_octets])), "signature");
    let plain_context = context(false, &[]);
    let no_attributes = signer(&plain_context, octets(&[0; 4]));
    assert_eq!(
        malformed(&with_signers(&[no_attributes])),
        "no signed attributes"
    );
}

#[test]
fn an_attribute_that_is_not_an_oid_and_a_set_has_no_reply() {
    let bad = context(true, &seq(&[oid_der(MESSAGE_TYPE)]));
    let one = signer(&bad, octets(&[0; 4]));
    assert_eq!(malformed(&with_signers(&[one])), "attribute");
}
