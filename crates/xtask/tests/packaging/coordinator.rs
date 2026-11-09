//! The coordinator's packaging in `packaging/coordinator/`: the unit runs the coordinator as its
//! own user with the flags of `serve.toml` and no environment; the sysusers.d entry and the
//! tmpfiles rules make that user and its two directories; `nfpm.yaml` installs exactly the
//! program, the web app and those four files, root's, and depends on systemd alone; the scripts
//! never enable or start the unit, and stop it only on removal. Each check is a function of the
//! committed texts, and each is shown to refuse its defect on an altered text.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::script;
use crate::unit::Unit;
use crate::yaml::{self, Node, Value};
use crate::{altered, refuses};

const USER: &str = "hostkeeper-coordinator";
const STATE_DIR: &str = "/var/lib/hostkeeper-coordinator";
const CONFIG_DIR: &str = "/etc/hostkeeper-coordinator";
const EXEC: &str =
    "/usr/bin/hostkeeper-coordinator serve --config /etc/hostkeeper-coordinator/serve.toml";

/// What `nfpm.yaml` installs, by destination: its source, and whether it is the web app's tree.
const CONTENTS: [(&str, &str, bool); 6] = [
    (
        "/usr/bin/hostkeeper-coordinator",
        "target/nfpm-coordinator/hostkeeper-coordinator",
        false,
    ),
    (
        "/usr/share/hostkeeper-coordinator/ui",
        "target/nfpm-coordinator/ui",
        true,
    ),
    (
        "/usr/lib/systemd/system/hostkeeper-coordinator.service",
        "packaging/coordinator/hostkeeper-coordinator.service",
        false,
    ),
    (
        "/usr/lib/sysusers.d/hostkeeper-coordinator.conf",
        "packaging/coordinator/hostkeeper-coordinator.sysusers.conf",
        false,
    ),
    (
        "/usr/lib/tmpfiles.d/hostkeeper-coordinator.conf",
        "packaging/coordinator/hostkeeper-coordinator.tmpfiles.conf",
        false,
    ),
    (
        "/usr/share/hostkeeper-coordinator/serve.toml.template",
        "packaging/coordinator/serve.toml.template",
        false,
    ),
];

fn read(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/coordinator")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn check_unit(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let want = [
        ("Service", "Type", "simple"),
        ("Service", "User", USER),
        ("Service", "Group", USER),
        ("Service", "ExecStart", EXEC),
        ("Service", "ReadWritePaths", STATE_DIR),
        ("Install", "WantedBy", "multi-user.target"),
    ];
    for (section, key, value) in want {
        let got = unit.single(section, key)?;
        if got != value {
            return Err(format!("[{section}] {key}={got}, not {value}"));
        }
    }
    for key in ["Environment", "EnvironmentFile", "PassEnvironment"] {
        if !unit.values("Service", key).is_empty() {
            return Err(format!(
                "[Service] {key}= is set: the coordinator takes flags, not environment"
            ));
        }
    }
    Ok(())
}

fn words(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

fn rules(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(words)
        .collect()
}

fn check_sysusers(text: &str) -> Result<(), String> {
    match rules(text).as_slice() {
        [rule]
            if rule.first() == Some(&"u")
                && rule.get(1) == Some(&USER)
                && rule.last() == Some(&STATE_DIR) =>
        {
            Ok(())
        }
        other => Err(format!(
            "the sysusers.d entry is {other:?}, not one `u {USER} ... {STATE_DIR}`"
        )),
    }
}

fn check_tmpfiles(text: &str) -> Result<(), String> {
    let want = vec![
        vec!["d", CONFIG_DIR, "0750", "root", USER, "-"],
        vec!["d", STATE_DIR, "0700", USER, USER, "-"],
    ];
    let got = rules(text);
    if got != want {
        return Err(format!("the tmpfiles rules are {got:?}, not {want:?}"));
    }
    Ok(())
}

/// The entries of a YAML mapping, by their text key.
fn mapping(node: &Node) -> BTreeMap<&str, &Node> {
    match &node.value {
        Value::Map(entries) => entries
            .iter()
            .filter_map(|(k, v)| k.as_str().map(|k| (k, v)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

fn texts(node: Option<&&Node>) -> Vec<String> {
    match node.map(|n| &n.value) {
        Some(Value::Seq(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn check_nfpm(text: &str) -> Result<(), String> {
    let root = yaml::load(text)?;
    let top = mapping(&root);
    let name = top.get("name").and_then(|n| n.as_str());
    if name != Some(USER) {
        return Err(format!("name is {name:?}, not {USER}"));
    }
    let depends = texts(top.get("depends"));
    if depends != ["systemd"] {
        return Err(format!("depends is {depends:?}, not [\"systemd\"]"));
    }
    let Some(Value::Seq(items)) = top.get("contents").map(|n| &n.value) else {
        return Err("no contents list".to_owned());
    };
    let mut got = Vec::new();
    for item in items {
        let entry = mapping(item);
        let field = |key: &str| {
            entry
                .get(key)
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_owned()
        };
        let info = entry
            .get("file_info")
            .map(|n| mapping(n))
            .unwrap_or_default();
        let owner = |key: &str| {
            info.get(key)
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_owned()
        };
        if (owner("owner"), owner("group")) != ("root".to_owned(), "root".to_owned()) {
            return Err(format!("{} is not root:root", field("dst")));
        }
        got.push((field("dst"), field("src"), field("type") == "tree"));
    }
    let want: Vec<(String, String, bool)> = CONTENTS
        .iter()
        .map(|(dst, src, tree)| ((*dst).to_owned(), (*src).to_owned(), *tree))
        .collect();
    if got != want {
        return Err(format!("contents are {got:?}, not {want:?}"));
    }
    let scripts = top.get("scripts").map(|n| mapping(n)).unwrap_or_default();
    for (key, path) in [
        (
            "postinstall",
            "packaging/coordinator/scripts/postinstall.sh",
        ),
        ("preremove", "packaging/coordinator/scripts/preremove.sh"),
    ] {
        if scripts.get(key).and_then(|n| n.as_str()) != Some(path) {
            return Err(format!("scripts.{key} is not {path}"));
        }
    }
    if scripts.len() != 2 {
        return Err(format!("scripts has {} entries, not 2", scripts.len()));
    }
    Ok(())
}

/// The programs `text` runs with the package manager's `args`, each command line one string.
fn ran(text: &str, args: &[&str]) -> Result<Vec<String>, String> {
    let commands = script::parse(text)?;
    let run = script::run(&commands, args, |_| 0);
    if run.status != 0 {
        return Err(format!("exits {} with {args:?}", run.status));
    }
    Ok(run.ran.iter().map(|w| w.join(" ")).collect())
}

/// The post-install script runs exactly these on every install and upgrade, deb and rpm.
const POSTINSTALL_RUNS: [&str; 3] = [
    "systemd-sysusers hostkeeper-coordinator.conf",
    "systemd-tmpfiles --create hostkeeper-coordinator.conf",
    "systemctl daemon-reload",
];

fn check_postinstall(text: &str) -> Result<(), String> {
    for args in [
        &["configure", ""][..],
        &["configure", "0.1.4"],
        &["1"],
        &["2"],
    ] {
        let got = ran(text, args)?;
        if got != POSTINSTALL_RUNS {
            return Err(format!(
                "with {args:?} it runs {got:?}, not {POSTINSTALL_RUNS:?}"
            ));
        }
    }
    Ok(())
}

fn check_preremove(text: &str) -> Result<(), String> {
    let stop = vec!["systemctl disable --now hostkeeper-coordinator.service".to_owned()];
    for args in [&["remove"][..], &["0"]] {
        let got = ran(text, args)?;
        if got != stop {
            return Err(format!(
                "on removal ({args:?}) it runs {got:?}, not {stop:?}"
            ));
        }
    }
    for args in [&["upgrade", "0.1.6"][..], &["1"]] {
        let got = ran(text, args)?;
        if !got.is_empty() {
            return Err(format!(
                "on an upgrade ({args:?}) it runs {got:?}, not nothing"
            ));
        }
    }
    Ok(())
}

#[test]
fn the_unit_runs_the_coordinator_as_its_user_with_the_flags_of_serve_toml() {
    let text = read("hostkeeper-coordinator.service");
    assert_eq!(check_unit(&text), Ok(()));
    refuses(
        check_unit,
        &[
            (
                altered(&text, "User=hostkeeper-coordinator", "User=root"),
                "User=root, not",
            ),
            (
                altered(
                    &text,
                    "ExecStart=/usr/bin/hostkeeper-coordinator serve --config",
                    "ExecStart=/usr/bin/hostkeeper-coordinator serve --state-file x --config",
                ),
                "ExecStart=",
            ),
            (
                altered(
                    &text,
                    "[Service]\n",
                    "[Service]\nEnvironmentFile=/etc/default/x\n",
                ),
                "EnvironmentFile= is set",
            ),
            (
                altered(
                    &text,
                    "ReadWritePaths=/var/lib/hostkeeper-coordinator\n",
                    "",
                ),
                "ReadWritePaths",
            ),
        ],
    );
}

#[test]
fn the_user_and_its_two_directories_are_made_as_the_unit_needs_them() {
    let sysusers = read("hostkeeper-coordinator.sysusers.conf");
    let tmpfiles = read("hostkeeper-coordinator.tmpfiles.conf");
    assert_eq!(check_sysusers(&sysusers), Ok(()));
    assert_eq!(check_tmpfiles(&tmpfiles), Ok(()));
    refuses(
        check_sysusers,
        &[(
            altered(
                &sysusers,
                "u      hostkeeper-coordinator",
                "u      coordinator",
            ),
            "not one `u",
        )],
    );
    refuses(
        check_tmpfiles,
        &[
            (
                altered(
                    &tmpfiles,
                    "0700 hostkeeper-coordinator",
                    "0755 hostkeeper-coordinator",
                ),
                "the tmpfiles rules",
            ),
            (
                altered(&tmpfiles, "0750 root", "0750 hostkeeper-coordinator"),
                "the tmpfiles rules",
            ),
        ],
    );
}

#[test]
fn nfpm_yaml_installs_the_program_the_web_app_and_the_four_files_and_depends_on_systemd_alone() {
    let text = read("nfpm.yaml");
    assert_eq!(check_nfpm(&text), Ok(()));
    refuses(
        check_nfpm,
        &[
            (
                altered(&text, "  - systemd\n", "  - systemd\n  - libc6\n"),
                "depends is",
            ),
            (altered(&text, "    type: tree\n", ""), "contents are"),
            (
                altered(
                    &text,
                    "  - src: packaging/coordinator/serve.toml.template\n",
                    "  - src: packaging/coordinator/serve.toml\n",
                ),
                "contents are",
            ),
            (
                altered(
                    &text,
                    "name: hostkeeper-coordinator\n",
                    "name: hostkeeper\n",
                ),
                "name is",
            ),
            (
                altered(
                    &text,
                    "    dst: /usr/bin/hostkeeper-coordinator\n    file_info:\n      mode: 0755\n      owner: root",
                    "    dst: /usr/bin/hostkeeper-coordinator\n    file_info:\n      mode: 0755\n      owner: nobody",
                ),
                "is not root:root",
            ),
            (
                altered(
                    &text,
                    "  preremove: packaging/coordinator/scripts/preremove.sh\n",
                    "",
                ),
                "scripts.preremove",
            ),
        ],
    );
}

#[test]
fn the_scripts_never_enable_or_start_the_unit_and_stop_it_only_on_removal() {
    let post = read("scripts/postinstall.sh");
    let pre = read("scripts/preremove.sh");
    assert_eq!(check_postinstall(&post), Ok(()));
    assert_eq!(check_preremove(&pre), Ok(()));
    refuses(
        check_postinstall,
        &[
            (
                altered(
                    &post,
                    "systemctl daemon-reload || true\n",
                    "systemctl daemon-reload || true\nsystemctl enable hostkeeper-coordinator.service\n",
                ),
                "it runs",
            ),
            (
                altered(&post, "systemd-sysusers hostkeeper-coordinator.conf\n", ""),
                "it runs",
            ),
        ],
    );
    refuses(
        check_preremove,
        &[
            (
                altered(&pre, "remove | 0)", "remove | 0 | 1)"),
                "on an upgrade",
            ),
            (altered(&pre, "disable --now", "disable"), "on removal"),
        ],
    );
}

const COORDINATOR_NFPM: &str = "nfpm.yaml";
const ARTIFACTS_WORKFLOW: &str = "../../.github/workflows/artifacts.yml";

/// The flag and directory of the `check-linux-pkg` command that the header comment of
/// `nfpm.yaml` names: the words after `<package>` up to the closing backtick.
fn header_check_args(nfpm: &str) -> Result<String, String> {
    let comment: String = nfpm
        .lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim())
        .collect::<Vec<_>>()
        .join(" ");
    let (_, after) = comment
        .split_once("release check-linux-pkg <package> ")
        .ok_or("the header names no `release check-linux-pkg <package>` command")?;
    let (args, _) = after
        .split_once('`')
        .ok_or("the header's command has no closing backtick")?;
    Ok(args.to_owned())
}

/// The flag and directory of each coordinator `check-linux-pkg` step of the workflow: the words
/// after its quoted package path.
fn workflow_check_args(workflow: &str) -> Vec<String> {
    workflow
        .lines()
        .filter(|l| l.contains("release check-linux-pkg \"dist/hostkeeper-coordinator"))
        .filter_map(|l| l.rsplit_once("\" ").map(|(_, args)| args.trim().to_owned()))
        .collect()
}

fn check_header_matches_workflow(nfpm: &str, workflow: &str) -> Result<(), String> {
    let header = header_check_args(nfpm)?;
    let steps = workflow_check_args(workflow);
    if steps.len() != 2 {
        return Err(format!(
            "the workflow has {} coordinator check-linux-pkg steps, not two (deb and rpm)",
            steps.len()
        ));
    }
    match steps.iter().find(|args| **args != header) {
        Some(args) => Err(format!(
            "the header names `{header}` but the workflow's step runs `{args}`"
        )),
        None => Ok(()),
    }
}

#[test]
fn the_header_names_the_check_the_workflow_runs_on_the_coordinators_packages() {
    let nfpm = read(COORDINATOR_NFPM);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/coordinator");
    let workflow = fs::read_to_string(path.join(ARTIFACTS_WORKFLOW)).expect("read artifacts.yml");
    assert_eq!(
        header_check_args(&nfpm),
        Ok("--coordinator-staged target/nfpm-coordinator".to_owned())
    );
    assert_eq!(check_header_matches_workflow(&nfpm, &workflow), Ok(()));
    // The daemon's flag, which the coordinator's check does not take as its staging.
    let old = altered(
        &nfpm,
        "--coordinator-staged target/nfpm-coordinator`",
        "--staged target/nfpm-coordinator`",
    );
    let message = check_header_matches_workflow(&old, &workflow).expect_err("the daemon's flag");
    assert!(
        message.contains("the header names `--staged target/nfpm-coordinator`")
            && message.contains("runs `--coordinator-staged target/nfpm-coordinator`"),
        "{message}"
    );
    // A workflow step with another directory.
    let other = altered(
        &workflow,
        "$RPM_ARCH.rpm\" --coordinator-staged target/nfpm-coordinator",
        "$RPM_ARCH.rpm\" --coordinator-staged target/other",
    );
    assert!(check_header_matches_workflow(&nfpm, &other).is_err());
}
