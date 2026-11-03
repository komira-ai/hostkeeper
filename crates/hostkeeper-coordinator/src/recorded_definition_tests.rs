//! kbf's definition is plain data: the recorded services-file entry of kbf is read like any
//! other service's, and no definition is built in.

use crate::desired::{Desired, DesiredError};
use crate::reconcile::{FailThreshold, Rollback};
use crate::services::{RoleDef, ServiceDef, Services};
use crate::testutil::{LEGACY, kbf_def, kbf_services};

const KBF_ENTRY: &str = include_str!("../tests/fixtures/recorded/kbf.services.toml");

fn role(
    max_updating: Option<u32>,
    min_up: Option<u32>,
    never_last: bool,
    after: &[&str],
) -> RoleDef {
    RoleDef {
        max_updating,
        min_up,
        never_last,
        after: after.iter().map(|r| (*r).to_owned()).collect(),
    }
}

/// Pins: the recorded kbf entry is exactly the rules kbf ran with when its definition was
/// built into the code: four roles, no limits of their own, only the frontend never the last
/// one up and waiting for storage, daemon and Mac daemon, no `after`, priority 0, and a
/// desired state that must name `min_up` for storage and frontend. The literal is a copy of
/// those rules, so a change to the recorded data shows here.
#[test]
fn the_recorded_kbf_definition_equals_the_rules_kbf_had_built_in() {
    let frozen = ServiceDef {
        name: "kbf".to_owned(),
        roles: [
            (
                "frontend",
                role(None, None, true, &["storage", "daemon", "mac-daemon"]),
            ),
            ("storage", role(None, None, false, &[])),
            ("daemon", role(None, None, false, &[])),
            ("mac-daemon", role(None, None, false, &[])),
        ]
        .into_iter()
        .map(|(name, def)| (name.to_owned(), def))
        .collect(),
        after: Vec::new(),
        require_min_up: vec!["storage".to_owned(), "frontend".to_owned()],
        priority: 0,
        selector: None,
        install_when_absent: false,
        bake_beats: 0,
        canary: None,
        rollback: Rollback::Manual,
        rollback_after_failed: FailThreshold::default(),
        min_version: None,
        defer_backoff: crate::defer::DeferBackoff::default(),
    };
    assert_eq!(kbf_def(), frozen);
    assert_eq!(kbf_services().get(LEGACY), Some(&frozen));
}

/// Behaviour: no definition is built in. Catches a kbf (or any service) that exists without
/// the services file naming it: the default has none, an empty file has none, and a file that
/// defines another service has that one alone.
#[test]
fn no_definition_is_built_in() {
    assert!(Services::default().get("kbf").is_none());
    assert!(Services::parse("").expect("empty").get("kbf").is_none());
    let other =
        Services::parse("[[service]]\nname = \"agent\"\n[service.roles.daemon]\n").expect("parsed");
    assert_eq!(other.names().collect::<Vec<_>>(), ["agent"]);
}

/// Behaviour: kbf's roles, `never_last`, `after` and required minimums come from the file.
/// The same entry with the `require_min_up` line removed requires no minimum, and one that
/// names another service in `after` waits for it: nothing of kbf's rules is in the code.
/// Catches a rule that is read from the name `kbf` and not from the data.
#[test]
fn kbf_rules_are_the_files_and_not_the_name() {
    let without: String = KBF_ENTRY
        .lines()
        .filter(|line| !line.starts_with("require_min_up"))
        .map(|line| format!("{line}\n"))
        .collect();
    let services = Services::parse(&without).expect("parsed");
    let kbf = services.get("kbf").expect("kbf");
    assert!(kbf.require_min_up.is_empty());
    assert_eq!(kbf.roles["frontend"], kbf_def().roles["frontend"]);
    let body = br#"{"version":"2.0"}"#;
    assert!(Desired::parse_request_for(body, &services, Some("kbf")).is_ok());
    assert!(matches!(
        Desired::parse_request_for(body, &kbf_services(), Some("kbf")),
        Err(DesiredError::Malformed(_))
    ));
}

/// Behaviour: a services file defines kbf like any other service: not necessarily first, with
/// its own priority and roles, waited for by `after = ["kbf"]` only when defined earlier, and
/// with the minimums it lists in `require_min_up`. Catches a kbf that is special in the file:
/// refused when not first, or given minimums its entry does not list.
#[test]
fn a_services_file_defines_kbf_like_any_service() {
    let text = "[[service]]\nname = \"agent\"\n[service.roles.daemon]\n\
                [[service]]\nname = \"kbf\"\npriority = 3\nafter = [\"agent\"]\n\
                require_min_up = [\"storage\"]\n\
                [service.roles.storage]\nmax_updating = 3\nmin_up = 2\n\
                [service.roles.frontend]\nnever_last = true\nafter = [\"storage\"]\n";
    let services = Services::parse(text).expect("parsed");
    let kbf = services.get("kbf").expect("kbf");
    assert_eq!(kbf.priority, 3);
    assert_eq!(kbf.after, ["agent"]);
    assert_eq!(kbf.role_names(), ["frontend", "storage"]);
    assert_eq!(kbf.roles["storage"], role(Some(3), Some(2), false, &[]));
    assert_eq!(kbf.roles["frontend"], role(None, None, true, &["storage"]));
    assert_eq!(kbf.require_min_up, ["storage"]);
    assert_eq!(services.names().collect::<Vec<_>>(), ["agent", "kbf"]);

    let parse = |body: &[u8]| Desired::parse_request_for(body, &services, Some("kbf")).map(|_| ());
    assert!(matches!(
        parse(br#"{"version":"2.0"}"#),
        Err(DesiredError::Malformed(_))
    ));
    assert_eq!(
        parse(br#"{"version":"2.0","min_up":{"frontend":0}}"#),
        Err(DesiredError::Malformed("min_up lacks storage".to_owned()))
    );
    assert_eq!(
        parse(br#"{"version":"2.0","min_up":{"storage":0}}"#),
        Ok(())
    );
}
