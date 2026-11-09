//! The checks of a repository package's extracted tree and the enablement of its `run` units
//! (packages-native.md section 10.2, packages-repositories.md sections 3.4 and 5; packages-tests.md,
//! "The hook, scripts and paths"), run directly over scratch trees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::enable::{SYSTEMD_DIR, apply, remove_link, wanted};
use hostkeeper_install::repository::tree::{
    HOOK, Rules, TreeError, check, hook_path, install_links, is_service_unit, unit_file,
};
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// A tree under a scratch directory, built entry by entry.
struct Tree {
    scratch: Scratch,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let scratch = scratch(name);
        fs::create_dir(scratch.path().join("tree")).expect("the tree");
        Tree { scratch }
    }

    fn root(&self) -> PathBuf {
        self.scratch.path().join("tree")
    }

    fn file(&self, rel: &str, text: &str, mode: u32) -> &Tree {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
        fs::write(&path, text).expect("the file");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("the mode");
        self
    }

    fn dir(&self, rel: &str) -> &Tree {
        fs::create_dir_all(self.root().join(rel)).expect("the directory");
        self
    }

    fn link(&self, rel: &str, target: &str) -> &Tree {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
        symlink(target, path).expect("the link");
        self
    }
}

/// The rules of the service `mdm` whose package is `mdm`, with `run` and no boot step.
fn rules<'a>(run: &'a [String]) -> Rules<'a> {
    Rules {
        service: "mdm",
        package: "mdm",
        run,
        boot: None,
        params: false,
    }
}

fn checked(tree: &Tree) -> Result<bool, TreeError> {
    check(&tree.root(), &rules(&[]))
}

const UNITS: &str = "usr/lib/systemd/system";

/// Proves every path of the rule is allowed: the binaries, the package's and the service's own
/// directories, the plain unit files named for the service in both unit directories (run units or
/// not: `.service`, `.socket`, `.timer`, a `-` or `@` suffix), and the directories above them.
#[test]
fn a_tree_of_the_allowed_paths_passes() {
    let tree = Tree::new("allowed");
    for file in [
        "usr/bin/mdm",
        "usr/bin/tool/nested",
        "usr/lib/mdm/lib.so",
        "usr/share/mdm/data",
        "usr/share/doc/mdm/copyright",
        "etc/mdm/mdm.conf",
        "usr/lib/systemd/system/mdm.service",
        "usr/lib/systemd/system/mdm-worker.service",
        "usr/lib/systemd/system/mdm@.service",
        "usr/lib/systemd/system/mdm@one.service",
        "usr/lib/systemd/system/mdm.socket",
        "usr/lib/systemd/system/mdm.timer",
        "lib/systemd/system/mdm-cleanup.timer",
    ] {
        tree.file(file, "x", 0o644);
    }
    tree.link("usr/bin/mdm-alias", "mdm");
    assert_eq!(checked(&tree), Ok(false));
}

/// Proves the three kinds of unit and the eight units of a service's package (three of them run
/// units, five not) are accepted together.
#[test]
fn a_package_of_eight_units_with_three_run_units_passes() {
    let tree = Tree::new("eight");
    let names = [
        "mdm.service",
        "mdm-a.service",
        "mdm-b.service",
        "mdm@.service",
        "mdm.socket",
        "mdm-a.socket",
        "mdm.timer",
        "mdm-a.timer",
    ];
    for name in names {
        tree.file(&format!("{UNITS}/{name}"), "[Service]\n", 0o644);
    }
    let run: Vec<String> = ["mdm.service", "mdm-a.service", "mdm.timer"]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(check(&tree.root(), &rules(&run)), Ok(false));
}

/// Proves a package whose name differs from its service's has the package's directories under
/// `usr/lib`, `usr/share` and `usr/share/doc`, and the service's own under `etc` and in the unit
/// directories; neither takes the other's place; a unit is named for the service, whatever the package.
#[test]
fn the_package_and_the_service_have_their_own_paths() {
    let tree = Tree::new("names");
    for file in [
        "usr/lib/mdm-server/x",
        "usr/share/mdm-server/x",
        "usr/share/doc/mdm-server/x",
        "etc/mdm/x",
        "usr/lib/systemd/system/mdm.service",
        "usr/lib/systemd/system/mdm-server.service",
    ] {
        tree.file(file, "x", 0o644);
    }
    let rules = Rules {
        service: "mdm",
        package: "mdm-server",
        run: &[],
        boot: None,
        params: false,
    };
    assert_eq!(check(&tree.root(), &rules), Ok(false));
    for wrong in ["usr/lib/mdm/x", "etc/mdm-server/x"] {
        let tree = Tree::new("names-wrong");
        tree.file(wrong, "x", 0o644);
        let outside = check(&tree.root(), &rules);
        let first = wrong.rsplit_once('/').expect("a directory").0;
        assert!(
            matches!(&outside, Err(TreeError::Outside(path)) if path == first || path == wrong),
            "{wrong}: {outside:?}"
        );
    }
}

/// Proves each path outside the rule is refused, naming the path: an enablement link's
/// directory, a cron file, a sudoers file, another package's directory, a name that only starts
/// like the package's, a file under `/opt`, and a drop-in directory of the service's own unit.
#[test]
fn a_path_outside_the_rule_is_refused_naming_it() {
    for (file, outside) in [
        (
            "etc/systemd/system/multi-user.target.wants/x",
            "etc/systemd",
        ),
        ("etc/cron.d/mdm", "etc/cron.d"),
        ("etc/sudoers.d/mdm", "etc/sudoers.d"),
        ("usr/sbin/mdm", "usr/sbin"),
        ("usr/lib/other/x", "usr/lib/other"),
        ("usr/lib/mdmx/x", "usr/lib/mdmx"),
        ("usr/share/other/x", "usr/share/other"),
        ("usr/share/doc/other/x", "usr/share/doc/other"),
        ("opt/mdm/x", "opt"),
        ("var/lib/mdm/x", "var"),
        (
            "usr/lib/systemd/system/mdm.service.d/x.conf",
            "usr/lib/systemd/system/mdm.service.d",
        ),
        ("usr/lib/systemd/user/mdm.service", "usr/lib/systemd/user"),
        (
            "usr/lib/systemd/system/other.service",
            "usr/lib/systemd/system/other.service",
        ),
        (
            "usr/lib/systemd/system/mdmx.service",
            "usr/lib/systemd/system/mdmx.service",
        ),
        (
            "lib/systemd/system/mdm.mount",
            "lib/systemd/system/mdm.mount",
        ),
        (
            "usr/lib/systemd/system/mdm.service.wants/x",
            "usr/lib/systemd/system/mdm.service.wants",
        ),
        ("lib/modules/x", "lib/modules"),
    ] {
        let tree = Tree::new("outside");
        tree.file(file, "x", 0o644);
        let error = checked(&tree).expect_err(file);
        assert_eq!(error, TreeError::Outside(outside.to_owned()), "{file}");
        assert_eq!(
            error.to_string(),
            format!(
                "the package installs /{outside}, outside the paths a service's package may use"
            )
        );
    }
}

/// Proves a link in a unit directory, an empty drop-in directory, and a file named for a link
/// target outside the service's names are refused.
#[test]
fn a_link_or_a_directory_in_a_unit_directory_is_refused() {
    let tree = Tree::new("unit-link");
    tree.link("usr/lib/systemd/system/mdm.service", "/dev/null");
    assert_eq!(
        checked(&tree),
        Err(TreeError::Outside(
            "usr/lib/systemd/system/mdm.service".to_owned()
        ))
    );
    let tree = Tree::new("unit-dir");
    tree.dir("usr/lib/systemd/system/mdm.service.d");
    assert_eq!(
        checked(&tree),
        Err(TreeError::Outside(
            "usr/lib/systemd/system/mdm.service.d".to_owned()
        ))
    );
    let tree = Tree::new("unit-dir-as-unit");
    tree.dir("usr/lib/systemd/system/mdm.service");
    assert_eq!(
        checked(&tree),
        Err(TreeError::Outside(
            "usr/lib/systemd/system/mdm.service".to_owned()
        ))
    );
}

/// Proves a socket node in the tree is refused as neither a file, a directory nor a link.
#[test]
fn a_special_file_is_refused() {
    let tree = Tree::new("special");
    tree.dir("usr/bin");
    let _listener = UnixListener::bind(tree.root().join("usr/bin/mdm.sock")).expect("a socket");
    let error = checked(&tree).expect_err("a socket is refused");
    assert_eq!(error, TreeError::Special("usr/bin/mdm.sock".to_owned()));
    assert_eq!(
        error.to_string(),
        "the package installs /usr/bin/mdm.sock, which is neither a file, a directory nor a link"
    );
}

/// Proves a tree that cannot be read is refused with the OS error.
#[test]
fn a_tree_that_cannot_be_read_is_refused() {
    let scratch = scratch("missing-tree");
    let error = check(&scratch.path().join("none"), &rules(&[])).expect_err("no tree");
    assert_eq!(
        error,
        TreeError::Io("No such file or directory (os error 2)".to_owned())
    );
    assert_eq!(
        error.to_string(),
        "the tree cannot be read: No such file or directory (os error 2)"
    );
}

/// Proves `is_service_unit` accepts `<service>.<kind>`, `<service>-x.<kind>`, `<service>@.<kind>`
/// and `<service>@x.<kind>` of the kinds service, socket and timer, and refuses another name,
/// another kind, a name that only starts with the service, a name with a slash or odd bytes.
#[test]
fn is_service_unit_follows_the_naming_rule() {
    for name in [
        "mdm.service",
        "mdm-x.service",
        "mdm-x.y_z:w@1.service",
        "mdm@.service",
        "mdm@x.service",
        "mdm.socket",
        "mdm-x.socket",
        "mdm@x.timer",
        "mdm.timer",
    ] {
        assert!(is_service_unit("mdm", name), "{name}");
    }
    for name in [
        "other.service",
        "xmdm.service",
        "mdmx.service",
        "mdm.mount",
        "mdm.path",
        "mdm.swap",
        "mdm.service.d",
        "mdm",
        "mdm.",
        ".service",
        "",
        "mdm/x.service",
        "mdm-a/b.service",
        "mdm@x/y.timer",
        "mdm-x y.service",
        "mdm-é.service",
        "mdm_x.service",
    ] {
        assert!(!is_service_unit("mdm", name), "{name:?}");
    }
    assert!(is_service_unit("my-svc", "my-svc.service"));
    assert!(is_service_unit("my-svc", "my-svc-x.timer"));
    assert!(!is_service_unit("my-svc", "my.service"));
}

/// Proves a `run` unit that is not a regular file of one of the unit directories is refused,
/// naming it, and that both unit directories hold one.
#[test]
fn a_run_unit_must_be_a_unit_file_of_the_package() {
    let tree = Tree::new("run-missing");
    tree.file("usr/lib/systemd/system/mdm.service", "[Service]\n", 0o644);
    let run = vec!["mdm.service".to_owned(), "mdm-b.service".to_owned()];
    let error = check(&tree.root(), &rules(&run)).expect_err("a missing unit");
    assert_eq!(error, TreeError::NoUnit("mdm-b.service".to_owned()));
    assert_eq!(
        error.to_string(),
        "the run unit mdm-b.service is not a unit file of the package"
    );
    let tree = Tree::new("run-lib");
    tree.file("lib/systemd/system/mdm.service", "[Service]\n", 0o644);
    assert_eq!(
        check(&tree.root(), &rules(&["mdm.service".to_owned()])),
        Ok(false)
    );
    let tree = Tree::new("run-other-name");
    tree.file("usr/lib/systemd/system/mdm.service", "[Service]\n", 0o644);
    let run = vec!["mdm.service".to_owned(), "mdm.timer".to_owned()];
    assert_eq!(
        check(&tree.root(), &rules(&run)),
        Err(TreeError::NoUnit("mdm.timer".to_owned()))
    );
}

/// Proves `unit_file` finds a regular file in `usr/lib/systemd/system` first and
/// `lib/systemd/system` next, and nothing for a link, a directory or a missing unit.
#[test]
fn unit_file_finds_a_regular_file_in_either_directory() {
    let tree = Tree::new("unit-file");
    tree.file("usr/lib/systemd/system/a.service", "x", 0o644);
    tree.file("lib/systemd/system/b.service", "x", 0o644);
    tree.file("usr/lib/systemd/system/both.service", "x", 0o644);
    tree.file("lib/systemd/system/both.service", "y", 0o644);
    tree.link("usr/lib/systemd/system/link.service", "a.service");
    tree.dir("usr/lib/systemd/system/dir.service");
    let root = tree.root();
    assert_eq!(
        unit_file(&root, "a.service").as_deref(),
        Some("usr/lib/systemd/system/a.service")
    );
    assert_eq!(
        unit_file(&root, "b.service").as_deref(),
        Some("lib/systemd/system/b.service")
    );
    assert_eq!(
        unit_file(&root, "both.service").as_deref(),
        Some("usr/lib/systemd/system/both.service")
    );
    for none in ["link.service", "dir.service", "missing.service"] {
        assert_eq!(unit_file(&root, none), None, "{none}");
    }
}

/// Proves the hook is `usr/lib/<package>/hostkeeper-hook`, and that a package with an executable
/// regular one gets `Ok(true)`, also with a boot step and params, which only a hook reads.
#[test]
fn a_package_with_an_executable_hook_has_one() {
    assert_eq!(HOOK, "hostkeeper-hook");
    assert_eq!(hook_path("mdm"), "usr/lib/mdm/hostkeeper-hook");
    let tree = Tree::new("hook");
    tree.file("usr/lib/mdm/hostkeeper-hook", "#!/bin/sh\n", 0o755);
    assert_eq!(checked(&tree), Ok(true));
    let with = Rules {
        boot: Some("start"),
        params: true,
        ..rules(&[])
    };
    assert_eq!(check(&tree.root(), &with), Ok(true));
}

/// Proves a hook that is not executable, is a link, is a directory, or is reached through a link
/// is refused, naming the hook and what is wrong with it.
#[test]
fn a_hook_that_is_not_a_regular_executable_file_is_refused() {
    type Build = Box<dyn Fn(&Tree)>;
    let cases: [(&str, Build, &str); 3] = [
        (
            "plain",
            Box::new(|t| {
                t.file("usr/lib/mdm/hostkeeper-hook", "x", 0o644);
            }),
            "not executable",
        ),
        (
            "link",
            Box::new(|t| {
                t.file("usr/lib/mdm/real", "x", 0o755);
                t.link("usr/lib/mdm/hostkeeper-hook", "real");
            }),
            "a link",
        ),
        (
            "dir",
            Box::new(|t| {
                t.dir("usr/lib/mdm/hostkeeper-hook");
            }),
            "not a regular file",
        ),
    ];
    for (name, build, why) in cases {
        let tree = Tree::new(name);
        build(&tree);
        let error = checked(&tree).expect_err(name);
        assert_eq!(
            error,
            TreeError::Hook("usr/lib/mdm/hostkeeper-hook".to_owned(), why),
            "{name}"
        );
        assert_eq!(
            error.to_string(),
            format!("the hook /usr/lib/mdm/hostkeeper-hook is {why}")
        );
    }
}

/// Proves a package with no hook (nothing at its path, no package directory) has none, and then a
/// boot step or params is refused; a file or a link where the package directory would be is
/// refused by the path rule.
#[test]
fn without_a_hook_a_boot_step_or_params_is_refused() {
    let tree = Tree::new("no-hook");
    tree.file("usr/lib/mdm/other", "x", 0o755);
    assert_eq!(checked(&tree), Ok(false));
    let boot = Rules {
        boot: Some("start"),
        ..rules(&[])
    };
    let error = check(&tree.root(), &boot).expect_err("a boot step");
    assert_eq!(error, TreeError::BootWithoutHook("start".to_owned()));
    assert_eq!(
        error.to_string(),
        "the manifest names the boot step \"start\", and the package has no hook"
    );
    let params = Rules {
        params: true,
        ..rules(&[])
    };
    let error = check(&tree.root(), &params).expect_err("params");
    assert_eq!(error, TreeError::ParamsWithoutHook);
    assert_eq!(
        error.to_string(),
        "the manifest has params, which only a hook reads, and the package has no hook"
    );
    let both = Rules {
        boot: Some("start"),
        params: true,
        ..rules(&[])
    };
    assert_eq!(
        check(&tree.root(), &both),
        Err(TreeError::BootWithoutHook("start".to_owned()))
    );
    let bare = Tree::new("no-package-dir");
    bare.file("usr/bin/mdm", "x", 0o755);
    assert_eq!(
        check(&bare.root(), &boot),
        Err(TreeError::BootWithoutHook("start".to_owned()))
    );
    let file = Tree::new("file-for-dir");
    file.file("usr/lib/mdm", "x", 0o644);
    assert_eq!(
        checked(&file),
        Err(TreeError::Outside("usr/lib/mdm".to_owned()))
    );
    let linked = Tree::new("linked-dir");
    linked.link("usr/lib/mdm", "elsewhere");
    assert_eq!(
        checked(&linked),
        Err(TreeError::Outside("usr/lib/mdm".to_owned()))
    );
}

fn links(unit: &str, text: &str) -> Vec<String> {
    install_links(unit, text)
        .expect("the links")
        .into_iter()
        .collect()
}

/// Proves `WantedBy=` gives a `<target>.wants/<unit>` link and `RequiredBy=` a
/// `<target>.requires/<unit>` link, several targets on one line or on several lines give one each,
/// and a unit with no `[Install]` gives none.
#[test]
fn install_links_follow_wanted_by_and_required_by() {
    let unit = "mdm.service";
    assert_eq!(
        links(
            unit,
            "[Unit]\nDescription=x\n[Service]\nExecStart=/x\n[Install]\nWantedBy=multi-user.target\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert_eq!(
        links(unit, "[Install]\nRequiredBy=multi-user.target\n"),
        ["multi-user.target.requires/mdm.service"]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target timers.target\nWantedBy=default.target\nRequiredBy=network-online.target\n"
        ),
        [
            "default.target.wants/mdm.service",
            "multi-user.target.wants/mdm.service",
            "network-online.target.requires/mdm.service",
            "timers.target.wants/mdm.service",
        ]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\n  WantedBy = multi-user.target  \nAlias=x.service\nAlso=y.service\nnot an assignment\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert!(links(unit, "[Service]\nExecStart=/x\n").is_empty());
    assert!(links(unit, "").is_empty());
}

/// Proves an empty assignment drops the targets before it (as systemd reads it), later ones
/// still count, and a target named outside `[Install]` is ignored.
#[test]
fn install_links_follow_an_empty_assignment_and_ignore_other_sections() {
    let unit = "mdm.service";
    assert!(links(unit, "[Install]\nWantedBy=multi-user.target\nWantedBy=\n").is_empty());
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\nWantedBy=\nWantedBy=timers.target\n"
        ),
        ["timers.target.wants/mdm.service"]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\nRequiredBy=timers.target\nRequiredBy=\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert!(
        links(
            unit,
            "[Unit]\nWantedBy=multi-user.target\n[Service]\nRequiredBy=timers.target\n"
        )
        .is_empty()
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\n[Other]\nWantedBy=timers.target\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\n[Unit]\n[Install]\nRequiredBy=timers.target\n"
        ),
        [
            "multi-user.target.wants/mdm.service",
            "timers.target.requires/mdm.service"
        ]
    );
}

/// Proves a target that is not a unit name (no dot, a slash, an odd byte, empty after a dot-less
/// word, 256 bytes) is refused naming the unit and the target, and one of 255 bytes is accepted.
#[test]
fn install_links_refuse_a_target_that_is_not_a_unit_name() {
    let long = format!("{}.target", "a".repeat(249));
    for (key, target) in [
        ("WantedBy", "nodot"),
        ("WantedBy", "a/timers.target"),
        ("RequiredBy", "bad name.timer,x"),
        ("WantedBy", "é.target"),
        ("RequiredBy", "../x.target"),
        ("WantedBy", long.as_str()),
    ] {
        let text = format!("[Install]\n{key}={target}\n");
        let error = install_links("mdm.service", &text).expect_err(target);
        let shown = target.split_whitespace().next().expect("a word");
        let expected = match shown == target {
            true => target.to_owned(),
            false => shown.to_owned(),
        };
        assert!(
            matches!(&error, TreeError::Target { unit, target: found } if unit == "mdm.service" && found == &expected),
            "{target}: {error:?}"
        );
    }
    let error = install_links("mdm.service", "[Install]\nWantedBy=nodot\n").expect_err("nodot");
    assert_eq!(
        error.to_string(),
        "the unit mdm.service is wanted by \"nodot\", not a unit name"
    );
    let at_bound = format!("{}.target", "a".repeat(248));
    assert_eq!(at_bound.len(), 255);
    assert_eq!(
        links("mdm.service", &format!("[Install]\nWantedBy={at_bound}\n")).len(),
        1
    );
}

const MDM_UNIT: &str = "[Unit]\nDescription=mdm\n[Service]\nExecStart=/usr/bin/mdm\n[Install]\nWantedBy=multi-user.target\nRequiredBy=graphical.target\n";

/// Proves `wanted` gives, for each run unit, the absolute link path under the service manager's
/// directory and the absolute path of the unit file where the package installs it (in either
/// unit directory).
#[test]
fn wanted_maps_each_link_to_its_unit_file() {
    assert_eq!(SYSTEMD_DIR, "etc/systemd/system");
    let tree = Tree::new("wanted");
    tree.file("usr/lib/systemd/system/mdm.service", MDM_UNIT, 0o644);
    tree.file(
        "lib/systemd/system/mdm.timer",
        "[Install]\nWantedBy=timers.target\n",
        0o644,
    );
    tree.file(
        "usr/lib/systemd/system/mdm-quiet.service",
        "[Service]\n",
        0o644,
    );
    let run = ["mdm.service", "mdm.timer", "mdm-quiet.service"].map(str::to_owned);
    let found = wanted(&tree.root(), &run).expect("the links");
    let want: BTreeMap<String, String> = [
        (
            "/etc/systemd/system/multi-user.target.wants/mdm.service",
            "/usr/lib/systemd/system/mdm.service",
        ),
        (
            "/etc/systemd/system/graphical.target.requires/mdm.service",
            "/usr/lib/systemd/system/mdm.service",
        ),
        (
            "/etc/systemd/system/timers.target.wants/mdm.timer",
            "/lib/systemd/system/mdm.timer",
        ),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .into_iter()
    .collect();
    assert_eq!(found, want);
}

/// Proves `wanted` refuses a run unit the package does not hold, a unit that cannot be read, and a
/// unit asking for a target that is not a unit name.
#[test]
fn wanted_refuses_a_missing_unreadable_or_bad_unit() {
    let tree = Tree::new("wanted-bad");
    tree.file(
        "usr/lib/systemd/system/mdm.service",
        "[Install]\nWantedBy=nodot\n",
        0o644,
    );
    tree.file("usr/lib/systemd/system/locked.service", "x", 0o000);
    let run = ["absent.service".to_owned()];
    assert_eq!(
        wanted(&tree.root(), &run),
        Err(TreeError::NoUnit("absent.service".to_owned()))
    );
    let run = ["mdm.service".to_owned()];
    assert_eq!(
        wanted(&tree.root(), &run),
        Err(TreeError::Target {
            unit: "mdm.service".to_owned(),
            target: "nodot".to_owned()
        })
    );
    let run = ["locked.service".to_owned()];
    assert_eq!(
        wanted(&tree.root(), &run),
        Err(TreeError::Io("Permission denied (os error 13)".to_owned()))
    );
}

/// Proves a unit file that is not UTF-8 is read lossily: its `[Install]` section still counts.
#[test]
fn wanted_reads_a_unit_that_is_not_utf8() {
    let tree = Tree::new("wanted-binary");
    let path = tree.root().join("usr/lib/systemd/system/mdm.service");
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
    fs::write(&path, b"# \xff\n[Install]\nWantedBy=multi-user.target\n").expect("the unit");
    let found = wanted(&tree.root(), &["mdm.service".to_owned()]).expect("the links");
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        ["/etc/systemd/system/multi-user.target.wants/mdm.service"]
    );
}

const LINK: &str = "/etc/systemd/system/multi-user.target.wants/mdm.service";
const UNIT: &str = "/usr/lib/systemd/system/mdm.service";

fn want_of(links: &[(&str, &str)]) -> BTreeMap<String, String> {
    links
        .iter()
        .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
        .collect()
}

fn recorded(links: &[&str]) -> BTreeSet<String> {
    links.iter().map(|l| (*l).to_owned()).collect()
}

fn real(root: &Path, link: &str) -> PathBuf {
    root.join(link.trim_start_matches('/'))
}

/// Proves `apply` writes a missing link (making the directories, mode 0755) to the unit file,
/// answers it as owned, and keeps a link it recorded and still finds.
#[test]
fn apply_creates_a_missing_link_and_keeps_a_recorded_one() {
    let root = scratch("apply-create");
    let want = want_of(&[(LINK, UNIT)]);
    let owned = apply(root.path(), &want, &recorded(&[])).expect("the links");
    assert_eq!(owned, recorded(&[LINK]));
    let path = real(root.path(), LINK);
    assert_eq!(fs::read_link(&path).expect("a link"), Path::new(UNIT));
    let dir = path.parent().expect("a parent");
    assert_eq!(
        fs::metadata(dir)
            .expect("the directory")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let again = apply(root.path(), &want, &recorded(&[LINK])).expect("the links");
    assert_eq!(again, recorded(&[LINK]));
    assert_eq!(fs::read_link(&path).expect("a link"), Path::new(UNIT));
}

/// Proves a link already there that hostkeeper did not record (an operator's, the package's own
/// scripts', a file) is left as it is and is not claimed, and a recorded link that now points
/// elsewhere is not claimed either.
#[test]
fn apply_leaves_a_foreign_link_unclaimed() {
    let root = scratch("apply-foreign");
    let path = real(root.path(), LINK);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
    symlink("/somewhere/else", &path).expect("the link");
    let want = want_of(&[(LINK, UNIT)]);
    assert_eq!(
        apply(root.path(), &want, &recorded(&[])).expect("ok"),
        recorded(&[])
    );
    assert_eq!(
        fs::read_link(&path).expect("a link"),
        Path::new("/somewhere/else")
    );
    // Recorded, but it points elsewhere now: not ours.
    assert_eq!(
        apply(root.path(), &want, &recorded(&[LINK])).expect("ok"),
        recorded(&[])
    );
    assert_eq!(
        fs::read_link(&path).expect("a link"),
        Path::new("/somewhere/else")
    );
    // The same for a regular file.
    fs::remove_file(&path).expect("the link");
    fs::write(&path, "x").expect("a file");
    assert_eq!(
        apply(root.path(), &want, &recorded(&[LINK])).expect("ok"),
        recorded(&[])
    );
    assert_eq!(fs::read_to_string(&path).expect("the file"), "x");
}

/// Proves a recorded link that is no longer wanted is taken away, one that is now a regular file
/// or is missing is left without error, and the other wanted links are still written.
#[test]
fn apply_removes_a_recorded_link_no_longer_wanted() {
    let root = scratch("apply-remove");
    let old = "/etc/systemd/system/multi-user.target.wants/old.service";
    let file = "/etc/systemd/system/multi-user.target.wants/file.service";
    let gone = "/etc/systemd/system/multi-user.target.wants/gone.service";
    let dir = real(root.path(), old)
        .parent()
        .expect("a parent")
        .to_path_buf();
    fs::create_dir_all(&dir).expect("the directory");
    symlink(UNIT, real(root.path(), old)).expect("the link");
    fs::write(real(root.path(), file), "x").expect("a file");
    let want = want_of(&[(LINK, UNIT)]);
    let owned = apply(root.path(), &want, &recorded(&[old, file, gone])).expect("the links");
    assert_eq!(owned, recorded(&[LINK]));
    assert!(
        fs::symlink_metadata(real(root.path(), old)).is_err(),
        "the old link is gone"
    );
    assert!(
        real(root.path(), file).is_file(),
        "a file is not ours to remove"
    );
    assert!(fs::symlink_metadata(real(root.path(), LINK)).is_ok());
}

/// Proves a path that cannot be looked at (a file where a directory should be) is an error, for
/// both a wanted link and a recorded one.
#[test]
fn apply_and_remove_link_report_a_path_that_cannot_be_read() {
    let root = scratch("apply-error");
    fs::create_dir_all(root.path().join("etc")).expect("the directory");
    fs::write(root.path().join("etc/systemd"), "x").expect("a file");
    let want = want_of(&[(LINK, UNIT)]);
    assert!(apply(root.path(), &want, &recorded(&[])).is_err());
    assert!(apply(root.path(), &BTreeMap::new(), &recorded(&[LINK])).is_err());
    assert!(remove_link(root.path(), LINK).is_err());
}

/// Proves `remove_link` removes a link, and leaves a regular file, a directory and a missing path
/// alone without error.
#[test]
fn remove_link_takes_away_a_link_and_nothing_else() {
    let root = scratch("remove-link");
    let dir = real(root.path(), "/etc/systemd/system");
    fs::create_dir_all(&dir).expect("the directory");
    symlink(UNIT, dir.join("a.service")).expect("the link");
    fs::write(dir.join("b.service"), "x").expect("a file");
    fs::create_dir(dir.join("c.service")).expect("a directory");
    remove_link(root.path(), "/etc/systemd/system/a.service").expect("the link goes");
    assert!(fs::symlink_metadata(dir.join("a.service")).is_err());
    for name in ["b.service", "c.service", "d.service"] {
        remove_link(root.path(), &format!("/etc/systemd/system/{name}")).expect("left alone");
    }
    assert!(dir.join("b.service").is_file());
    assert!(dir.join("c.service").is_dir());
}

/// Proves `apply` reports a link that cannot be written (a directory it cannot write into, or one
/// it cannot make) as an error, so the step fails instead of recording a link it did not make.
#[test]
fn apply_reports_a_link_it_cannot_write() {
    let root = scratch("apply-readonly");
    let units = real(root.path(), "/etc/systemd/system/multi-user.target.wants");
    fs::create_dir_all(&units).expect("the directory");
    fs::set_permissions(&units, fs::Permissions::from_mode(0o555)).expect("the mode");
    let want = want_of(&[(LINK, UNIT)]);
    let no_link = apply(root.path(), &want, &recorded(&[]));
    fs::set_permissions(&units, fs::Permissions::from_mode(0o755)).expect("the mode");
    let other = scratch("apply-readonly-parent");
    let etc = other.path().join("etc");
    fs::create_dir(&etc).expect("the directory");
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o555)).expect("the mode");
    let no_directory = apply(other.path(), &want, &recorded(&[]));
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).expect("the mode");
    assert_eq!(
        no_link.expect_err("no link").to_string(),
        "Permission denied (os error 13)"
    );
    assert_eq!(
        no_directory.expect_err("no directory").to_string(),
        "Permission denied (os error 13)"
    );
    assert!(fs::symlink_metadata(real(root.path(), LINK)).is_err());
}
