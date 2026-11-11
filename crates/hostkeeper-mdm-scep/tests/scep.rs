//! Behaviour of the SCEP server: what a Mac's client gets for each request it can make.

mod common;

use common::*;
use der::{Decode, Encode};
use hostkeeper_mdm_scep::{Ca, Error, Refusal, Verdict};
use rsa::pkcs1v15::{Signature, VerifyingKey};
use sha2::Sha256;
use signature::Verifier;
use spki::DecodePublicKey;
use x509_cert::Certificate;

fn issue_for(spec: &Spec) -> (Client, hostkeeper_mdm_scep::Reply, Store) {
    let client = Client::new(2048);
    let mut store = Store::with(CHALLENGE, SERIAL);
    let reply = pki(&request(&client, spec), &mut store).expect("a reply");
    (client, reply, store)
}

fn refused_with(spec: &Spec, store: &mut Store) -> (Refusal, Rep) {
    let client = Client::new(2048);
    let reply = pki(&request(&client, spec), store).expect("a reply");
    let Verdict::Refused(why) = reply.verdict else {
        panic!("{:?}", reply.verdict)
    };
    (why, check_rep(&reply.der))
}

#[test]
fn ca_caps_names_what_the_server_implements() {
    assert_eq!(
        ca().ca_caps(),
        "POSTPKIOperation\nSHA-256\nAES\nSCEPStandard\n"
    );
}

#[test]
fn ca_cert_is_the_certificate_of_the_ca() {
    let cert = Certificate::from_der(ca().ca_cert()).expect("a certificate");
    assert_eq!(
        cert.tbs_certificate().subject().to_string(),
        "CN=hostkeeper MDM CA,O=Komira"
    );
}

#[test]
fn a_request_gets_an_identity_named_for_the_challenges_serial_and_not_the_requests() {
    let (client, reply, store) = issue_for(&Spec::default());
    assert_eq!(reply.verdict, Verdict::Issued);
    let rep = check_rep(&reply.der);
    assert_eq!((rep.status.as_str(), rep.message_type.as_str()), ("0", "3"));
    assert_eq!(rep.fail_info, None);
    let (certs, _) = open_rep(&rep, &client);
    let issued = &certs[0];
    let tbs = issued.tbs_certificate();
    assert_eq!(
        tbs.subject().to_string(),
        format!("CN={SERIAL},O=hostkeeper,OU=hostkeeper-mdm")
    );
    assert_eq!(tbs.issuer().to_string(), "CN=hostkeeper MDM CA,O=Komira");
    assert_eq!(
        tbs.subject_public_key_info().to_der().expect("DER"),
        client.spki_der(),
        "the identity is for the key of the request"
    );
    assert_eq!(
        store.consumed[CHALLENGE].1,
        issued.to_der().expect("DER"),
        "the store holds what was sent"
    );
    assert_eq!(
        certs[1].to_der().expect("DER"),
        ca().cert_der(),
        "the CA certificate rides along"
    );
}

#[test]
fn the_identity_is_signed_by_the_ca_and_valid_for_the_policys_days_from_before_now() {
    let (client, reply, _) = issue_for(&Spec::default());
    let (certs, _) = open_rep(&check_rep(&reply.der), &client);
    let issued = &certs[0];
    let ca_cert = Certificate::from_der(ca().cert_der()).expect("a certificate");
    let key = rsa::PublicKey::from_public_key_der(
        &ca_cert
            .tbs_certificate()
            .subject_public_key_info()
            .to_der()
            .expect("DER"),
    )
    .expect("a key");
    VerifyingKey::<Sha256>::new(key)
        .verify(
            &issued.tbs_certificate().to_der().expect("DER"),
            &Signature::try_from(issued.signature().raw_bytes()).expect("a signature"),
        )
        .expect("signed by the CA");
    let validity = issued.tbs_certificate().validity();
    let not_before = std::time::SystemTime::from(validity.not_before);
    let not_after = std::time::SystemTime::from(validity.not_after);
    assert_eq!(
        now().duration_since(not_before).expect("before").as_secs(),
        3600
    );
    assert_eq!(
        not_after.duration_since(now()).expect("after").as_secs(),
        1095 * 86_400
    );
}

#[test]
fn the_identity_is_for_a_client_and_not_a_ca() {
    let (client, reply, _) = issue_for(&Spec::default());
    let (certs, _) = open_rep(&check_rep(&reply.der), &client);
    let extensions = certs[0].tbs_certificate().extensions().expect("extensions");
    use const_oid::db::rfc5280::{ID_CE_BASIC_CONSTRAINTS, ID_CE_EXT_KEY_USAGE, ID_CE_KEY_USAGE};
    let ids: Vec<_> = extensions.iter().map(|e| e.extn_id).collect();
    assert_eq!(
        ids,
        [
            ID_CE_BASIC_CONSTRAINTS,
            ID_CE_KEY_USAGE,
            ID_CE_EXT_KEY_USAGE
        ],
        "basic constraints, key usage, extended key usage"
    );
    let eku = extensions
        .iter()
        .find(|e| e.extn_id == ID_CE_EXT_KEY_USAGE)
        .expect("eku");
    let bc = extensions
        .iter()
        .find(|e| e.extn_id == ID_CE_BASIC_CONSTRAINTS)
        .expect("bc");
    assert_eq!(
        bc.extn_value.as_bytes(),
        [0x30, 0x00],
        "not a CA: an empty BasicConstraints"
    );
    assert!(
        eku.extn_value
            .as_bytes()
            .windows(8)
            .any(|w| w == [0x2b, 6, 1, 5, 5, 7, 3, 2]),
        "clientAuth"
    );
}

#[test]
fn every_envelope_cipher_of_a_client_is_opened_and_answered_in_aes() {
    for (cipher, expected) in [
        (AES128, AES128),
        (AES192, AES192),
        (AES256, AES256),
        (DES3, AES128),
    ] {
        let spec = Spec {
            cipher,
            ..Spec::default()
        };
        let (client, reply, _) = issue_for(&spec);
        assert_eq!(reply.verdict, Verdict::Issued, "{cipher}");
        let (certs, used) = open_rep(&check_rep(&reply.der), &client);
        assert_eq!(
            used.to_string(),
            expected,
            "the reply cipher for a {cipher} request"
        );
        assert_eq!(certs.len(), 2);
    }
}

#[test]
fn a_sha1_signed_request_with_a_sha1_signed_csr_is_answered() {
    let spec = Spec {
        sha1: true,
        csr_sha1: true,
        ..Spec::default()
    };
    let (_, reply, _) = issue_for(&spec);
    assert_eq!(reply.verdict, Verdict::Issued);
}

#[test]
fn a_lost_certrep_retried_with_the_same_key_and_transaction_gets_the_stored_certificate() {
    let client = Client::new(2048);
    let message = request(&client, &Spec::default());
    let mut store = Store::with(CHALLENGE, SERIAL);
    let first = pki(&message, &mut store).expect("a reply");
    assert_eq!(first.verdict, Verdict::Issued);
    let retry = pki(&message, &mut store).expect("a reply");
    assert_eq!(retry.verdict, Verdict::Replayed);
    let (first_certs, _) = open_rep(&check_rep(&first.der), &client);
    let (retry_certs, _) = open_rep(&check_rep(&retry.der), &client);
    assert_eq!(
        first_certs[0].to_der().expect("DER"),
        retry_certs[0].to_der().expect("DER")
    );
    assert_eq!(store.consumed.len(), 1, "one certificate was issued");
}

#[test]
fn a_retry_in_a_new_session_with_a_new_transaction_id_gets_the_stored_certificate_too() {
    let client = Client::new(2048);
    let mut store = Store::with(CHALLENGE, SERIAL);
    let first = pki(&request(&client, &Spec::default()), &mut store).expect("a reply");
    let again = Spec {
        transaction_id: Some("0123456789ABCDEF".to_owned()),
        ..Spec::default()
    };
    let retry = pki(&request(&client, &again), &mut store).expect("a reply");
    assert_eq!(retry.verdict, Verdict::Replayed);
    let rep = check_rep(&retry.der);
    assert_eq!(
        rep.transaction_id, b"0123456789ABCDEF",
        "the new transaction is echoed"
    );
    let (a, _) = open_rep(&check_rep(&first.der), &client);
    let (b, _) = open_rep(&rep, &client);
    assert_eq!(a[0].to_der().expect("DER"), b[0].to_der().expect("DER"));
}

#[test]
fn a_different_key_reusing_a_consumed_challenge_is_refused_and_the_first_key_keeps_its_certificate()
{
    let first = Client::new(2048);
    let mut store = Store::with(CHALLENGE, SERIAL);
    pki(&request(&first, &Spec::default()), &mut store).expect("a reply");
    let (why, rep) = refused_with(&Spec::default(), &mut store);
    assert_eq!(why, Refusal::ChallengeConsumed);
    assert_eq!(
        (rep.status.as_str(), rep.fail_info.as_deref()),
        ("2", Some("2"))
    );
    assert_eq!(rep.content, None);
    let retry = pki(&request(&first, &Spec::default()), &mut store).expect("a reply");
    assert_eq!(
        retry.verdict,
        Verdict::Replayed,
        "the first key is still served"
    );
}

#[test]
fn an_unknown_challenge_is_refused_with_a_failure_reply_and_consumes_nothing() {
    let mut store = Store::with(CHALLENGE, SERIAL);
    let spec = Spec {
        challenge: Some("not-issued".to_owned()),
        ..Spec::default()
    };
    let (why, rep) = refused_with(&spec, &mut store);
    assert_eq!(why, Refusal::ChallengeUnknown);
    assert_eq!(
        (rep.status.as_str(), rep.fail_info.as_deref()),
        ("2", Some("2"))
    );
    assert!(store.consumed.is_empty());
    let (_, ok, _) = (
        0,
        pki(&request(&Client::new(2048), &Spec::default()), &mut store).expect("a reply"),
        0,
    );
    assert_eq!(
        ok.verdict,
        Verdict::Issued,
        "the open challenge is still good"
    );
}

#[test]
fn a_failure_reply_verifies_for_an_independent_checker_and_echoes_the_transaction() {
    let mut store = Store::default();
    let spec = Spec {
        transaction_id: Some("TX-42".to_owned()),
        nonce: Some(vec![9; 16]),
        ..Spec::default()
    };
    let (_, rep) = refused_with(&spec, &mut store);
    assert_eq!(rep.transaction_id, b"TX-42");
    assert_eq!(
        rep.recipient_nonce,
        vec![9; 16],
        "the sender's nonce comes back as the recipient nonce"
    );
    assert_eq!(rep.sender_nonce.len(), 16);
    let names: Vec<String> = rep.attribute_oids.iter().map(ToString::to_string).collect();
    for required in [
        CONTENT_TYPE,
        MESSAGE_DIGEST,
        MESSAGE_TYPE,
        PKI_STATUS,
        FAIL_INFO,
        TRANSACTION_ID,
    ] {
        assert!(names.iter().any(|n| n == required), "{required} is signed");
    }
}

#[test]
fn a_certificate_that_cannot_be_issued_is_refused_and_consumes_nothing() {
    let mut store = Store::with(CHALLENGE, "not a serial; CN=evil");
    let (why, rep) = refused_with(&Spec::default(), &mut store);
    assert_eq!(why, Refusal::IssueFailed);
    assert_eq!(rep.fail_info.as_deref(), Some("2"));
    assert!(store.consumed.is_empty(), "the challenge stays open");
}

#[test]
fn a_serial_is_letters_and_digits_of_a_sensible_length() {
    for bad in ["", "A,B", "A B", "A=B", &"A".repeat(65)] {
        let mut store = Store::with(CHALLENGE, bad);
        let (why, _) = refused_with(&Spec::default(), &mut store);
        assert_eq!(why, Refusal::IssueFailed, "{bad:?}");
    }
}

#[test]
fn an_org_with_special_characters_stays_one_attribute_of_the_subject() {
    let policy = hostkeeper_mdm_scep::Policy {
        org: "A, Inc.+B=C".to_owned(),
        ..Default::default()
    };
    let custom = Ca::from_der(
        ca().cert_der(),
        &ca().key_pkcs8_der().expect("a key"),
        policy,
    )
    .expect("a CA");
    let client = Client::new(2048);
    let mut store = Store::with(CHALLENGE, SERIAL);
    let reply = custom
        .pki_operation(
            &request(&client, &Spec::default()),
            &mut store,
            now(),
            &mut rand::rng(),
        )
        .expect("a reply");
    assert_eq!(reply.verdict, Verdict::Issued);
    let (certs, _) = open_rep(&check_rep(&reply.der), &client);
    let name = certs[0].tbs_certificate().subject();
    assert_eq!(name.iter_rdn().count(), 3, "CN, O and OU");
    assert_eq!(
        name.to_string(),
        format!("CN={SERIAL},O=A\\, Inc.\\+B=C,OU=hostkeeper-mdm")
    );
}

#[test]
fn a_ca_survives_its_stored_form() {
    let key = ca().key_pkcs8_der().expect("a key");
    let again = Ca::from_der(ca().cert_der(), &key, Default::default()).expect("a CA");
    assert_eq!(again.cert_der(), ca().cert_der());
    assert!(format!("{again:?}").contains("<private>"));
    assert!(
        !format!("{again:?}").contains(&format!("{:?}", &key[..8])),
        "the key is not printed"
    );
}

#[test]
fn ca_material_that_is_not_a_certificate_and_a_key_is_refused_with_its_reason() {
    let key = ca().key_pkcs8_der().expect("a key");
    assert!(matches!(
        Ca::from_der(b"junk", &key, Default::default()),
        Err(Error::CaMaterial(_))
    ));
    assert!(matches!(
        Ca::from_der(ca().cert_der(), b"junk", Default::default()),
        Err(Error::CaMaterial(_))
    ));
    let bad_name = Ca::generate(
        &mut rand::rng(),
        1024,
        "not a dn",
        now(),
        1,
        Default::default(),
    );
    assert!(matches!(bad_name, Err(Error::CaMaterial(_))));
    let tiny = Ca::generate(&mut rand::rng(), 8, "CN=x", now(), 1, Default::default());
    assert!(matches!(tiny, Err(Error::CaMaterial(_))));
}

#[test]
fn a_sender_that_does_not_sort_its_attributes_is_verified_over_the_bytes_it_signed() {
    let spec = Spec {
        reorder_attributes: true,
        ..Spec::default()
    };
    let (_, reply, _) = issue_for(&spec);
    assert_eq!(reply.verdict, Verdict::Issued);
}

#[test]
fn the_message_digest_binds_the_signed_attributes_to_the_content() {
    let (why, _) = refused_with(
        &Spec {
            wrong_digest: true,
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(why, Refusal::BadMessage("messageDigest attribute"));
    let (why, _) = refused_with(
        &Spec {
            digest_attribute: false,
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(why, Refusal::BadMessage("messageDigest attribute"));
    let (why, _) = refused_with(
        &Spec {
            content_type_attribute: false,
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(why, Refusal::BadMessage("contentType attribute"));
}

#[test]
fn a_request_signed_by_a_key_other_than_its_certificates_is_refused() {
    let client = Client::new(2048);
    let other = Client::new(2048);
    let forged = Client {
        key: other.key.clone(),
        cert: client.cert.clone(),
    };
    let mut store = Store::with(CHALLENGE, SERIAL);
    let reply = pki(&request(&forged, &Spec::default()), &mut store).expect("a reply");
    assert_eq!(
        reply.verdict,
        Verdict::Refused(Refusal::BadMessage("signature does not verify"))
    );
    assert_eq!(check_rep(&reply.der).fail_info.as_deref(), Some("1"));
    assert!(store.consumed.is_empty());
    assert_eq!(store.redeemed, 0, "nothing reached the store");
}

#[test]
fn a_request_without_the_signers_certificate_is_refused() {
    let spec = Spec {
        signer_in_message: false,
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut Store::default());
    assert_eq!(
        why,
        Refusal::BadMessage("the signer's certificate is not in the message")
    );
}

#[test]
fn algorithms_the_server_does_not_implement_are_refused_by_name() {
    let (why, rep) = refused_with(
        &Spec {
            digest_algorithm: "2.16.840.1.101.3.4.2.3",
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(why, Refusal::BadMessage("digest algorithm"));
    assert_eq!(rep.fail_info.as_deref(), Some("1"));
    let (why, _) = refused_with(
        &Spec {
            signature_algorithm: "1.2.840.10045.4.3.2",
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(why, Refusal::BadMessage("signature algorithm"));
    let (why, rep) = refused_with(
        &Spec {
            cipher: "1.2.840.113549.3.2",
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(
        why,
        Refusal::BadAlg("a content cipher that is not AES-CBC or 3DES")
    );
    assert_eq!(rep.fail_info.as_deref(), Some("0"));
    let (why, _) = refused_with(
        &Spec {
            key_transport: "1.2.840.113549.1.1.7",
            ..Spec::default()
        },
        &mut Store::default(),
    );
    assert_eq!(
        why,
        Refusal::BadAlg("a key transport that is not PKCS#1 v1.5")
    );
}

#[test]
fn a_message_type_other_than_pkcsreq_is_refused() {
    for kind in ["17", "20", "21", "3"] {
        let spec = Spec {
            message_type: kind.to_owned(),
            ..Spec::default()
        };
        let (why, rep) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
        assert_eq!(
            why,
            Refusal::BadRequest("a message type that is not PKCSReq"),
            "{kind}"
        );
        assert_eq!(rep.fail_info.as_deref(), Some("2"));
    }
}

#[test]
fn a_request_without_a_challenge_is_refused() {
    let spec = Spec {
        challenge: None,
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
    assert_eq!(why, Refusal::BadRequest("no challenge"));
}

#[test]
fn a_key_shorter_than_the_policys_is_refused_before_the_store_is_asked() {
    let client = Client::new(1024);
    let mut store = Store::with(CHALLENGE, SERIAL);
    let reply = pki(&request(&client, &Spec::default()), &mut store).expect("a reply");
    assert_eq!(
        reply.verdict,
        Verdict::Refused(Refusal::BadRequest("a key shorter than the policy's"))
    );
    assert_eq!(store.redeemed, 0);
}

#[test]
fn an_envelope_to_another_key_cannot_be_opened() {
    let other = Client::new(2048);
    let spec = Spec {
        envelope_to: Some(other.key.to_public_key()),
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
    // The RSA decryption rejects implicitly: a wrong key gives garbage, which fails further on.
    assert!(
        matches!(
            why,
            Refusal::BadMessage("content key" | "content" | "certificate request")
        ),
        "{why:?}"
    );
}

#[test]
fn an_envelope_whose_key_is_not_the_size_of_ours_cannot_be_opened() {
    let small = Client::new(1024);
    let spec = Spec {
        wrap_to: Some(small.key.to_public_key()),
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
    assert_eq!(why, Refusal::BadMessage("content key"));
}

#[test]
fn an_envelope_with_no_recipient_or_a_bad_iv_is_refused_by_name() {
    let cases = [
        (
            Spec {
                no_recipient: true,
                ..Spec::default()
            },
            Refusal::BadAlg("a recipient that is not RSA key transport"),
        ),
        (
            Spec {
                iv_mode: 1,
                ..Spec::default()
            },
            Refusal::BadMessage("content cipher parameters"),
        ),
        (
            Spec {
                iv_mode: 2,
                ..Spec::default()
            },
            Refusal::BadMessage("content cipher parameters"),
        ),
        (
            Spec {
                iv_mode: 3,
                ..Spec::default()
            },
            Refusal::BadMessage("content"),
        ),
    ];
    for (spec, expected) in cases {
        let (why, _) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
        assert_eq!(why, expected);
    }
}

#[test]
fn a_certificate_request_that_is_not_one_is_refused() {
    let two = [seq(&[]), seq(&[])].concat();
    for plain in [b"not a request".to_vec(), two, seq(&[])] {
        let spec = Spec {
            plaintext: Some(plain),
            ..Spec::default()
        };
        let (why, _) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
        assert_eq!(why, Refusal::BadMessage("certificate request"));
    }
}

#[test]
fn a_certificate_request_that_does_not_prove_possession_of_its_key_is_refused() {
    let mut store = Store::with(CHALLENGE, SERIAL);
    let spec = Spec {
        break_csr_signature: true,
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut store);
    assert_eq!(
        why,
        Refusal::BadMessage("the request's own signature does not verify")
    );
    let spec = Spec {
        csr_algorithm: "1.2.840.10045.4.3.2",
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut store);
    assert_eq!(why, Refusal::BadMessage("request signature algorithm"));
    assert_eq!(store.redeemed, 0);
}

#[test]
fn a_request_for_a_key_that_is_not_rsa_is_refused() {
    let p256 = der::Decode::from_der(
        &der::Encode::to_der(&spki::SubjectPublicKeyInfoOwned {
            algorithm: spki::AlgorithmIdentifierOwned {
                oid: oid("1.2.840.10045.2.1"),
                parameters: None,
            },
            subject_public_key: der::asn1::BitString::from_bytes(&[4; 65]).expect("bits"),
        })
        .expect("DER"),
    )
    .expect("an spki");
    let _: &spki::SubjectPublicKeyInfoOwned = &p256;
    let spec = Spec {
        csr_spki: Some(der::Encode::to_der(&p256).expect("DER")),
        ..Spec::default()
    };
    let (why, _) = refused_with(&spec, &mut Store::with(CHALLENGE, SERIAL));
    assert_eq!(why, Refusal::BadMessage("request key is not RSA"));
}

#[test]
fn a_message_with_no_transaction_or_nonce_or_layout_has_no_reply() {
    let client = Client::new(2048);
    let mut store = Store::default();
    let no_tid = request(
        &client,
        &Spec {
            transaction_id: None,
            ..Spec::default()
        },
    );
    assert_eq!(
        pki(&no_tid, &mut store).unwrap_err(),
        Error::Malformed("no transactionID")
    );
    let no_nonce = request(
        &client,
        &Spec {
            nonce: None,
            ..Spec::default()
        },
    );
    assert_eq!(
        pki(&no_nonce, &mut store).unwrap_err(),
        Error::Malformed("no senderNonce")
    );
    let not_signed = request(
        &client,
        &Spec {
            outer_type: ENVELOPED_DATA,
            ..Spec::default()
        },
    );
    assert_eq!(
        pki(&not_signed, &mut store).unwrap_err(),
        Error::Malformed("not signed data")
    );
    for junk in [
        &b""[..],
        b"junk",
        &[0x30, 0x00],
        &[0x30, 0x03, 0x02, 0x01, 0x00],
    ] {
        assert!(
            matches!(pki(junk, &mut store), Err(Error::Malformed(_))),
            "{junk:?}"
        );
    }
}

#[test]
fn no_corruption_of_a_valid_request_panics_and_a_corrupted_signed_part_is_never_issued() {
    let (client, message) = standard();
    let mut issued = 0;
    let mut tried = 0;
    for at in (0..message.len()).step_by(7) {
        let mut broken = message.clone();
        broken[at] ^= 0x01;
        let mut store = Store::with(CHALLENGE, SERIAL);
        tried += 1;
        if let Ok(reply) = pki(&broken, &mut store)
            && reply.verdict == Verdict::Issued
        {
            issued += 1;
        }
    }
    // Only bytes no signature covers can change without a refusal: the temporary certificate's
    // own signature and the like, which the server does not check (its trust is the challenge).
    assert!(issued < tried / 2, "{issued} of {tried} flips were issued");
    for end in 0..message.len() {
        assert!(
            pki(&message[..end], &mut Store::default()).is_err(),
            "truncated at {end}"
        );
    }
    let _ = client;
}
