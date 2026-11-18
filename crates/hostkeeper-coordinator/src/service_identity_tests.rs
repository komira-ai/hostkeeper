//! `Coordinator.IssueServiceCertificate` over a farm: what a certificate says, who gets one and who
//! does not, in the order of the design note's checks, and what an issue changes. In process, with
//! the enrollment's fixed clock and the in-memory store.

use hostkeeper_enrollment::ca::CaBundle;
use hostkeeper_proto::hostkeeper::agent::v1::{self as agent, CoordinatorRefusal, RenewRequest};
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use x509_parser::parse_x509_certificate;

use crate::enrollment::RpcError;
use crate::enrollment::tests::support::{HostKey, NOW};
use crate::marks_support::{operator as operator_caller, who};
use crate::service_identity_support::{
    Fix, Held, PLAIN, SERVICE, csr_of, identity_targets, request_for,
};
use crate::testutil::id;

/// The refusal `token` of `IssueServiceCertificate`.
fn refused(refusal: CoordinatorRefusal) -> RpcError {
    RpcError::Refused(refusal)
}

/// The next serial the counter of certificates gives: what was issued before took the ones below.
fn next_serial(fix: &Fix) -> u64 {
    fix.enrollment()
        .store()
        .allocate_serial()
        .expect("a serial")
}

/// Retires `held` as an operator.
fn retire(fix: &Fix, held: &Held) {
    fix.coordinator()
        .retire_host(
            &operator_caller(),
            &operator::RetireRequest {
                node_id: held.name.clone(),
                reason: "board failed".to_owned(),
            },
        )
        .expect("retired");
}

/// Proves: a host approved by an operator whose group lists a service whose entry says `identity`
/// is answered a certificate for the key of its CSR that names the service and the host, is signed
/// by the coordinator's CA, is good for client authentication alone (and is no CA's), and is valid
/// for 28800 seconds from the coordinator's clock; the answer carries the CA bundle; one audit
/// entry `service_identity_issued` by the coordinator names the host, the service and the serial,
/// which is the certificate's, and holds nothing else of the request.
///
/// Catches: a certificate that names another host or service, takes anything but its key from the
/// CSR, lives longer or shorter than 8 hours, is good for server authentication, or is issued with no
/// audit entry or one that holds the CSR.
#[tokio::test]
async fn a_certificate_names_the_service_and_the_host_for_client_authentication_for_eight_hours() {
    let fix = Fix::new("identity-issue").await;
    let held = fix.operator_host("node-01");
    let key = HostKey::new();
    let answer = fix
        .ask_with(&held.peer(), &request_for(SERVICE, &key))
        .expect("issued");

    let bundle: &CaBundle = fix.enrollment().ca_bundle();
    let read = bundle
        .service_certificate(&answer.certificate_der)
        .expect("a service certificate of this CA");
    assert_eq!(read.service.as_str(), SERVICE);
    assert_eq!(read.host_id.as_str(), "node-01");
    assert_eq!(read.key_hash, key.key_hash());
    assert_eq!(read.not_after_unix - read.not_before_unix, 28_800);
    assert!((read.not_before_unix..read.not_after_unix).contains(&NOW));
    assert_eq!(
        answer.ca_bundle_pem,
        crate::enrollment::pem("CERTIFICATE", bundle.certificate_der())
    );

    let (_, parsed) = parse_x509_certificate(&answer.certificate_der).expect("DER");
    let usage = parsed
        .extended_key_usage()
        .expect("readable")
        .expect("an extended key usage")
        .value;
    assert!(usage.client_auth);
    assert!(!usage.server_auth && !usage.any && !usage.code_signing && !usage.email_protection);
    assert!(usage.other.is_empty());
    assert!(!parsed.is_ca());
    let serial = parsed.tbs_certificate.serial.to_u64_digits();

    let issued = fix.audited("service_identity_issued");
    assert_eq!(issued.len(), 1);
    assert_eq!(
        (
            issued[0].by.as_str(),
            issued[0].node_id.as_deref(),
            issued[0].service.as_deref(),
            issued[0].reason.as_deref(),
        ),
        (
            "coordinator",
            Some("node-01"),
            Some(SERVICE),
            Some(format!("serial {}", serial[0]).as_str())
        )
    );
}

/// Proves: an entry whose manifest does not say `identity` (absent, or false) is assigned no
/// identity: the host that is listed the service is refused `service_not_assigned`, no serial is
/// taken and nothing is audited.
///
/// Catches: a certificate for any service a group lists, whatever its release says.
#[tokio::test]
async fn an_entry_without_identity_gets_none() {
    let fix = Fix::new("identity-plain").await;
    let held = fix.operator_host("node-01");
    let before = next_serial(&fix);
    assert_eq!(
        fix.ask(&held, PLAIN).map(|_| ()),
        Err(refused(CoordinatorRefusal::ServiceNotAssigned))
    );
    fix.release(PLAIN, "1.1", Some(false)).await;
    fix.farm.tell("node-01", &[]);
    assert_eq!(
        fix.ask(&held, PLAIN).map(|_| ()),
        Err(refused(CoordinatorRefusal::ServiceNotAssigned))
    );
    assert_eq!(next_serial(&fix), before + 1, "no serial was taken");
    assert_eq!(fix.audited("service_identity_issued"), []);

    fix.release(PLAIN, "1.2", Some(true)).await;
    fix.farm.tell("node-01", &[]);
    assert!(
        fix.ask(&held, PLAIN).is_ok(),
        "the entry of the version wanted now says identity"
    );
}

/// Proves: a host the coordinator approved itself, by an inventory record, gets no certificate
/// (`identity_not_approved`) until an operator approves its key with `HostIdentity.Approve`, and
/// then gets one; and a host that was replaced by a new key needs a new approval, the approval of
/// the key it held before counting for nothing.
///
/// Catches: an inventory record that is enough for a service identity, and an approval that
/// outlives the key it was given for.
#[tokio::test]
async fn an_auto_host_gets_none_until_an_operator_approves_its_key_and_a_new_key_needs_a_new_approval()
 {
    let fix = Fix::new("identity-auto").await;
    let held = fix.auto_host("node-01");
    assert_eq!(
        fix.ask(&held, SERVICE).map(|_| ()),
        Err(refused(CoordinatorRefusal::IdentityNotApproved))
    );
    assert_eq!(fix.audited("service_identity_issued"), []);

    let approve = |key: &HostKey| operator::ApproveHostIdentityRequest {
        node_id: "node-01".to_owned(),
        key_sha256: key.key_hash().to_hex(),
        reason: "racked by us".to_owned(),
    };
    fix.coordinator()
        .host_identity_approve(&operator_caller(), &approve(&held.key))
        .expect("approved");
    assert!(fix.ask(&held, SERVICE).is_ok());

    // The inventory record approves a new key for the same host id, taking the id over.
    let renewed = fix.auto_host_replaced("node-01");
    assert_eq!(
        fix.ask(&renewed, SERVICE).map(|_| ()),
        Err(refused(CoordinatorRefusal::IdentityNotApproved))
    );
    fix.coordinator()
        .host_identity_approve(&operator_caller(), &approve(&renewed.key))
        .expect("approved again");
    assert!(fix.ask(&renewed, SERVICE).is_ok());
}

/// Proves: a service no group lists for the host is refused `service_not_assigned`: one a group
/// the host is not in lists, one no group lists, one the host's group lists and an uninstall of
/// is ordered on the host, and a host in no group, whatever any group lists.
///
/// Catches: a certificate for a service because the coordinator knows it, not because it is the
/// host's.
#[tokio::test]
async fn a_service_not_assigned_to_the_host_is_refused() {
    let fix = Fix::new("identity-unassigned").await;
    fix.farm.group("elsewhere", &[SERVICE], None);
    let held = fix.operator_host("node-01");
    let lone = fix.lone_host("node-02");
    let not_assigned = Err(refused(CoordinatorRefusal::ServiceNotAssigned));

    // A group the host is not in lists it.
    assert_eq!(fix.ask(&lone, SERVICE).map(|_| ()), not_assigned);
    // No group lists it.
    assert_eq!(fix.ask(&held, "unlisted").map(|_| ()), not_assigned);
    assert!(fix.ask(&held, SERVICE).is_ok());

    // An uninstall is ordered on the host while its group still lists the service.
    fix.farm
        .world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("test", "seed"),
            |state| {
                state
                    .assignments
                    .entry(SERVICE.to_owned())
                    .or_default()
                    .uninstall
                    .insert("node-01".to_owned(), 1);
            },
        )
        .expect("seeded");
    assert_eq!(fix.ask(&held, SERVICE).map(|_| ()), not_assigned);
}

/// Proves: once the host's group no longer lists the service and the coordinator orders the host to
/// uninstall it, the host is refused `service_not_assigned`, and a host of the group that never ran
/// it is refused too.
///
/// Catches: a service being uninstalled that keeps getting 8-hour certificates.
#[tokio::test]
async fn a_service_the_group_drops_is_refused_once_it_is_being_uninstalled() {
    let fix = Fix::new("identity-uninstall").await;
    let held = fix.operator_host("node-01");
    let idle = fix.operator_host("node-02");
    fix.farm.tell("node-01", &[(SERVICE, "0.1.230")]);
    assert!(fix.ask(&held, SERVICE).is_ok());

    fix.relist(&[PLAIN]);
    assert_eq!(
        fix.farm.world.intents(),
        [(SERVICE.to_owned(), "node-01".to_owned(), 1)]
    );
    let not_assigned = Err(refused(CoordinatorRefusal::ServiceNotAssigned));
    assert_eq!(fix.ask(&held, SERVICE).map(|_| ()), not_assigned);
    assert_eq!(fix.ask(&idle, SERVICE).map(|_| ()), not_assigned);
}

/// Proves: a service name that breaks the grammar is refused `invalid_service`: empty, with a
/// path in it, beginning with `-` or `.`, and 256 bytes long.
///
/// Catches: a name that reaches the subject of a certificate or the audit entry unchecked.
#[tokio::test]
async fn a_malformed_service_name_is_refused() {
    let fix = Fix::new("identity-name").await;
    let held = fix.operator_host("node-01");
    for name in ["", "../etc", "a/b", "-x", ".x", &"x".repeat(256), "a b"] {
        assert_eq!(
            fix.ask(&held, name).map(|_| ()),
            Err(refused(CoordinatorRefusal::InvalidService)),
            "{name:?}"
        );
    }
    assert_eq!(fix.audited("service_identity_issued"), []);
}

/// Proves: a CSR that is not a request, one for a key that is not P-256, and one whose
/// self-signature is wrong are refused `bad_csr` and `key_not_p256`; a CSR with any subject, the
/// service's or `host:pending`, is read for its key alone.
///
/// Catches: a certificate for a key nobody proved they hold, and a subject taken from the CSR.
#[tokio::test]
async fn a_csr_is_checked_for_its_key_and_its_signature_but_not_its_subject() {
    let fix = Fix::new("identity-csr").await;
    let held = fix.operator_host("node-01");
    let ask = |csr: Vec<u8>| {
        fix.ask_with(
            &held.peer(),
            &hostkeeper_proto::hostkeeper::agent::v1::IssueServiceCertificateRequest {
                service: SERVICE.to_owned(),
                csr_der: csr,
            },
        )
        .map(|_| ())
    };
    assert_eq!(
        ask(b"not a request".to_vec()),
        Err(refused(CoordinatorRefusal::BadCsr))
    );
    assert_eq!(ask(Vec::new()), Err(refused(CoordinatorRefusal::BadCsr)));
    let other = HostKey::of(&rcgen::PKCS_ECDSA_P384_SHA384);
    assert_eq!(
        ask(csr_of(&other, "service:js-agent")),
        Err(refused(CoordinatorRefusal::KeyNotP256))
    );
    let mut tampered = csr_of(&HostKey::new(), "service:js-agent");
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert_eq!(ask(tampered), Err(refused(CoordinatorRefusal::BadCsr)));
    for subject in ["service:js-agent", "host:pending", "anything at all"] {
        assert_eq!(ask(csr_of(&HostKey::new(), subject)), Ok(()), "{subject}");
    }
}

/// Proves: the refusal a caller is told is the first check that fails, in the design note's order:
/// no certificate or a certificate that is no host's is `forbidden` before anything, a denied host
/// is `denied` before its service name is looked at, a bad service name is `invalid_service`
/// before its CSR is read, a bad CSR is `bad_csr` before the assignment is looked at, and an
/// assignment is `service_not_assigned` before the host's assurance is.
///
/// Catches: a refusal that tells a caller more than the check it failed first, such as the
/// assurance of a host that is denied.
#[tokio::test]
async fn the_checks_are_made_in_the_order_of_the_design_note() {
    let fix = Fix::new("identity-order").await;
    let auto = fix.auto_host("node-01");
    let bad_csr =
        |service: &str| hostkeeper_proto::hostkeeper::agent::v1::IssueServiceCertificateRequest {
            service: service.to_owned(),
            csr_der: b"no".to_vec(),
        };
    let first = |peer: &Option<std::sync::Arc<[u8]>>, request| {
        fix.ask_with(peer, &request).map(|_| ()).unwrap_err()
    };

    // Not a host certificate: forbidden whatever the request says.
    assert_eq!(
        first(&None, bad_csr("a b")),
        refused(CoordinatorRefusal::Forbidden)
    );
    assert_eq!(
        first(&Some(std::sync::Arc::from(&b"junk"[..])), bad_csr("a b")),
        refused(CoordinatorRefusal::Forbidden)
    );
    // The service name before the CSR, the CSR before the assignment, the assignment before the
    // assurance (the host is auto and unapproved).
    assert_eq!(
        first(&auto.peer(), bad_csr("a b")),
        refused(CoordinatorRefusal::InvalidService)
    );
    assert_eq!(
        first(&auto.peer(), bad_csr("unlisted")),
        refused(CoordinatorRefusal::BadCsr)
    );
    assert_eq!(
        first(&auto.peer(), request_for("unlisted", &HostKey::new())),
        refused(CoordinatorRefusal::ServiceNotAssigned)
    );
    assert_eq!(
        first(&auto.peer(), request_for(SERVICE, &HostKey::new())),
        refused(CoordinatorRefusal::IdentityNotApproved)
    );
    // Denied before the service name.
    retire(&fix, &auto);
    assert_eq!(
        first(&auto.peer(), bad_csr("a b")),
        refused(CoordinatorRefusal::Denied)
    );
}

/// Proves: a host an operator retired is refused `denied` at its next ask, also for a service
/// it was issued before, and is issued nothing more.
///
/// Catches: a retired host that keeps getting service certificates until an operator notices.
#[tokio::test]
async fn a_retired_host_is_refused_denied() {
    let fix = Fix::new("identity-retired").await;
    let held = fix.operator_host("node-01");
    let other = fix.operator_host("node-02");
    assert!(fix.ask(&held, SERVICE).is_ok());
    retire(&fix, &held);
    assert_eq!(
        fix.ask(&held, SERVICE).map(|_| ()),
        Err(refused(CoordinatorRefusal::Denied))
    );
    assert_eq!(fix.audited("service_identity_issued").len(), 1);
    assert!(fix.ask(&other, SERVICE).is_ok(), "another host is served");
}

/// Proves: a service certificate is no host's: presented in place of the client certificate, it is
/// refused `forbidden` by `IssueServiceCertificate` (it cannot be used to ask for more) and by
/// `Renew`, and the heartbeat's caller check does not read it as a host either.
///
/// Catches: a service certificate that authenticates as the host it names, which would let a
/// service act as the daemon.
#[tokio::test]
async fn a_service_certificate_is_refused_where_a_host_certificate_is_needed() {
    let fix = Fix::new("identity-service-cert").await;
    let held = fix.operator_host("node-01");
    let service_cert = fix.ask(&held, SERVICE).expect("issued").certificate_der;
    let peer = Some(std::sync::Arc::<[u8]>::from(service_cert.as_slice()));

    assert_eq!(
        fix.ask_with(&peer, &request_for(SERVICE, &HostKey::new()))
            .map(|_| ()),
        Err(refused(CoordinatorRefusal::Forbidden))
    );
    assert_eq!(
        fix.enrollment()
            .renew(
                Some(&service_cert),
                RenewRequest {
                    csr_der: held.key.csr(),
                }
            )
            .map(|_| ()),
        Err(refused(CoordinatorRefusal::Forbidden))
    );
    assert!(fix.coordinator().identify(&service_cert).is_err());
    assert!(fix.enrollment().identity_of(&service_cert).is_none());
    assert!(fix.coordinator().identify(&held.cert).is_ok());
    let _ = id("node-01");
}

/// Proves: an issue that cannot write its audit entry answers `store_failed` and no certificate,
/// and a later ask, once the store takes writes, is answered.
///
/// Catches: a certificate issued that the audit log does not show.
#[tokio::test]
async fn an_audit_entry_that_cannot_be_written_issues_nothing() {
    let fix = Fix::new("identity-audit-fails").await;
    let held = fix.operator_host("node-01");
    fix.farm
        .world
        .fake
        .refuse(|_| Some(crate::store::audit_failure()));
    assert_eq!(
        fix.ask(&held, SERVICE).map(|_| ()),
        Err(refused(CoordinatorRefusal::StoreFailed))
    );
    assert_eq!(fix.audited("service_identity_issued"), []);
    fix.farm.world.fake.heal();
    assert!(fix.ask(&held, SERVICE).is_ok());
    assert_eq!(fix.audited("service_identity_issued").len(), 1);
}

/// Proves: with no host group held (a coordinator that places by role), a service the rollout file
/// gives the host's role and whose entry says `identity` is assigned, one whose entry does not is
/// not, and an operator's entry that takes the service off the host takes its identity too.
///
/// Catches: a decision made for group mode alone.
#[tokio::test]
async fn a_coordinator_without_groups_decides_by_the_role_of_the_host() {
    let fix = Fix::without_groups("identity-roles");
    for (package, identity) in [("web", Some(true)), ("db", None)] {
        let body = serde_json::json!({"service": package, "version": "2.0"});
        fix.farm
            .set("", &body, Some(identity_targets(package, 1, identity)))
            .await;
    }
    let held = fix.lone_host("node-01");
    fix.farm.tell("node-01", &[]);
    assert!(fix.ask(&held, "web").is_ok());
    assert_eq!(
        fix.ask(&held, "db").map(|_| ()),
        Err(refused(CoordinatorRefusal::ServiceNotAssigned))
    );
    fix.coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("test", "seed"),
            |state| {
                state
                    .assignments
                    .entry("web".to_owned())
                    .or_default()
                    .hosts
                    .insert("node-01".to_owned(), crate::assignment::Entry::Exclude);
            },
        )
        .expect("seeded");
    assert_eq!(
        fix.ask(&held, "web").map(|_| ()),
        Err(refused(CoordinatorRefusal::ServiceNotAssigned))
    );
}

/// What `Hosts.Get` shows of the service identities of `host`.
fn shown(fix: &Fix, host: &str) -> Vec<agent::ServiceIdentityReport> {
    fix.coordinator()
        .get_host(
            &operator_caller(),
            &operator::GetHostRequest {
                node_id: host.to_owned(),
            },
        )
        .expect("the host")
        .host
        .expect("a host")
        .heartbeat
        .map(|beat| beat.service_identities)
        .unwrap_or_default()
}

/// Proves: `service-identity-v1` is acted on: the coordinator advertises it, a daemon that announces
/// it has the list it reports kept and shown in its host's heartbeat, and a daemon that does not
/// has none read, though it carries one.
///
/// Catches: a feature advertised for behaviour that is not there, and a list read from a daemon that
/// did not announce it.
pub(crate) async fn service_identity_v1_is_acted_on() {
    assert!(crate::service::FEATURES.contains(&crate::service_identity::FEATURE));
    let fix = Fix::new("identity-feature").await;
    fix.operator_host("node-01");
    fix.operator_host("node-02");
    let list = vec![agent::ServiceIdentityReport {
        service: SERVICE.to_owned(),
        not_after_unix: Some(NOW + 28_800),
        last_error: None,
    }];
    let heartbeat = |features: &[&str]| agent::HeartbeatRequest {
        features: features.iter().map(|name| (*name).to_owned()).collect(),
        service_identities: list.clone(),
        ..agent::HeartbeatRequest::default()
    };
    let hear = |host: &str, features: &[&str]| {
        fix.coordinator()
            .hear_service_identities(&who(host, "storage"), &heartbeat(features));
    };
    hear("node-01", &[crate::service_identity::FEATURE]);
    hear("node-02", &[]);
    assert_eq!(shown(&fix, "node-01"), list);
    assert_eq!(shown(&fix, "node-02"), []);
}

#[tokio::test]
async fn service_identity_v1_is_advertised_and_read_from_the_daemons_that_announce_it() {
    service_identity_v1_is_acted_on().await;
}

/// Proves: a host the coordinator has not heard since it started is refused `service_not_assigned`
/// (its daemon's installers are not known, so no entry is picked), and is issued a certificate
/// once it has been heard; and a group that lists the service but wants no version of it assigns
/// no identity.
///
/// Catches: a certificate for an entry the coordinator could not order the host, and a decision
/// made for a service with no version.
#[tokio::test]
async fn a_host_not_heard_and_a_service_with_no_version_wanted_are_refused() {
    let fix = Fix::new("identity-unheard").await;
    let quiet = fix.unheard_host("node-01");
    let not_assigned = Err(refused(CoordinatorRefusal::ServiceNotAssigned));
    assert_eq!(fix.ask(&quiet, SERVICE).map(|_| ()), not_assigned);
    fix.farm.tell("node-01", &[]);
    assert!(fix.ask(&quiet, SERVICE).is_ok());

    // A group that lists the service wants no version of it until one is set.
    fix.farm.group("fresh", &["unset"], None);
    let fresh = fix.unheard_host("node-02");
    fix.farm
        .world
        .move_to("node-02", "fresh")
        .expect("moved to the group");
    fix.farm.tell("node-02", &[]);
    assert_eq!(fix.ask(&fresh, "unset").map(|_| ()), not_assigned);
}

/// Proves: a host certificate of a key the host no longer holds (its id was approved again for
/// another key) is refused `forbidden` by `IssueServiceCertificate` and by `ListDeny`, and the
/// new key's is not.
///
/// Catches: a replaced key that goes on asking for service certificates and reading the deny
/// list.
#[tokio::test]
async fn a_certificate_of_a_replaced_key_is_no_host_certificate() {
    let fix = Fix::new("identity-replaced").await;
    let old = fix.auto_host("node-01");
    let new = fix.auto_host_replaced("node-01");
    let forbidden = refused(CoordinatorRefusal::Forbidden);
    assert_eq!(fix.ask(&old, SERVICE).map(|_| ()), Err(forbidden));
    assert_eq!(
        fix.enrollment()
            .list_deny(old.peer().as_deref())
            .map(|_| ()),
        Err(forbidden)
    );
    assert!(fix.enrollment().list_deny(new.peer().as_deref()).is_ok());
}
