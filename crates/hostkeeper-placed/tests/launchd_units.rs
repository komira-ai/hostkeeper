//! A package of launchd plists, placed on a host whose unit directory is launchd's: each
//! `units/<label>.plist` is linked at `/Library/LaunchDaemons/<label>.plist` through `current`, a
//! `[units] run` entry that is a label gets no `WantedBy=` link and needs no `[Install]`, and the
//! update and removal of such a package go through the same placer steps as a systemd one. The
//! shape is hello's macOS package: two slots, `hello.a` and `hello.b`, each a plist and a run
//! entry.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hostkeeper_placed::testing::UnitCall;
use hostkeeper_placed::{ClaimError, Claims, LintError, PathKind, PlacedError};
use support::{LAUNCHD_DIR, Spec, World, service, version};

const PACKAGES: &str = "/var/lib/hostkeeper/packages";

/// A launchd plist whose job runs `program`.
fn plist(program: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>hello.a</string>\n\t<key>ProgramArguments</key>\n\
         \t<array>\n\t\t<string>{program}</string>\n\t</array>\n</dict>\n</plist>\n"
    )
}

/// hello's macOS package: the plists of its two slots, both run.
fn hello() -> Spec {
    let program = format!("{PACKAGES}/hello/current/bin/hello");
    Spec::new()
        .unit("hello.a.plist", &plist(&program))
        .unit("hello.b.plist", &plist(&program))
        .run("hello.a")
        .run("hello.b")
}

/// The placed link of the plist of `label`.
fn placed(label: &str) -> PathBuf {
    Path::new(LAUNCHD_DIR).join(format!("{label}.plist"))
}

/// What that link points at: the plist of `current`.
fn through_current(label: &str) -> PathBuf {
    PathBuf::from(format!("{PACKAGES}/hello/current/units/{label}.plist"))
}

/// Catches a plist refused as a unit file, linked somewhere else, or given a `WantedBy=` link:
/// the claims of hello's macOS package are exactly its two plists in the launchd directory,
/// each a unit linked into `current`; `desired` adds nothing to them, and the lint passes
/// though no plist has an `[Install]` section.
#[test]
fn a_plist_package_claims_its_plists_and_no_wants_link() {
    let world = World::launchd("plist-claims");
    let manifest = world.package("hello", "1.0.0", &hello());

    let claims = Claims::of(&world.roots, &manifest).expect("the plists are claimed");
    let found: Vec<(PathBuf, PathKind, PathBuf)> = claims
        .iter()
        .map(|claim| (claim.path.clone(), claim.kind, claim.link_target.clone()))
        .collect();
    assert_eq!(
        found,
        vec![
            (
                placed("hello.a"),
                PathKind::Unit,
                through_current("hello.a")
            ),
            (
                placed("hello.b"),
                PathKind::Unit,
                through_current("hello.b")
            ),
        ]
    );
    let desired = world.placer().desired(&manifest).expect("desired");
    assert_eq!(desired.paths(), claims.paths());
    assert_eq!(
        hostkeeper_placed::lint::lint(&world.fs, &world.roots, &manifest),
        Ok(())
    );
}

/// Catches a plist whose label is not the service's getting a link: a manifest edited after
/// loading to list `other.a.plist` is refused `UnitName` by `Claims::of`, and so by `desired`.
#[test]
fn a_plist_of_another_service_is_refused() {
    let world = World::launchd("plist-foreign");
    let mut manifest = world.package("hello", "1.0.0", &hello());
    manifest.unit_files.insert("other.a.plist".to_owned());

    let expected = ClaimError::UnitName {
        service: "hello".to_owned(),
        file: "other.a.plist".to_owned(),
    };
    assert_eq!(
        Claims::of(&world.roots, &manifest).expect_err("another service's plist"),
        expected
    );
    assert!(matches!(
        world.placer().desired(&manifest),
        Err(PlacedError::Claim(found)) if found == expected
    ));
}

/// Catches a launchd run entry taken without its plist, or a label of another service taken as
/// a run entry: a manifest edited to run `hello.c` (no `hello.c.plist`) or `other.a` is refused
/// `RunUnit` by the lint and by `desired`, which is what the update's switch calls.
#[test]
fn a_run_label_needs_its_plist_and_the_services_name() {
    let world = World::launchd("plist-run");
    let loaded = world.package("hello", "1.0.0", &hello());
    for unit in ["hello.c", "other.a", "hello a"] {
        let mut manifest = loaded.clone();
        manifest.run.push(unit.to_owned());
        let expected = LintError::RunUnit {
            unit: unit.to_owned(),
        };
        assert_eq!(
            hostkeeper_placed::lint::lint(&world.fs, &world.roots, &manifest),
            Err(expected.clone()),
            "{unit}"
        );
        assert!(
            matches!(
                world.placer().desired(&manifest),
                Err(PlacedError::Lint(found)) if found == expected
            ),
            "{unit}"
        );
    }
}

/// Catches an update of a plist package that loses, duplicates or never reloads its links, and
/// a removal that leaves one: the install links both plists through `current`, enabling writes
/// nothing more, the update to `1.1.0` (`relink_to` after `current` moves) keeps the same two
/// links and reloads, and `remove_all` stops both labels' plists, removes both links and
/// reloads once more, leaving no link in the launchd directory and nothing in the record.
#[test]
fn a_plist_package_is_installed_updated_and_removed() {
    let world = World::launchd("plist-flow");
    let placer = world.placer();
    let first = world.package("hello", "1.0.0", &hello());
    let linking = placer
        .begin(&placer.desired(&first).expect("desired"))
        .expect("begin");
    placer.link(&linking).expect("link");
    placer
        .switch_current(&service("hello"), &version("1.0.0"))
        .expect("switch");
    placer.reload().expect("reload");
    placer.commit(linking).expect("commit");
    assert!(
        !placer.enable_run_units(&first).expect("enable"),
        "a launchd label has no WantedBy= link to write"
    );
    let both: Vec<PathBuf> = vec![placed("hello.a"), placed("hello.b")];
    assert_eq!(world.placed_links(), both);
    for label in ["hello.a", "hello.b"] {
        assert_eq!(world.read_link(placed(label)), Some(through_current(label)));
        assert_eq!(
            world.resolve(placed(label)),
            Some(PathBuf::from(format!(
                "{PACKAGES}/hello/1.0.0/units/{label}.plist"
            )))
        );
    }

    let second = world.package("hello", "1.1.0", &hello());
    placer
        .switch_current(&service("hello"), &version("1.1.0"))
        .expect("switch to 1.1.0");
    let reloads_before = count_reloads(&world);
    assert!(
        placer.relink_to(&second).expect("relink"),
        "the update relinks"
    );
    assert_eq!(count_reloads(&world), reloads_before + 1);
    assert_eq!(world.placed_links(), both);
    assert_eq!(
        world.resolve(placed("hello.b")),
        Some(PathBuf::from(format!(
            "{PACKAGES}/hello/1.1.0/units/hello.b.plist"
        )))
    );
    let versions: BTreeSet<String> = world
        .record()
        .paths
        .values()
        .map(|entry| entry.version.clone())
        .collect();
    assert_eq!(versions, BTreeSet::from(["1.1.0".to_owned()]));

    let removal = placer.remove_all(&service("hello")).expect("remove");
    assert_eq!(removal.removed, both);
    assert!(removal.reloaded);
    assert_eq!(
        removal.stopped,
        vec!["hello.a".to_owned(), "hello.b".to_owned()]
    );
    assert_eq!(world.placed_links(), Vec::<PathBuf>::new());
    assert!(world.record().paths.is_empty());
    assert_eq!(count_reloads(&world), reloads_before + 2);
}

fn count_reloads(world: &World) -> usize {
    world
        .unit_calls()
        .iter()
        .filter(|call| **call == UnitCall::DaemonReload)
        .count()
}
