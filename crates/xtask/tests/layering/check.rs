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
const TESTKIT: &str = "hostkeeper-testkit";
const WIRE: &str = "hostkeeper-wire";
const OIDC: &str = "hostkeeper-oidc";
const TLS: &str = "hostkeeper-tls";
const COORDINATOR: &str = "hostkeeper-coordinator";
const UNPACK: &str = "hostkeeper-unpack";
const ENROLLMENT: &str = "hostkeeper-enrollment";
const MDM_GATE: &str = "hostkeeper-mdm-gate";
const OBSERVE: &str = "hostkeeper-observe";
const LAUNCHD: &str = "hostkeeper-launchd";
const OPENRC: &str = "hostkeeper-openrc";
const MDM: &str = "hostkeeper-mdm";
const MDM_STEP: &str = "hostkeeper-mdm-step";
const PXE: &str = "hostkeeper-pxe";
const PXE_PACKETS: &str = "hostkeeper-pxe-packets";
const PROTO: &str = "hostkeeper-proto";
const HKCTL: &str = "hostkeeper-hkctl";
const DAEMON: &str = "hostkeeper-daemon";
const ENROLL: &str = "hostkeeper-enroll";
const LINK: &str = "hostkeeper-link";
const MDM_CORE: &str = "hostkeeper-mdm-core";
const PACKAGE: &str = "hostkeeper-package";
const PLACED: &str = "hostkeeper-placed";
const PRIVILEGED: &str = "hostkeeper-privileged";
const PXE_SEED: &str = "hostkeeper-pxe-seed";
const RELEASE: &str = "hostkeeper-release";
const UI_TESTS: &str = "hostkeeper-ui-tests";
const BINARY: &str = "hostkeeper";
const HELLO: &str = "hello";

/// One row of the layers: a crate, the workspace crates its normal and build dependencies must
/// be exactly, and the workspace crates its dev-dependencies may hold besides itself and the
/// testkit.
pub type Layer = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// Every crate of the workspace but those of [`NO_ROW`], in name order. The dev column is what
/// each crate's tests use: the coordinator's tests turn on `hostkeeper-oidc`'s `testkit` feature
/// (a fake issuer), which a dev-dependency can do and a normal dependency must not; the daemon's and the link's tests run a
/// coordinator in process, behind a listener's TLS (`hostkeeper-tls`), and the daemon's run the
/// example service hello's hook in process;
/// hello's tests read its output with the hook contract and load its package with the
/// manifest loader; `hkctl`'s tests forward the script's calls to a coordinator built from its
/// flags, whose CI allow-list is `hostkeeper-oidc`'s testkit issuer's; the observer's tests run
/// hooks through the step runner; the CLI crates' tests read the protobuf descriptors, and the
/// web app's send each of its calls to a coordinator built from its flags, whose CI allow-list
/// is `hostkeeper-oidc`'s testkit issuer's (the web app's crate generates its API client from
/// the protos' command table and descriptors, a normal dependency); the privileged tests drive the
/// step runner's launcher and dead-man and the observer's adapters against the host, and on a
/// Mac start the daemon (their program `hostkeeper-device-daemon`, which needs the daemon as a
/// normal dependency), the launchd adapters and the package check, and
/// read the Mac's enrollment claims (`hostkeeper-enroll`'s `MacClaims`) from the real `ioreg`;
/// their program `hostkeeper-selfupdate-daemon` runs the daemon on the Linux adapters with a
/// package store and a heartbeat link of its own, so it needs the config, the launchd crate's
/// platform, the link, the placement roots and the step crate as normal dependencies; and the
/// first-boot tests start the coordinator and the daemon as child processes (the coordinator is
/// their program `hostkeeper-privileged-coordinator`, which needs it as a normal dependency); the
/// end-to-end tests run the coordinator in process with its fake issuer's config, and fetch and
/// check the fixture's attestation with the package crate; the Mac's self-update test answers
/// the heartbeats its daemon writes with the coordinator in process, read with the wire crate; the Mac's end-to-end test packages
/// hello with their program `hello-e2e`, which runs hello's hook and server and so needs hello
/// as a normal dependency; and the network-install tests run `hostkeeper-pxe` (their program
/// `hostkeeper-privileged-pxe`, which needs it as a normal dependency) and read a DHCP client's
/// packet with its packet crate; the `mdm` package's certificate-unit test loads that package's
/// units and drop-ins (`hostkeeper-mdm-core`) into the host's systemd. The binary's tests run
/// the macOS pkg's postinstall (`hostkeeper-launchd`) against the binary.
pub const LAYERS: [Layer; 31] = [
    (BINARY, &[DAEMON], &[LAUNCHD]),
    (CONFIG, &[], &[]),
    (
        COORDINATOR,
        &[
            CONFIG, ENROLLMENT, MDM_GATE, OIDC, PROTO, RELEASE, TLS, WIRE,
        ],
        &[OIDC],
    ),
    (
        DAEMON,
        &[
            CONFIG, ENROLL, HOOK, LAUNCHD, LINK, OBSERVE, OPENRC, PACKAGE, PLACED, PROTO, RELEASE,
            STEPS, UNPACK,
        ],
        &[COORDINATOR, HELLO, OIDC, TLS, WIRE],
    ),
    (ENROLL, &[CONFIG, PROTO, STEPS], &[COORDINATOR, ENROLLMENT]),
    (ENROLLMENT, &[], &[]),
    (HELLO, &[], &[CONFIG, HOOK]),
    (HKCTL, &[PROTO], &[COORDINATOR, OIDC]),
    (HOOK, &[], &[]),
    (LAUNCHD, &[HOOK, OBSERVE, PLACED, STEPS], &[]),
    (
        LINK,
        &[CONFIG, HOOK, PROTO, STEPS, WIRE],
        &[COORDINATOR, ENROLL, OIDC, TLS],
    ),
    (MDM, &[HOOK, MDM_CORE, MDM_STEP], &[]),
    (MDM_CORE, &[], &[]),
    (MDM_GATE, &[CONFIG, ENROLLMENT, WIRE], &[]),
    (MDM_STEP, &[], &[]),
    (OBSERVE, &[CONFIG, PROTO], &[HOOK, STEPS]),
    (OIDC, &[], &[]),
    (OPENRC, &[OBSERVE, PLACED, STEPS], &[HOOK]),
    (PACKAGE, &[CONFIG, HOOK, PROTO, RELEASE, STEPS], &[]),
    (PLACED, &[CONFIG], &[]),
    (
        PRIVILEGED,
        &[
            CONFIG,
            COORDINATOR,
            DAEMON,
            HELLO,
            HOOK,
            LAUNCHD,
            LINK,
            PLACED,
            PROTO,
            PXE,
            STEPS,
        ],
        &[
            ENROLL,
            MDM_CORE,
            OBSERVE,
            OIDC,
            PACKAGE,
            PXE_PACKETS,
            UNPACK,
            WIRE,
        ],
    ),
    (PROTO, &[], &[]),
    (
        PXE,
        &[
            CONFIG,
            ENROLLMENT,
            PACKAGE,
            PROTO,
            PXE_PACKETS,
            PXE_SEED,
            STEPS,
            WIRE,
        ],
        &[],
    ),
    (PXE_PACKETS, &[WIRE], &[]),
    (PXE_SEED, &[CONFIG, ENROLLMENT, WIRE], &[]),
    (RELEASE, &[CONFIG, PROTO], &[]),
    (STEPS, &[CONFIG, HOOK], &[]),
    (TLS, &[], &[]),
    (UI_TESTS, &[PROTO], &[COORDINATOR, OIDC]),
    (UNPACK, &[CONFIG, HOOK, STEPS], &[]),
    (WIRE, &[], &[]),
];

/// The crates of the workspace that have no row in [`LAYERS`]: the tooling, which no crate
/// depends on, and the testkit, which has rules of its own.
pub const NO_ROW: [&str; 3] = ["coverage-gate", "xtask", TESTKIT];

/// Pairs (crate, crate it must not reach): the first never depends on the second through the
/// rows of [`LAYERS`], directly or through any chain of rows. Each row is exact, so a crate
/// reaches through its manifests what its rows reach.
pub const NOT_ABOVE: [(&str, &str); 2] = [(STEPS, LAUNCHD), (OBSERVE, LAUNCHD)];

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
    members_of(&value)
}

/// The packages of `cargo metadata --no-deps` JSON with their dependencies.
fn members_of(value: &serde_json::Value) -> Members {
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

/// Whether `name` is the name of a crate of this product: `hostkeeper` or `hostkeeper-*`.
fn is_ours(name: &str) -> bool {
    name == BINARY || name.starts_with("hostkeeper-")
}

/// Every rule the workspace breaks, one line each: first the kbf names (all of `lock`), then
/// the crates of this product that are not members (all of `lock`), then the crates in the
/// order of `LAYERS`, then the testkit.
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
    for name in lock
        .keys()
        .filter(|name| is_ours(name.as_str()) && !members.contains_key(name.as_str()))
    {
        let chain = chain_to(lock, &member_names, name);
        out.push(format!(
            "{name} is not a workspace member, reached through {}",
            chain.join(" -> ")
        ));
    }
    for (name, exact, dev_allowed) in LAYERS {
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
            if dev != name && dev != TESTKIT && !dev_allowed.contains(&dev) {
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

/// Checks that `table` has a row for exactly the crates of the workspace whose `cargo metadata
/// --no-deps` JSON is `metadata`, other than those of [`NO_ROW`], and that the dev column of
/// each row is the set of workspace crates its dev-dependencies name (itself and the testkit
/// left out). `Err` holds one line per crate with no row, row with no crate, and row whose dev
/// column is not what its crate has; [`check`] holds the normal and build column.
pub fn rows_cover(table: &[Layer], metadata: &serde_json::Value) -> Result<(), String> {
    let members = members_of(metadata);
    let rows: BTreeSet<&str> = table.iter().map(|(name, _, _)| *name).collect();
    let mut out = Vec::new();
    for name in members
        .keys()
        .map(String::as_str)
        .filter(|name| !NO_ROW.contains(name) && !rows.contains(name))
    {
        out.push(format!(
            "{name} is a crate of the workspace with no row in LAYERS"
        ));
    }
    for (name, _, dev_allowed) in table {
        if !members.contains_key(*name) {
            out.push(format!(
                "LAYERS has a row for {name}, which is not a crate of the workspace"
            ));
            continue;
        }
        let actual: BTreeSet<&str> = workspace_deps(&members, name, |kind| kind == Kind::Dev)
            .into_iter()
            .filter(|dev| *dev != *name && *dev != TESTKIT)
            .collect();
        let allowed: BTreeSet<&str> = dev_allowed.iter().copied().collect();
        if actual != allowed {
            out.push(format!(
                "{name} has dev-dependencies on the workspace crates {}, \
                 but its row in LAYERS names {}",
                set(&actual),
                set(&allowed)
            ));
        }
    }
    if out.is_empty() {
        Ok(())
    } else {
        Err(out.join("\n"))
    }
}

/// The shortest chain of rows of `table` from `from` to `to` (normal and build column), both
/// ends included; `None` when the rows never lead there. `from` and `to` differ.
pub fn reaches_in(table: &[Layer], from: &str, to: &str) -> Option<Vec<String>> {
    let edges: Edges = table
        .iter()
        .map(|(name, exact, _)| {
            (
                name.to_string(),
                exact.iter().map(|dep| dep.to_string()).collect(),
            )
        })
        .collect();
    shortest(&edges, from, to)
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

/// Whether the role-enum scan leaves the file out: it holds test code.
fn is_test_file(name: &str) -> bool {
    name == "tests.rs" || name == "testutil.rs" || name.ends_with("_tests.rs")
}

/// Whether `c` can be part of an identifier.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `text` with every comment (line, and block comments, nested), string literal (plain, byte
/// and raw) and character literal replaced by spaces. Newlines are kept, so a line number of
/// the result is the line number of `text`. A lifetime such as `'static` is code and stays.
fn mask_non_code(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let blank = |out: &mut String, c: char| out.push(if c == '\n' { '\n' } else { ' ' });
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let after_ident = i > 0 && is_ident(chars[i - 1]);
        let raw_from = match (c, at(i + 1)) {
            ('r', _) if !after_ident => Some(i + 1),
            ('b', 'r') if !after_ident => Some(i + 2),
            _ => None,
        };
        let hashes = raw_from.map_or(0, |from| (from..).take_while(|j| at(*j) == '#').count());
        let raw_quote = raw_from.map(|from| from + hashes).filter(|q| at(*q) == '"');
        // The index one past the end of the span to blank, if `i` starts one.
        let end = if c == '/' && at(i + 1) == '/' {
            Some(
                (i..chars.len())
                    .find(|j| chars[*j] == '\n')
                    .unwrap_or(chars.len()),
            )
        } else if c == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            let mut j = i;
            while j < chars.len() {
                if at(j) == '/' && at(j + 1) == '*' {
                    depth += 1;
                    j += 2;
                } else if at(j) == '*' && at(j + 1) == '/' {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            Some(j)
        } else if let Some(quote) = raw_quote {
            let closing: String = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            let body: String = chars[quote + 1..].iter().collect();
            Some(body.find(&closing).map_or(chars.len(), |at| {
                quote + 1 + body[..at].chars().count() + closing.len()
            }))
        } else if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            Some((j + 1).min(chars.len()))
        } else if c == '\'' && at(i + 1) == '\\' {
            let close = (i + 3..chars.len()).find(|j| chars[*j] == '\'');
            Some(close.map_or(chars.len(), |j| j + 1))
        } else if c == '\'' && at(i + 2) == '\'' {
            Some(i + 3)
        } else {
            None
        };
        match end {
            Some(end) => {
                for blanked in &chars[i..end.min(chars.len())] {
                    blank(&mut out, *blanked);
                }
                i = end;
            }
            None => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The words of `code` (runs of identifier characters) with the byte offset each starts at.
fn words_of(code: &str) -> Vec<(usize, &str)> {
    let mut words = Vec::new();
    let mut start = None;
    for (at, c) in code
        .char_indices()
        .chain(std::iter::once((code.len(), ' ')))
    {
        match (start, is_ident(c)) {
            (None, true) => start = Some(at),
            (Some(from), false) => {
                words.push((from, &code[from..at]));
                start = None;
            }
            _ => {}
        }
    }
    words
}

/// The type of a `const` or `static` item: `rest`, which follows the `:`, up to the `=` (or
/// `;`, `,` or closing bracket) that ends it. The `;` of an array type is inside brackets.
fn declared_type(rest: &str) -> &str {
    let mut depth = 0_i32;
    let mut previous = ' ';
    for (at, c) in rest.char_indices() {
        let ends = match c {
            '(' | '[' | '<' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth -= 1;
                depth < 0
            }
            '>' if previous != '-' => {
                depth -= 1;
                depth < 0
            }
            '=' | '{' => true,
            ';' | ',' => depth <= 0,
            _ => false,
        };
        if ends {
            return &rest[..at];
        }
        previous = c;
    }
    rest
}

/// Whether the type that begins `returned` (after a `->`) is an array of `RoleName`, possibly
/// behind `&`, a lifetime or `mut`, and the name possibly qualified by a path.
fn returns_role_array(returned: &str) -> bool {
    let mut rest = returned;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('&') {
            rest = after;
        } else if let Some(after) = rest.strip_prefix('\'') {
            rest = after.trim_start_matches(is_ident);
        } else if let Some(after) = rest.strip_prefix("mut ") {
            rest = after;
        } else {
            break;
        }
    }
    rest.strip_prefix('[').is_some_and(|inside| {
        let path: String = inside
            .chars()
            .take_while(|c| is_ident(*c) || *c == ':' || c.is_whitespace())
            .collect();
        path.trim().rsplit("::").next().map(str::trim) == Some("RoleName")
    })
}

/// What the code of one file says that brings the closed set of roles back: `(line, what)`, a
/// line once, in line order. `code` is the file with comments and literals masked
/// ([`mask_non_code`]), so a declaration is matched across the lines it spans and is reported at
/// the line it starts on. The things looked for: the identifier `HostRole`, an `enum` whose
/// name ends in `Role`, a `const` or `static` of a type naming `RoleName` (not `const fn`, not a
/// `'static` lifetime), and a function returning an array of `RoleName`, the name possibly
/// qualified by a path.
fn closed_roles_in(code: &str) -> Vec<(usize, String)> {
    let line_of = |offset: usize| code[..offset].matches('\n').count() + 1;
    let words = words_of(code);
    let mut found: Vec<(usize, u8, String)> = Vec::new();
    for (index, (offset, word)) in words.iter().enumerate() {
        let next = words.get(index + 1).map(|(_, next)| *next);
        match *word {
            "HostRole" => found.push((line_of(*offset), 0, "the identifier HostRole".to_string())),
            "enum" => {
                if let Some(name) = next.filter(|name| name.ends_with("Role")) {
                    found.push((
                        line_of(*offset),
                        1,
                        format!("the enum {name}, a closed set of roles"),
                    ));
                }
            }
            "const" | "static" if !code[..*offset].ends_with('\'') => {
                let name_at = if next == Some("mut") {
                    index + 2
                } else {
                    index + 1
                };
                if let Some((name_offset, name)) = words.get(name_at)
                    && !matches!(*name, "fn" | "unsafe" | "async" | "extern")
                {
                    let after = &code[name_offset + name.len()..];
                    if let Some(rest) = after.trim_start().strip_prefix(':')
                        && !rest.starts_with(':')
                        && words_of(declared_type(rest))
                            .iter()
                            .any(|(_, word)| *word == "RoleName")
                    {
                        found.push((
                            line_of(*offset),
                            2,
                            "a const or static of type RoleName".to_string(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    for (arrow, _) in code.match_indices("->") {
        if returns_role_array(&code[arrow + 2..]) {
            // The declaration starts at the nearest `fn` before the arrow.
            let start = words
                .iter()
                .rev()
                .find(|(offset, word)| *offset < arrow && *word == "fn")
                .map_or(arrow, |(offset, _)| *offset);
            found.push((
                line_of(start),
                3,
                "a function returning an array of RoleName".to_string(),
            ));
        }
    }
    found.sort();
    found.dedup_by_key(|(line, _, _)| *line);
    found
        .into_iter()
        .map(|(line, _, what)| (line, what))
        .collect()
}

/// The sorted paths directly under `dir`; an unreadable `dir` panics.
fn sorted_entries(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("read a directory entry").path())
        .collect();
    paths.sort();
    paths
}

/// Scans the `.rs` files under `dir`, skipping test files and directories named `tests`.
fn scan_role_dir(root: &Path, dir: &Path, scanned: &mut usize, found: &mut Vec<String>) {
    for path in sorted_entries(dir) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        if path.is_dir() {
            if name != "tests" {
                scan_role_dir(root, &path, scanned, found);
            }
        } else if name.ends_with(".rs") && !is_test_file(&name) {
            *scanned += 1;
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let masked = mask_non_code(&text);
            // The lines of `#[cfg(test)]` items are blanked before the scan.
            let mut skipping = false;
            let mut code = String::with_capacity(masked.len());
            for (raw, line) in text.lines().zip(masked.lines()) {
                if skipping {
                    // A `#[cfg(test)]` item at column 0 ends at the first column-0 line that is
                    // `}` or ends in `;`.
                    let top_level = !raw.starts_with(char::is_whitespace);
                    let ends = raw == "}" || (top_level && raw.ends_with(';'));
                    skipping = !ends;
                } else if raw == "#[cfg(test)]" {
                    skipping = true;
                } else {
                    code.push_str(line);
                }
                code.push('\n');
            }
            for (line, what) in closed_roles_in(&code) {
                found.push(format!("{relative}:{line}: {what}"));
            }
        }
    }
}

/// Scans every non-test `.rs` file under `root/crates/*/src` for the closed set of roles coming
/// back (see `closed_roles_in`). Returns the number of files scanned and one line per finding,
/// `<file>:<line>: <what>`, in path order. Test files (`tests.rs`, `testutil.rs`, `*_tests.rs`),
/// directories named `tests` and `#[cfg(test)]` items at column 0 are left out; comments, string
/// and character literals are not code, and a declaration may span lines.
pub fn closed_role_enums(root: &Path) -> (usize, Vec<String>) {
    let mut scanned = 0;
    let mut found = Vec::new();
    for krate in sorted_entries(&root.join("crates")) {
        let src = krate.join("src");
        if src.is_dir() {
            scan_role_dir(root, &src, &mut scanned, &mut found);
        }
    }
    (scanned, found)
}
