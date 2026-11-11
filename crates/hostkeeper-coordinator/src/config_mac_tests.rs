//! The start's check of the Mac serial allowlist against the coordinator ([`check_allowlist`]),
//! and the fixed host role in the certificates the coordinator's enrollment issues.

use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_proto::hostkeeper::agent::v1::RenewRequest;
use x509_parser::extensions::GeneralName;

use super::check_allowlist;
use crate::config::StartError;
use crate::enrollment::tests::support::{HostKey, NOW, Rig, coordinator_over, enrolled};
use crate::enrollment::{MacAllowlist, os_random};
use crate::host_groups_support::{SELF, World};

/// The URI prefix the host certificates of the tests are issued under.
const PREFIX: &str = "urn:kbf:host-role:";

/// The allowlist the entries `rows` say, each a `[[mac]]` table body.
fn allowlist(rows: &[&str]) -> MacAllowlist {
    let text: String = rows
        .iter()
        .map(|row| format!("[[mac]]\nhost_id_prefix = \"mac-\"\n{row}\n"))
        .collect();
    MacAllowlist::parse(&text).expect("an allowlist")
}

/// A world whose coordinator holds no host group.
fn ungrouped(name: &str) -> World {
    World::new(name)
}

/// A world whose coordinator holds the group `build`.
fn grouped(name: &str) -> World {
    let world = World::new(name);
    world.put("build", &[SELF], None).expect("put");
    world
}

/// Proves: a coordinator that holds no group refuses an entry that names services, as
/// `MacServicesRetired` naming its serial, and an entry whose group it does not hold, as
/// `MacHostGroup` naming the group, with the exact texts.
///
/// Catches: a start that accepts a Mac entry no group can place because the coordinator has not
/// been given its groups yet.
#[test]
fn without_groups_services_and_unheld_groups_are_refused() {
    let world = ungrouped("mac-check-no-groups");
    let by_services = allowlist(&["serial = \"S1\"\nrole = \"build\"\nservices = [\"web\"]"]);
    let by_group = allowlist(&["serial = \"S2\"\nhost_group = \"build\""]);

    let services = check_allowlist(&by_services, world.coordinator()).expect_err("refused");
    let group = check_allowlist(&by_group, world.coordinator()).expect_err("refused");

    assert!(matches!(&services, StartError::MacServicesRetired(serial) if serial == "S1"));
    assert_eq!(
        services.to_string(),
        "--mac-serial-allowlist: the entry of serial S1 names services; the coordinator places \
         hosts by host group, and the Mac's group decides what it runs: name its host_group alone"
    );
    assert!(matches!(&group, StartError::MacHostGroup(name) if name == "build"));
    assert_eq!(
        group.to_string(),
        "--mac-serial-allowlist names host group build, which the coordinator does not hold"
    );
}

/// Proves: a coordinator that holds no group accepts the empty allowlist.
///
/// Catches: a start that refuses a coordinator with no Mac entries.
#[test]
fn without_groups_the_empty_allowlist_passes() {
    let world = ungrouped("mac-check-empty-ok");

    assert!(check_allowlist(&MacAllowlist::default(), world.coordinator()).is_ok());
}

/// Proves: a coordinator that holds a group refuses an entry that names services, as
/// `MacServicesRetired` naming the serial of the first such entry, even
/// when the entry's group is held.
///
/// Catches: a start that lets a Mac's services override its group's.
#[test]
fn with_groups_an_entry_naming_services_is_refused() {
    let world = grouped("mac-check-services-retired");
    let list = allowlist(&[
        "serial = \"S1\"\nhost_group = \"build\"",
        "serial = \"S2\"\nhost_group = \"build\"\nservices = [\"web\"]",
        "serial = \"S3\"\nhost_group = \"build\"\nservices = [\"db\"]",
    ]);

    let refused = check_allowlist(&list, world.coordinator()).expect_err("refused");

    assert!(matches!(&refused, StartError::MacServicesRetired(serial) if serial == "S2"));
    assert_eq!(
        refused.to_string(),
        "--mac-serial-allowlist: the entry of serial S2 names services; the coordinator places \
         hosts by host group, and the Mac's group decides what it runs: name its host_group alone"
    );
}

/// Proves: a coordinator that holds a group refuses an entry whose group it does not hold, as
/// `MacHostGroup` naming the group, with the exact text; a role-only entry names its group by
/// its role.
///
/// Catches: a start that accepts a Mac that no approval can place.
#[test]
fn with_groups_an_unheld_group_is_refused() {
    let world = grouped("mac-check-unheld");
    let by_group = allowlist(&[
        "serial = \"S1\"\nhost_group = \"build\"",
        "serial = \"S2\"\nhost_group = \"lab\"",
    ]);
    let by_role = allowlist(&["serial = \"S3\"\nrole = \"lab\""]);

    for list in [by_group, by_role] {
        let refused = check_allowlist(&list, world.coordinator()).expect_err("refused");

        assert!(matches!(&refused, StartError::MacHostGroup(group) if group == "lab"));
        assert_eq!(
            refused.to_string(),
            "--mac-serial-allowlist names host group lab, which the coordinator does not hold"
        );
    }
}

/// Proves: a coordinator that holds a group accepts an allowlist whose every entry names a
/// held group (by `host_group` or by `role`) and no services, and the empty allowlist.
///
/// Catches: a start that refuses a well-formed group allowlist.
#[test]
fn with_groups_held_groups_and_the_empty_allowlist_pass() {
    let world = grouped("mac-check-groups-ok");
    let list = allowlist(&[
        "serial = \"S1\"\nhost_group = \"build\"",
        "serial = \"S2\"\nrole = \"build\"",
    ]);

    assert!(check_allowlist(&list, world.coordinator()).is_ok());
    assert!(check_allowlist(&MacAllowlist::default(), world.coordinator()).is_ok());
}

/// Every URI of the subject alternative names of the certificate `der`, and how many names it
/// has in all.
fn uris_of(der: &[u8]) -> (Vec<String>, usize) {
    let (_, certificate) = x509_parser::parse_x509_certificate(der).expect("a certificate");
    let san = certificate
        .subject_alternative_name()
        .expect("one extension")
        .expect("present");
    let names = &san.value.general_names;
    let uris = names
        .iter()
        .filter_map(|name| match name {
            GeneralName::URI(uri) => Some((*uri).to_owned()),
            _ => None,
        })
        .collect();
    (uris, names.len())
}

/// An enrollment issuing host certificates under [`PREFIX`].
fn kbf_rig(name: &str) -> Rig {
    Rig::configured(name, Arc::new(RealFs), os_random(), |enrollment| {
        enrollment.with_host_role_uri_prefix(PREFIX)
    })
}

/// Proves: the host certificate issued at `Poll` carries exactly one subject alternative
/// name, the URI `<prefix>host`, whatever role the host was approved in.
///
/// Catches: a certificate that carries the host's role, so that a move between groups would
/// need a new certificate.
#[test]
fn a_host_certificate_carries_the_fixed_host_role() {
    let rig = kbf_rig("mac-cert-fixed-role");
    let key = HostKey::new();

    let certificate = enrolled(&rig.enrollment, &key, "machine-1", "s1");

    assert_eq!(
        uris_of(&certificate),
        (vec!["urn:kbf:host-role:host".to_owned()], 1)
    );
}

/// Proves: a coordinator over the enrollment identifies the issued certificate with the role
/// the enrollment store holds for the host (the approval's `storage`), never the `host` of
/// the certificate's URI, and the host id of the certificate's subject.
///
/// Catches: an identity that reads its role from the certificate, naming every host `host`.
#[tokio::test]
async fn the_coordinator_identifies_the_certificate_by_the_stored_role() {
    let rig = kbf_rig("mac-cert-identity");
    let key = HostKey::new();
    let certificate = enrolled(&rig.enrollment, &key, "machine-1", "s1");
    let coordinator = coordinator_over("mac-cert-identity-coordinator", &rig).await;

    let who = coordinator
        .coordinator
        .identify(&certificate)
        .expect("an enrolled host");

    assert_eq!(who.role.as_str(), "storage");
    assert_eq!(who.host_id.as_str(), "s1");
}

/// Proves: the certificate of a renewal carries the same single URI `<prefix>host` as the
/// first, and the coordinator identifies it as the same host and role.
///
/// Catches: a renewal that is issued without the host role URI, so that the host is no longer
/// identified after its first renewal.
#[tokio::test]
async fn a_renewed_certificate_carries_the_same_host_role() {
    let rig = kbf_rig("mac-cert-renewed");
    let key = HostKey::new();
    let current = enrolled(&rig.enrollment, &key, "machine-1", "s1");
    rig.clock.set(NOW + 3_600);

    let renewed = rig
        .enrollment
        .renew(
            Some(current.as_slice()),
            RenewRequest { csr_der: key.csr() },
        )
        .expect("renewed");

    assert_ne!(renewed.certificate_der, current);
    assert_eq!(
        uris_of(&renewed.certificate_der),
        (vec!["urn:kbf:host-role:host".to_owned()], 1)
    );
    let coordinator = coordinator_over("mac-cert-renewed-coordinator", &rig).await;
    let who = coordinator
        .coordinator
        .identify(&renewed.certificate_der)
        .expect("an enrolled host");
    assert_eq!((who.host_id.as_str(), who.role.as_str()), ("s1", "storage"));
}

/// Proves: the host-role prefix the CA writes when it is given none is the coordinator's default
/// `--host-role-uri-prefix`, the one the certificate readers of `hostkeeper-wire` default to.
///
/// Catches: the two defaults drifting apart, so that a host certificate issued under the default
/// names its role under another prefix than the readers look for.
#[test]
fn the_cas_default_host_role_prefix_is_the_readers() {
    assert_eq!(
        hostkeeper_enrollment::ca::DEFAULT_HOST_ROLE_URI_PREFIX,
        hostkeeper_wire::identity::DEFAULT_HOST_ROLE_URI_PREFIX
    );
}
