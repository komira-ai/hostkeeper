//! The rollout entry of the example service hello (`examples/hello/rollout.toml`), read with the
//! coordinator's own reader: what docs/hello.md tells an operator to append is a file the
//! coordinator accepts, with the settings the farm proof relies on.

use hostkeeper_coordinator::canary::Canary;
use hostkeeper_coordinator::reconcile::{FailThreshold, Rollback};
use hostkeeper_coordinator::services::Services;

const ENTRY: &str = include_str!("../../../examples/hello/rollout.toml");

/// Catches the entry the proof appends to a rollout file being refused by the coordinator, or
/// losing a setting the proof needs: install by role, a lone canary, and an automatic rollback
/// that one failed host is enough for.
#[test]
fn the_hello_entry_loads_with_the_settings_the_farm_proof_needs() {
    let services = Services::parse(ENTRY).unwrap_or_else(|e| panic!("refused: {e}"));
    assert_eq!(services.names().collect::<Vec<_>>(), ["hello"]);
    let hello = services.get("hello").expect("hello is defined");
    assert_eq!(hello.role_names(), ["farm"]);
    assert!(hello.install_when_absent, "a host of the role is ordered to install it");
    assert_eq!(hello.bake_beats, 2);
    assert_eq!(hello.canary, Some(Canary { count: 1 }));
    assert_eq!(hello.rollback, Rollback::Automatic);
    assert_eq!(
        hello.rollback_after_failed,
        FailThreshold {
            percent: 25,
            min_hosts: 1
        }
    );
    assert_eq!(
        hello.rollback_after_failed.hosts(1),
        1,
        "on a one-host farm, one failed host rolls the release back"
    );
    assert!(hello.selector.is_some(), "the hosts of the role belong");
}

/// Catches the entry being read as a second copy of a service a rollout file already defines:
/// appended after another service's entry it adds hello and changes nothing else.
#[test]
fn the_hello_entry_appends_to_a_rollout_file_with_another_service() {
    let other = "[[service]]\nname = \"web\"\nroles = [\"frontend\"]\n";
    let services =
        Services::parse(&format!("{other}\n{ENTRY}")).unwrap_or_else(|e| panic!("refused: {e}"));
    assert_eq!(services.names().collect::<Vec<_>>(), ["hello", "web"]);
    assert_eq!(
        services.get("web").map(|web| web.role_names()),
        Some(vec!["frontend"])
    );
}
