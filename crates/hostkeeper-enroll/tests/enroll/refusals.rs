//! Each check an issued certificate must pass, and each way the coordinator can break the
//! enrollment protocol. Every refusal leaves the pki directory as it was.

use std::path::PathBuf;

use hostkeeper_enroll::{
    Error, HostId, Issued, NONCE_MAX, NONCE_MIN, Outcome, PollAnswer, ProtocolError, Reason,
    Refusal, RequestId,
};
use x509_cert::Certificate;
use x509_cert::der::oid::db::rfc5280::{ID_KP_CLIENT_AUTH, ID_KP_SERVER_AUTH};
use x509_cert::der::{DecodePem, Encode};

use crate::support::{Args, Ca, HOST, T0, WEEK, World, eku, pem, proper};

/// A world whose coordinator issues what `issue` makes.
fn issuing(name: &str, issue: impl FnMut(&Ca, &Args) -> Issued + Send + 'static) -> World {
    let world = World::new(name);
    world.coordinator().issue = Some(Box::new(issue));
    world
}

/// The refusal of the first certificate `issue` makes, after which nothing is stored.
fn first_refusal(name: &str, issue: impl FnMut(&Ca, &Args) -> Issued + Send + 'static) -> Error {
    let world = issuing(name, issue);
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    let refused = enroller.run_once().expect_err("refused");
    assert_eq!(world.current(), None, "nothing was stored");
    assert!(world.pki().join("pending/key").exists(), "the key is kept");
    refused
}

/// A certificate for the request's key with `subject` and `extensions`, valid `window`.
fn leaf(
    ca: &Ca,
    args: &Args,
    subject: &str,
    extensions: Vec<x509_cert::ext::Extension>,
    window: (u64, u64),
) -> Issued {
    Issued {
        host_id: args.host_id.clone(),
        cert_pem: ca.leaf(&args.spki, subject, extensions, window),
        ca_bundle_pem: ca.pem.clone(),
    }
}

fn refusal(error: &Error) -> Refusal {
    match error {
        Error::Refused(refusal) => refusal.clone(),
        other => panic!("not a refused certificate: {other:?}"),
    }
}

/// Catches a certificate taken for a host the coordinator did not name.
#[test]
fn a_certificate_without_a_host_id_is_refused() {
    let refused = first_refusal("no-host-id", |ca: &Ca, args: &Args| Issued {
        host_id: String::new(),
        ..proper(ca, args)
    });
    assert_eq!(refusal(&refused), Refusal::NoHostId);
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the coordinator named no host id"
    );
}

/// Catches text that is not a certificate, or a bundle that is not certificates, being stored.
#[test]
fn a_certificate_or_bundle_that_is_not_pem_is_refused() {
    let refused = first_refusal("not-pem", |ca: &Ca, args: &Args| Issued {
        cert_pem: "not a certificate".into(),
        ..proper(ca, args)
    });
    assert_eq!(refusal(&refused), Refusal::NotPem);
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the certificate is not one PEM certificate"
    );
    let refused = first_refusal("bundle-not-pem", |ca: &Ca, args: &Args| Issued {
        ca_bundle_pem: "not a bundle".into(),
        ..proper(ca, args)
    });
    assert_eq!(refusal(&refused), Refusal::BundleNotPem);
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the CA bundle is not PEM certificates"
    );
}

/// Catches a bundle trusted without the pinned CA in it, and a certificate trusted because the
/// bundle holds the pinned CA although another CA signed it.
#[test]
fn a_certificate_not_signed_by_the_pinned_ca_is_refused() {
    let refused = first_refusal("no-pinned-ca", |ca: &Ca, args: &Args| Issued {
        ca_bundle_pem: Ca::new(0x72).pem,
        ..proper(ca, args)
    });
    assert_eq!(refusal(&refused), Refusal::NoPinnedCa);
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the CA bundle does not hold the CA whose SHA-256 is the pinned ca_sha256"
    );

    let refused = first_refusal("other-ca", |ca: &Ca, args: &Args| {
        let other = Ca::new(0x73);
        Issued {
            ca_bundle_pem: format!("{}{}", other.pem, ca.pem),
            ..proper(&other, args)
        }
    });
    assert_eq!(refusal(&refused), Refusal::NotSignedByCa);
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the certificate is not signed by the pinned CA"
    );
}

/// Catches a signature that is not an ECDSA signature being read as a good one, or the check
/// failing open when it cannot be parsed.
#[test]
fn a_certificate_whose_signature_is_not_ecdsa_is_refused() {
    let refused = first_refusal("bad-signature", |ca: &Ca, args: &Args| {
        let issued = proper(ca, args);
        let cert = Certificate::from_pem(issued.cert_pem.as_bytes()).expect("PEM");
        let mut der = cert.to_der().expect("DER");
        let signature = cert.signature().raw_bytes().to_vec();
        let at = der
            .windows(signature.len())
            .rposition(|window| window == signature.as_slice())
            .expect("the signature is in the DER");
        // The DER SEQUENCE tag of the signature becomes a SET: not an ECDSA signature.
        der[at] = 0x31;
        Issued {
            cert_pem: pem(&der),
            ..issued
        }
    });
    assert_eq!(refusal(&refused), Refusal::NotSignedByCa);
}

/// Catches a subject other than exactly `host:<id>` being accepted: another host's name, an
/// extra attribute, no common name.
#[test]
fn a_certificate_for_another_subject_is_refused() {
    for (n, subject) in [
        "CN=host:web-2",
        "CN=host:web-1,O=extra",
        "O=host:web-1",
        "CN=web-1",
    ]
    .into_iter()
    .enumerate()
    {
        let subject = subject.to_owned();
        let refused = first_refusal(&format!("subject-{n}"), move |ca: &Ca, args: &Args| {
            let usage = vec![eku(&[ID_KP_CLIENT_AUTH])];
            leaf(ca, args, &subject, usage, (args.now, args.now + WEEK))
        });
        assert_eq!(
            refusal(&refused),
            Refusal::Subject("host:web-1".into()),
            "subject {n}"
        );
        assert_eq!(
            refused.to_string(),
            "the issued certificate was refused: the certificate's subject is not exactly host:web-1"
        );
    }
}

/// Catches a certificate without client authentication being stored: the heartbeat could not
/// use it.
#[test]
fn a_certificate_without_client_authentication_is_refused() {
    let refused = first_refusal("server-auth", |ca: &Ca, args: &Args| {
        let usage = vec![eku(&[ID_KP_SERVER_AUTH])];
        leaf(
            ca,
            args,
            "CN=host:web-1",
            usage,
            (args.now, args.now + WEEK),
        )
    });
    assert_eq!(refusal(&refused), Refusal::NoClientAuth);
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the certificate does not allow client authentication"
    );
    let refused = first_refusal("no-eku", |ca: &Ca, args: &Args| {
        leaf(
            ca,
            args,
            "CN=host:web-1",
            Vec::new(),
            (args.now, args.now + WEEK),
        )
    });
    assert_eq!(refusal(&refused), Refusal::NoClientAuth);
}

/// Catches a certificate not yet valid, or already expired, being stored as current.
#[test]
fn a_certificate_not_valid_now_is_refused() {
    let refused = first_refusal("not-yet", |ca: &Ca, args: &Args| {
        let usage = vec![eku(&[ID_KP_CLIENT_AUTH])];
        leaf(
            ca,
            args,
            "CN=host:web-1",
            usage,
            (args.now + 1, args.now + WEEK),
        )
    });
    assert_eq!(
        refusal(&refused),
        Refusal::NotValidNow {
            not_before: T0 + 1,
            not_after: T0 + WEEK
        }
    );
    let refused = first_refusal("expired", |ca: &Ca, args: &Args| {
        let usage = vec![eku(&[ID_KP_CLIENT_AUTH])];
        leaf(
            ca,
            args,
            "CN=host:web-1",
            usage,
            (args.now - WEEK, args.now - 1),
        )
    });
    assert_eq!(
        refused.to_string(),
        format!(
            "the issued certificate was refused: the certificate is not valid now: it is valid \
             from {} through {}",
            T0 - WEEK,
            T0 - 1
        )
    );
}

/// Catches a renewal or a return that hands the host another host's id.
#[test]
fn a_renewal_for_another_host_is_refused() {
    let world = World::new("other-host");
    let mut enroller = world.enrolled();
    world.coordinator().issue = Some(Box::new(|ca: &Ca, args: &Args| Issued {
        host_id: "web-9".into(),
        ..proper(ca, args)
    }));
    world.set_now(T0 + WEEK / 2);
    let refused = enroller.run_once().expect_err("another host");
    assert_eq!(
        refusal(&refused),
        Refusal::OtherHost {
            held: HostId::new("web-1").expect("id"),
            issued: HostId::new("web-9").expect("id"),
        }
    );
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the certificate is for host web-9, not for this \
         host, web-1"
    );
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
}

/// Catches a refused or failed renewal changing the current generation, or being retried as
/// something else than the same renewal.
#[test]
fn a_refused_renewal_leaves_the_current_generation() {
    let world = World::new("renew-refused");
    let mut enroller = world.enrolled();
    world.coordinator().renew_refusal = Some(Reason::new("host_excluded"));
    world.set_now(T0 + WEEK / 2);
    let refused = enroller.run_once().expect_err("refused");
    assert!(matches!(&refused, Error::RenewRefused(r) if r.as_str() == "host_excluded"));
    assert_eq!(refused.to_string(), "renewal refused: host_excluded");
    world.coordinator().down = Some("connection refused".into());
    let failed = enroller.run_once().expect_err("down");
    assert_eq!(
        failed.to_string(),
        "the coordinator could not be reached: connection refused"
    );
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
    world.coordinator().down = None;
    world.coordinator().renew_refusal = None;
    assert!(matches!(enroller.run_once(), Ok(Outcome::Renewed(_))));
    assert_eq!(world.coordinator().renews.len(), 2);
}

/// Catches an answer for another request being taken for this one.
#[test]
fn an_answer_for_another_request_is_refused() {
    let world = World::new("other-request");
    let other = RequestId::of_spki(b"another key");
    world.coordinator().answer_id = Some(other.clone());
    let mut enroller = world.open();
    let refused = enroller.run_once().expect_err("another request");
    let Error::Protocol(ProtocolError::OtherRequest { sent, answered }) = &refused else {
        panic!("{refused:?}");
    };
    assert_eq!(answered, &other);
    assert_eq!(
        refused.to_string(),
        format!(
            "the coordinator broke the enrollment protocol: it answered for request {other}, \
             not {sent}"
        )
    );
    assert!(world.coordinator().polls.is_empty());
}

/// Catches a denial answered to `Enroll` being polled as if pending.
#[test]
fn a_request_denied_at_enroll_is_not_polled() {
    let world = World::new("denied-at-enroll");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().deny("duplicate_claim");
    drop(enroller);
    let mut enroller = world.open();
    let refused = enroller.run_once().expect_err("denied");
    assert!(matches!(&refused, Error::Denied(r) if r.as_str() == "duplicate_claim"));
    assert_eq!(
        world.coordinator().polls.len(),
        2,
        "only the first enroller polled"
    );
}

/// Catches an unsigned poll that gets no nonce, a nonce outside 16 to 64 bytes, and a signed
/// poll answered with a nonce again, each taken as an answer.
#[test]
fn a_poll_without_a_proper_nonce_is_refused() {
    let world = World::new("no-challenge");
    world.coordinator().unsigned_answer = Some(PollAnswer::Pending);
    let refused = world.open().run_once().expect_err("no nonce");
    assert!(matches!(
        refused,
        Error::Protocol(ProtocolError::NoChallenge)
    ));
    assert_eq!(
        refused.to_string(),
        "the coordinator broke the enrollment protocol: it answered an unsigned poll without a \
         nonce"
    );

    for (len, accepted) in [
        (NONCE_MIN - 1, false),
        (NONCE_MIN, true),
        (NONCE_MAX, true),
        (NONCE_MAX + 1, false),
    ] {
        let world = World::new(&format!("nonce-{len}"));
        world.coordinator().nonce_len = len;
        let result = world.open().run_once();
        if accepted {
            assert_eq!(result.expect("an accepted nonce"), Outcome::Pending);
        } else {
            let refused = result.expect_err("a bad nonce");
            assert_eq!(
                refused.to_string(),
                format!(
                    "the coordinator broke the enrollment protocol: its nonce has {len} bytes, \
                     not 16 to 64"
                )
            );
        }
    }

    let world = World::new("challenge-again");
    world.coordinator().signed_answer = Some(PollAnswer::Challenge(vec![1; 32]));
    let refused = world.open().run_once().expect_err("a nonce again");
    assert!(matches!(
        refused,
        Error::Protocol(ProtocolError::ChallengeAgain)
    ));
    assert_eq!(
        refused.to_string(),
        "the coordinator broke the enrollment protocol: it answered a signed poll with another \
         nonce"
    );
}
