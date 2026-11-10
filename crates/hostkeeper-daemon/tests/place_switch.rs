//! What the daemon's switch of a hooked order (`place::hooked_switch`) refuses before it asks the
//! installer for anything: a staged tree it cannot read, one this daemon did not stage, and a
//! previous release it cannot activate again.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::path::Path;

use hostkeeper_daemon::place::hooked_switch;
use hostkeeper_hook::Params;
use hostkeeper_steps::{Bounds, Staged, Switch, SwitchError, Toward, UpdateOrder};
use support_host::NativeHost;

const TARGET_FILE: &str = "target.json";

/// A sealed tree in `dir` that holds `target_json` as its `target.json`, when given.
fn sealed(dir: &Path, target_json: Option<&str>) -> Staged {
    fs::create_dir_all(dir).expect("a directory");
    fs::write(dir.join("hook"), "#!hook\n").expect("a file");
    if let Some(text) = target_json {
        fs::write(dir.join(TARGET_FILE), text).expect("a target.json");
    }
    Staged::seal(dir).expect("a sealed tree")
}

fn order(service: &str, staged: Staged) -> UpdateOrder {
    UpdateOrder {
        service: service.to_owned(),
        target_version: "2.0.0".to_owned(),
        previous_version: None,
        artifact_digest: "ab".repeat(32),
        staged,
        previous: None,
        hook_binary: "hook".into(),
        hook_args: Vec::new(),
        previous_hook: None,
        bounds: Bounds {
            prepare_secs: 1,
            cutover_secs: 1,
            verify_secs: 1,
            rollback_secs: 1,
            cleanup_secs: 1,
            verify_deadline_secs: 1,
            promotion_ceiling_secs: 1,
            cleanup_retries: 0,
        },
        declared_params: Params::default(),
        min_updater_protocol: None,
        order_try: None,
        builtin_steps: false,
    }
}

fn switch_of(host: &NativeHost) -> Switch {
    let daemon = host.daemon();
    let ctx = daemon.ctx();
    hooked_switch(
        ctx.placement.roots.clone(),
        ctx.placement.fs.clone(),
        ctx.units.clone(),
        ctx.repository(),
    )
}

fn failed(got: Result<(), SwitchError>) -> String {
    match got {
        Err(SwitchError::Failed(why)) => why,
        other => panic!("a failed switch, got {other:?}"),
    }
}

/// A version directory with no `target.json` is none this daemon staged: the switch fails naming
/// the directory, for an ordinary service and for hostkeeper itself with no installer recorded.
#[test]
fn a_staged_tree_with_no_target_json_is_not_switched() {
    let host = NativeHost::new("place-switch-none");
    let switch = switch_of(&host);
    for service in ["web", "hostkeeper"] {
        let dir = host.scratch.path().join(format!("none-{service}"));
        let order = order(service, sealed(&dir, None));
        assert_eq!(
            failed(switch(Toward::Target, &order)),
            format!(
                "{} holds no target.json: it was not staged by an installer of this daemon",
                dir.display()
            )
        );
    }
}

/// A `target.json` that does not parse fails the switch before any installer runs.
#[test]
fn a_staged_tree_with_a_damaged_target_json_is_not_switched() {
    let host = NativeHost::new("place-switch-damaged");
    let switch = switch_of(&host);
    let dir = host.scratch.path().join("damaged");
    let order = order("web", sealed(&dir, Some("{")));
    assert!(!failed(switch(Toward::Target, &order)).is_empty());
    assert!(host.tools().is_empty(), "no installer ran");
}

/// The order to switch back from the staged `web` 2.0.0, whose previous release is the sealed
/// tree in `previous` (version `previous_version`).
fn rollback(host: &NativeHost, previous: Staged, previous_version: Option<&str>) -> UpdateOrder {
    host.stage_native(
        "web",
        "2.0.0",
        &support_host::placed_manifest("web", "2.0.0", None),
    );
    let staged = Staged::seal(&host.version_dir("web", "2.0.0")).expect("a sealed tree");
    UpdateOrder {
        previous: Some(previous),
        previous_version: previous_version.map(str::to_owned),
        ..order("web", staged)
    }
}

/// A rollback to a previous version whose directory holds no `target.json` fails naming that
/// directory: it cannot be activated again.
#[test]
fn a_rollback_to_a_version_with_no_target_json_fails() {
    let host = NativeHost::new("place-switch-previous");
    let dir = host.scratch.path().join("previous");
    let order = rollback(&host, sealed(&dir, None), Some("1.0.0"));
    assert_eq!(
        failed(switch_of(&host)(Toward::Back, &order)),
        format!(
            "{} holds no target.json: it cannot be activated again",
            dir.display()
        )
    );
}

/// An order that names the directory of the release it replaces but not its version cannot be
/// switched back.
#[test]
fn a_rollback_whose_order_names_half_of_the_previous_release_fails() {
    let host = NativeHost::new("place-switch-half");
    let dir = host.scratch.path().join("half");
    let order = rollback(&host, sealed(&dir, None), None);
    assert_eq!(
        failed(switch_of(&host)(Toward::Back, &order)),
        "the order for web names only half of the release it replaces"
    );
}

/// An update of hostkeeper itself reads `placed.json` to learn whether its switch is left to the
/// new binary: when the record cannot be read the switch fails, and no installer runs.
#[test]
fn an_update_of_hostkeeper_with_an_unreadable_placed_json_is_not_switched() {
    let host = NativeHost::new("place-switch-record");
    fs::write(host.placed_record_path(), "{").expect("a damaged placed.json");
    let dir = host.scratch.path().join("record");
    let order = order("hostkeeper", sealed(&dir, None));
    let why = failed(switch_of(&host)(Toward::Target, &order));
    assert!(why.contains("placed.json"), "{why}");
    assert!(host.tools().is_empty(), "no installer ran");
}
