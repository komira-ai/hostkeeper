//! The table of layers itself: a row for every crate of the workspace and for nothing else, no
//! crate depending on a crate of this product that left the workspace, and the launchd crate
//! above the step runner and the observer, in the rows and in the manifests.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::check::{
    LAYERS, Layer, NOT_ABOVE, check, metadata_of, reaches_in, read_lock, rows_cover,
};
use crate::{
    CONFIG_DEP, HOOK_DEP, PLACED_DEP, PROTO_DEP, STEPS_DEP, TESTKIT_DEP, bare_crate, fixture,
    layered, manifest_of, refused, steps_through_shim, vendored_dep,
};

const OBSERVE_DEP: &str = "hostkeeper-observe = { path = \"../hostkeeper-observe\" }\n";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The package list of `cargo metadata` JSON.
fn packages(metadata: &mut Value) -> &mut Vec<Value> {
    metadata["packages"]
        .as_array_mut()
        .expect("metadata lists packages")
}

/// A package of `cargo metadata` JSON named `name` with the dev-dependencies `dev`.
fn package(name: &str, dev: &[&str]) -> Value {
    let dependencies: Vec<Value> = dev
        .iter()
        .map(|dep| json!({ "name": dep, "kind": "dev" }))
        .collect();
    json!({ "name": name, "dependencies": dependencies })
}

/// `LAYERS` with the normal column of the row of `name` replaced by `exact`.
fn with_row(table: &[Layer], name: &str, exact: &'static [&'static str]) -> Vec<Layer> {
    table
        .iter()
        .map(|row| {
            if row.0 == name {
                (row.0, exact, row.2)
            } else {
                *row
            }
        })
        .collect()
}

/// Catches: a crate added to the workspace with no row in `LAYERS` (here a new crate, and the
/// removed rights crate coming back as a member), a row left for a crate that is gone (here
/// the row the rights crate had, and a row whose crate is taken out, one no dev column names), and a dev column that is
/// not what its crate's dev-dependencies are (one more on the crate's side, one fewer on the
/// row's). The real workspace passes, its normal and build column included.
#[test]
fn layering_rules_cover_every_surviving_crate() {
    let root = workspace_root();
    let metadata = metadata_of(&root);
    assert_eq!(rows_cover(&LAYERS, &metadata), Ok(()));
    assert_eq!(check(&root), Ok(()));

    let mut grown = metadata.clone();
    packages(&mut grown).push(package("hostkeeper-new", &[]));
    packages(&mut grown).push(package("hostkeeper-rights", &[]));
    assert_eq!(
        rows_cover(&LAYERS, &grown),
        refused(
            "hostkeeper-new is a crate of the workspace with no row in LAYERS\n\
             hostkeeper-rights is a crate of the workspace with no row in LAYERS"
        )
    );

    let mut stale = LAYERS.to_vec();
    stale.push((
        "hostkeeper-rights",
        &["hostkeeper-config", "hostkeeper-hook"],
        &[],
    ));
    assert_eq!(
        rows_cover(&stale, &metadata),
        refused("LAYERS has a row for hostkeeper-rights, which is not a crate of the workspace")
    );
    let mut shrunk = metadata.clone();
    packages(&mut shrunk).retain(|package| package["name"] != "hostkeeper-package");
    assert_eq!(
        rows_cover(&LAYERS, &shrunk),
        refused("LAYERS has a row for hostkeeper-package, which is not a crate of the workspace")
    );

    let mut more_dev = metadata.clone();
    let wire = packages(&mut more_dev)
        .iter_mut()
        .find(|package| package["name"] == "hostkeeper-wire")
        .expect("the wire crate is a member");
    wire["dependencies"]
        .as_array_mut()
        .expect("a package lists its dependencies")
        .push(json!({ "name": "hostkeeper-oidc", "kind": "dev" }));
    assert_eq!(
        rows_cover(&LAYERS, &more_dev),
        refused(
            "hostkeeper-wire has dev-dependencies on the workspace crates {hostkeeper-oidc}, \
             but its row in LAYERS names {}"
        )
    );
    let oidc_only: &'static [&'static str] = &["hostkeeper-oidc"];
    let fewer_dev: Vec<Layer> = LAYERS
        .iter()
        .map(|row| {
            if row.0 == "hostkeeper-coordinator" {
                (row.0, row.1, oidc_only)
            } else {
                *row
            }
        })
        .collect();
    assert_eq!(
        rows_cover(&fewer_dev, &metadata),
        refused(
            "hostkeeper-coordinator has dev-dependencies on the workspace crates \
             {hostkeeper-ctl, hostkeeper-oidc}, but its row in LAYERS names {hostkeeper-oidc}"
        )
    );
}

/// Catches: a crate depending on the removed rights crate, here kept as a path crate outside
/// the workspace (where neither the layers nor the coverage gate would see it), being let
/// through; the refusal names the chain from the member. The real workspace names the crate
/// nowhere: its lockfile has no package of that name, and it passes.
#[test]
fn no_crate_depends_on_the_removed_rights_crate() {
    let root = workspace_root();
    assert!(!read_lock(&root).contains_key("hostkeeper-rights"));
    assert_eq!(check(&root), Ok(()));

    let mut edits = bare_crate("vendored", "hostkeeper-rights");
    edits.push(manifest_of(
        "hostkeeper-steps",
        layered(
            "hostkeeper-steps",
            &format!(
                "{CONFIG_DEP}{HOOK_DEP}{}",
                vendored_dep("hostkeeper-rights")
            ),
            "",
            TESTKIT_DEP,
        ),
    ));
    let dir = fixture("clean", edits);
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-rights is not a workspace member, \
             reached through hostkeeper-steps -> hostkeeper-rights"
        )
    );
}

/// Catches: a row of the step runner or the observer that leads to the launchd crate, directly
/// or through the rows of other crates, so that the macOS code would be linked into the layers
/// every platform builds; and a walk that stops at the first row.
#[test]
fn launchd_stays_above_steps_and_observe_in_the_rows() {
    for (from, to) in NOT_ABOVE {
        assert_eq!(reaches_in(&LAYERS, from, to), None, "{from}");
    }
    let through_placed = with_row(
        &LAYERS,
        "hostkeeper-placed",
        &["hostkeeper-config", "hostkeeper-launchd"],
    );
    for (from, to) in NOT_ABOVE {
        assert_eq!(reaches_in(&through_placed, from, to), None, "{from}");
    }
    let through_config = with_row(&LAYERS, "hostkeeper-config", &["hostkeeper-launchd"]);
    for (from, to) in NOT_ABOVE {
        assert_eq!(
            reaches_in(&through_config, from, to),
            Some(vec![
                from.to_string(),
                "hostkeeper-config".to_string(),
                to.to_string()
            ]),
            "{from}"
        );
    }
}

/// The launchd crate's manifest without its edge to `cut`, which a fixture that points `cut` at
/// launchd must drop: Cargo refuses the cycle.
fn launchd_without(cut: &str) -> (String, String) {
    let deps: String = [
        ("hostkeeper-hook", HOOK_DEP),
        ("hostkeeper-observe", OBSERVE_DEP),
        ("hostkeeper-placed", PLACED_DEP),
        ("hostkeeper-steps", STEPS_DEP),
    ]
    .iter()
    .filter(|(name, _)| *name != cut)
    .map(|(_, dep)| *dep)
    .collect();
    manifest_of(
        "hostkeeper-launchd",
        layered("hostkeeper-launchd", &deps, "", TESTKIT_DEP),
    )
}

/// What the launchd row reports when its manifest left out `cut`.
fn launchd_row_without(cut: &str) -> String {
    let all = [
        "hostkeeper-hook",
        "hostkeeper-observe",
        "hostkeeper-placed",
        "hostkeeper-steps",
    ];
    let kept: Vec<&str> = all.iter().copied().filter(|name| *name != cut).collect();
    format!(
        "hostkeeper-launchd depends (normal or build) on the workspace crates {{{}}}, \
         but must depend on exactly {{{}}}",
        kept.join(", "),
        all.join(", ")
    )
}

/// Catches: the step runner or the observer (which every platform builds) gaining a normal or
/// build dependency on the launchd crate, directly or through a crate outside the workspace, so
/// that the macOS code would be linked into the shared layers. The launchd crate depends on both,
/// so each fixture drops that edge (Cargo refuses the cycle), and the launchd row is reported too.
#[test]
fn hostkeeper_steps_and_observe_do_not_depend_on_launchd() {
    let launchd_dep = "hostkeeper-launchd = { path = \"../hostkeeper-launchd\" }\n";
    let steps_row = "hostkeeper-steps depends (normal or build) on the workspace crates \
                     {hostkeeper-config, hostkeeper-hook, hostkeeper-launchd}, but must depend \
                     on exactly {hostkeeper-config, hostkeeper-hook}";
    let dir = fixture(
        "clean",
        vec![
            launchd_without("hostkeeper-steps"),
            manifest_of(
                "hostkeeper-steps",
                layered(
                    "hostkeeper-steps",
                    &format!("{CONFIG_DEP}{HOOK_DEP}{launchd_dep}"),
                    "",
                    TESTKIT_DEP,
                ),
            ),
        ],
    );
    assert_eq!(
        check(&dir),
        Err(format!(
            "{}\n{steps_row}",
            launchd_row_without("hostkeeper-steps")
        ))
    );
    let mut through = steps_through_shim("hostkeeper-launchd");
    through.push(launchd_without("hostkeeper-steps"));
    let dir = fixture("clean", through);
    assert_eq!(
        check(&dir),
        Err(format!(
            "{}\n{steps_row}\nhostkeeper-steps reaches the workspace crate hostkeeper-launchd \
             through hostkeeper-steps -> shim -> hostkeeper-launchd",
            launchd_row_without("hostkeeper-steps")
        ))
    );
    let dir = fixture(
        "clean",
        vec![
            launchd_without("hostkeeper-observe"),
            manifest_of(
                "hostkeeper-observe",
                layered(
                    "hostkeeper-observe",
                    &format!("{CONFIG_DEP}{PROTO_DEP}{launchd_dep}"),
                    "",
                    TESTKIT_DEP,
                ),
            ),
        ],
    );
    assert_eq!(
        check(&dir),
        Err(format!(
            "{}\nhostkeeper-observe depends (normal or build) on the workspace crates \
             {{hostkeeper-config, hostkeeper-launchd, hostkeeper-proto}}, but must depend on \
             exactly {{hostkeeper-config, hostkeeper-proto}}",
            launchd_row_without("hostkeeper-observe")
        ))
    );
}

/// Catches: a check that refuses the edge the macOS crate needs, `hostkeeper-launchd` on the
/// step runner (its step launcher implements `StepLauncher`), or a row of the launchd crate that
/// leaves it out (the launchd crate without it is refused by its row); the edge runs down only,
/// and the runner and the observer still reach nothing of launchd.
#[test]
fn the_launchd_crate_may_depend_on_the_step_runner() {
    assert_eq!(check(&fixture("clean", vec![])), Ok(()));
    assert_eq!(
        reaches_in(&LAYERS, "hostkeeper-launchd", "hostkeeper-steps"),
        Some(vec![
            "hostkeeper-launchd".to_string(),
            "hostkeeper-steps".to_string()
        ])
    );
    let dir = fixture("clean", vec![launchd_without("hostkeeper-steps")]);
    assert_eq!(check(&dir), Err(launchd_row_without("hostkeeper-steps")));
    for (from, to) in NOT_ABOVE {
        assert_eq!(reaches_in(&LAYERS, from, to), None, "{from}");
    }
}
