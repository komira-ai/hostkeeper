//! The layering rules, read from a workspace directory.
//!
//! The members and their direct dependencies come from `cargo metadata --no-deps --offline`;
//! the whole graph (every kind, every platform) comes from the workspace's `Cargo.lock`. A
//! layer crate's normal and build dependencies are followed through crates outside the
//! workspace (a path crate under an excluded directory), so such a crate cannot hide a
//! workspace crate behind it.
//! Nothing here touches the network.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::process::Command;

const HOOK: &str = "hostkeeper-hook";
const CONFIG: &str = "hostkeeper-config";
const STEPS: &str = "hostkeeper-steps";
const RIGHTS: &str = "hostkeeper-rights";
const TESTKIT: &str = "hostkeeper-testkit";
const UNPACK: &str = "hostkeeper-unpack";

/// Per layer crate: the workspace crates its normal and build dependencies must be exactly.
const LAYERS: [(&str, &[&str]); 5] = [
    (HOOK, &[]),
    (CONFIG, &[]),
    (STEPS, &[CONFIG, HOOK]),
    (RIGHTS, &[CONFIG, HOOK]),
    (UNPACK, &[CONFIG, HOOK, STEPS]),
];

/// A package name to the names of the packages it depends on.
pub type Edges = BTreeMap<String, BTreeSet<String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Normal,
    Dev,
    Build,
}

struct Dep {
    name: String,
    kind: Kind,
}

type Members = BTreeMap<String, Vec<Dep>>;

/// Runs `cargo <args> --manifest-path <manifest>` and returns its stdout. A command that fails
/// panics with its stderr.
pub fn cargo(args: &[&str], manifest: &Path) -> String {
    let out = Command::new(env!("CARGO"))
        .args(args)
        .arg("--manifest-path")
        .arg(manifest)
        .output()
        .unwrap_or_else(|e| panic!("cannot run cargo: {e}"));
    assert!(
        out.status.success(),
        "cargo {} failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("cargo printed UTF-8")
}

/// Checks the workspace in `root` (a directory holding `Cargo.toml` and `Cargo.lock`). `Err`
/// holds one line per violation.
pub fn check(root: &Path) -> Result<(), String> {
    let lock = read_lock(root);
    let metadata = cargo(
        &[
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ],
        &root.join("Cargo.toml"),
    );
    let found = violations(&parse_members(&metadata), &lock);
    if found.is_empty() {
        Ok(())
    } else {
        Err(found.join("\n"))
    }
}

/// The package graph of `root/Cargo.lock`.
pub fn read_lock(root: &Path) -> Edges {
    let path = root.join("Cargo.lock");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    parse_lock(&text)
}

/// Reads the `[[package]]` blocks of a `Cargo.lock` (format 4): each block's `name` and the
/// names in its `dependencies` list (an entry is `"name"`, `"name version"` or
/// `"name version (source)"`; only the name is kept).
pub fn parse_lock(text: &str) -> Edges {
    let mut edges = Edges::new();
    let mut in_package = false;
    let mut in_deps = false;
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with('[') {
            in_package = line == "[[package]]";
            in_deps = false;
            current = None;
        } else if in_deps {
            if line == "]" {
                in_deps = false;
            } else if let Some(owner) = &current {
                let entry = line.trim().trim_end_matches(',').trim_matches('"');
                if let Some(name) = entry.split_whitespace().next() {
                    edges
                        .entry(owner.clone())
                        .or_default()
                        .insert(name.to_string());
                }
            }
        } else if in_package
            && current.is_none()
            && let Some(rest) = line.strip_prefix("name = ")
        {
            let name = rest.trim_matches('"').to_string();
            edges.entry(name.clone()).or_default();
            current = Some(name);
        } else if in_package && line == "dependencies = [" {
            in_deps = true;
        }
    }
    edges
}

fn parse_members(json: &str) -> Members {
    let value: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("cargo metadata printed no JSON: {e}"));
    let mut members = Members::new();
    for package in value["packages"]
        .as_array()
        .expect("metadata lists packages")
    {
        let deps = package["dependencies"]
            .as_array()
            .expect("a package lists its dependencies")
            .iter()
            .map(|dep| Dep {
                name: dep["name"]
                    .as_str()
                    .expect("a dependency has a name")
                    .to_string(),
                kind: match dep["kind"].as_str() {
                    None => Kind::Normal,
                    Some("dev") => Kind::Dev,
                    Some("build") => Kind::Build,
                    Some(other) => panic!("unknown dependency kind {other}"),
                },
            })
            .collect();
        let name = package["name"].as_str().expect("a package has a name");
        members.insert(name.to_string(), deps);
    }
    members
}

fn is_kbf(name: &str) -> bool {
    name == "kbf" || name.starts_with("kbf-") || name.starts_with("kbf_")
}

/// The names of the workspace crates `name` depends on, among the kinds `wanted` accepts.
fn workspace_deps<'a>(
    members: &'a Members,
    name: &str,
    wanted: impl Fn(Kind) -> bool,
) -> BTreeSet<&'a str> {
    members
        .get(name)
        .into_iter()
        .flatten()
        .filter(|dep| wanted(dep.kind) && members.contains_key(&dep.name))
        .map(|dep| dep.name.as_str())
        .collect()
}

/// The workspace crates `name` reaches through its normal and build dependencies, each with the
/// shortest chain from `name` to it. The walk starts at `name`'s direct dependencies and
/// continues through crates that are not workspace members (their lockfile edges); it stops at
/// a member, whose own dependencies are judged by its own rules.
fn reached(members: &Members, lock: &Edges, name: &str) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::from([name.to_string()]);
    let mut queue: VecDeque<Vec<String>> = VecDeque::from([vec![name.to_string()]]);
    while let Some(chain) = queue.pop_front() {
        let node = chain.last().expect("a chain is never empty");
        let next: Vec<&str> = if chain.len() == 1 {
            members
                .get(name)
                .into_iter()
                .flatten()
                .filter(|dep| dep.kind != Kind::Dev)
                .map(|dep| dep.name.as_str())
                .collect()
        } else {
            lock.get(node)
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect()
        };
        for dep in next {
            let mut longer = chain.clone();
            longer.push(dep.to_string());
            if members.contains_key(dep) {
                if dep != name {
                    found.entry(dep.to_string()).or_insert(longer);
                }
            } else if seen.insert(dep.to_string()) {
                queue.push_back(longer);
            }
        }
    }
    found
}

fn set(names: &BTreeSet<&str>) -> String {
    format!(
        "{{{}}}",
        names.iter().copied().collect::<Vec<_>>().join(", ")
    )
}

/// The shortest path in `edges` from `from` to `to`, both ends included.
fn shortest<'a>(edges: &'a Edges, from: &'a str, to: &str) -> Option<Vec<String>> {
    let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::from([from]);
    let mut queue = VecDeque::from([from]);
    while let Some(node) = queue.pop_front() {
        if node == to {
            let mut path = vec![node.to_string()];
            let mut at = node;
            while let Some(&before) = parent.get(at) {
                path.push(before.to_string());
                at = before;
            }
            path.reverse();
            return Some(path);
        }
        for next in edges.get(node).into_iter().flatten() {
            if seen.insert(next.as_str()) {
                parent.insert(next.as_str(), node);
                queue.push_back(next.as_str());
            }
        }
    }
    None
}

/// The shortest chain from a member (other than `target`) to `target`; `target` alone when no
/// member reaches it.
fn chain_to(edges: &Edges, members: &[&str], target: &str) -> Vec<String> {
    let mut best: Option<Vec<String>> = None;
    for start in members.iter().filter(|member| **member != target) {
        if let Some(path) = shortest(edges, start, target)
            && best.as_ref().is_none_or(|b| path.len() < b.len())
        {
            best = Some(path);
        }
    }
    best.unwrap_or_else(|| vec![target.to_string()])
}

/// Every rule the workspace breaks, one line each: first the kbf names (all of `lock`), then
/// the hook, config, steps and rights crates, then the testkit.
fn violations(members: &Members, lock: &Edges) -> Vec<String> {
    let mut out = Vec::new();
    let member_names: Vec<&str> = members.keys().map(String::as_str).collect();
    for name in lock.keys().filter(|name| is_kbf(name.as_str())) {
        let chain = chain_to(lock, &member_names, name);
        out.push(format!(
            "{name} is a kbf package (kbf, kbf-* or kbf_*), reached through {}",
            chain.join(" -> ")
        ));
    }
    for (name, exact) in LAYERS {
        if !members.contains_key(name) {
            out.push(format!("{name} is not a workspace member"));
            continue;
        }
        let chains = reached(members, lock, name);
        let actual: BTreeSet<&str> = chains.keys().map(String::as_str).collect();
        let wanted: BTreeSet<&str> = exact.iter().copied().collect();
        if actual != wanted {
            out.push(format!(
                "{name} depends (normal or build) on the workspace crates {}, \
                 but must depend on exactly {}",
                set(&actual),
                set(&wanted)
            ));
            for (dep, chain) in &chains {
                if chain.len() > 2 && !wanted.contains(dep.as_str()) {
                    out.push(format!(
                        "{name} reaches the workspace crate {dep} through {}",
                        chain.join(" -> ")
                    ));
                }
            }
        }
        for dev in workspace_deps(members, name, |kind| kind == Kind::Dev) {
            if dev != name && dev != TESTKIT {
                out.push(format!(
                    "{name} has a dev-dependency on the workspace crate {dev}, \
                     which is neither itself nor {TESTKIT}"
                ));
            }
        }
    }
    if !members.contains_key(TESTKIT) {
        out.push(format!("{TESTKIT} is not a workspace member"));
        return out;
    }
    for dep in workspace_deps(members, TESTKIT, |_| true) {
        out.push(format!("{TESTKIT} depends on the workspace crate {dep}"));
    }
    for name in members.keys().filter(|name| name.as_str() != TESTKIT) {
        if workspace_deps(members, name, |kind| kind != Kind::Dev).contains(TESTKIT) {
            out.push(format!(
                "{name} has a normal or build dependency on {TESTKIT}, \
                 which is a dev-dependency only"
            ));
        }
    }
    out
}

/// The normal dependencies of `hostkeeper-hook` that are not optional.
const HOOK_PLAIN_DEPS: [&str; 4] = ["serde", "serde_json", "thiserror", "toml"];

/// Checks the `hostkeeper-hook` package of `cargo metadata --no-deps` JSON: the harness
/// dependency `rustix` is optional and only the feature `conformance` turns it on, and no
/// other normal dependency is non-optional beyond `HOOK_PLAIN_DEPS`. `Err` holds one line per
/// violation.
pub fn check_hook_harness(metadata: &serde_json::Value) -> Result<(), String> {
    let Some(package) = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|package| package["name"] == HOOK)
    else {
        return Err(format!("{HOOK} is not in the metadata"));
    };
    let normal: Vec<&serde_json::Value> = package["dependencies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|dep| dep["kind"].is_null())
        .collect();
    let mut out = Vec::new();
    if !normal
        .iter()
        .any(|dep| dep["name"] == "rustix" && dep["optional"] == true)
    {
        out.push(format!(
            "{HOOK} must have rustix as an optional normal dependency"
        ));
    }
    if package["features"]["conformance"] != serde_json::json!(["dep:rustix"]) {
        out.push(format!(
            "{HOOK} must have the feature conformance = [\"dep:rustix\"]"
        ));
    }
    let plain: BTreeSet<&str> = normal
        .iter()
        .filter(|dep| dep["optional"] != true)
        .filter_map(|dep| dep["name"].as_str())
        .filter(|name| !HOOK_PLAIN_DEPS.contains(name))
        .collect();
    if !plain.is_empty() {
        out.push(format!(
            "{HOOK} has non-optional normal dependencies {} beyond {}",
            set(&plain),
            HOOK_PLAIN_DEPS.join(", ")
        ));
    }
    if out.is_empty() {
        Ok(())
    } else {
        Err(out.join("\n"))
    }
}

/// The `cargo metadata --no-deps --offline` JSON of the workspace in `root`.
pub fn metadata_of(root: &Path) -> serde_json::Value {
    let text = cargo(
        &[
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ],
        &root.join("Cargo.toml"),
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("cargo metadata printed no JSON: {e}"))
}
