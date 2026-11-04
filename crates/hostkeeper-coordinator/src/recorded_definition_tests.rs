//! kbf's definition is plain data: the recorded rollout-file entry of kbf is read like any
//! other service's, and no definition is built in.

use crate::desired::Desired;
use crate::services::{ServiceDef, Services};
use crate::testutil::{LEGACY, kbf_def, kbf_services};

/// Pins: the recorded kbf entry is its four roles and every setting at its default (priority 0,
/// no selector, no bake, no canary, manual rollback). The literal is a copy of those settings,
/// so a change to the recorded data shows here.
///
/// Catches: a role or a setting added to the recorded entry without the tests that rest on it
/// being read again.
#[test]
fn the_recorded_kbf_definition_is_its_four_roles() {
    assert_eq!(
        kbf_def(),
        ServiceDef::new("kbf", &["frontend", "storage", "daemon", "mac-daemon"])
    );
    assert_eq!(kbf_services().get(LEGACY), Some(&kbf_def()));
}

/// Behaviour: no definition is built in. Catches a kbf (or any service) that exists without
/// the rollout file naming it: the default has none, an empty file has none, and a file that
/// defines another service has that one alone.
#[test]
fn no_definition_is_built_in() {
    assert!(Services::default().get("kbf").is_none());
    assert!(Services::parse("").expect("empty").get("kbf").is_none());
    let other =
        Services::parse("[[service]]\nname = \"agent\"\nroles = [\"daemon\"]\n").expect("parsed");
    assert_eq!(other.names().collect::<Vec<_>>(), ["agent"]);
}

/// Behaviour: kbf's roles come from the file: an entry named `kbf` with one other role has that
/// role alone, and a desired state with no `min_up` is accepted for it as for the recorded
/// entry: nothing of kbf's definition is in the code.
///
/// Catches: a rule that is read from the name `kbf` and not from the data.
#[test]
fn kbf_is_the_files_and_not_the_name() {
    let services =
        Services::parse("[[service]]\nname = \"kbf\"\nroles = [\"cook\"]\n").expect("parsed");
    assert_eq!(services.get("kbf").expect("kbf").role_names(), ["cook"]);
    let body = br#"{"version":"2.0"}"#;
    assert!(Desired::parse_request_for(body, &services, Some("kbf")).is_ok());
    assert!(Desired::parse_request_for(body, &kbf_services(), Some("kbf")).is_ok());
}

/// Behaviour: a rollout file defines kbf like any other service: not necessarily first, with
/// its own priority and roles. Catches a kbf that is special in the file: refused when not
/// first, or given roles its entry does not list.
#[test]
fn a_rollout_file_defines_kbf_like_any_service() {
    let text = "[[service]]\nname = \"agent\"\nroles = [\"daemon\"]\n\
                [[service]]\nname = \"kbf\"\npriority = 3\nroles = [\"storage\", \"frontend\"]\n";
    let services = Services::parse(text).expect("parsed");
    let kbf = services.get("kbf").expect("kbf");
    assert_eq!(kbf.priority, 3);
    assert_eq!(kbf.role_names(), ["frontend", "storage"]);
    assert_eq!(services.names().collect::<Vec<_>>(), ["agent", "kbf"]);

    let parse = |body: &[u8]| Desired::parse_request_for(body, &services, Some("kbf")).map(|_| ());
    assert_eq!(parse(br#"{"version":"2.0"}"#), Ok(()));
    assert_eq!(
        parse(br#"{"version":"2.0","min_up":{"frontend":0}}"#),
        Ok(())
    );
}
