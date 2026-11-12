//! The rules of `net-install.yml` ([`FILE`]), the workflow of the two network-install jobs, beyond
//! those every workflow keeps ([`super::lint`]): its triggers (a push to main that changes one of
//! [`PATHS`], one schedule, and by hand; never a pull request), its concurrency ([`GROUP`], a newer
//! run cancelling the older), exactly its two jobs ([`NET_JOBS`]); and, of each job, the one dpkg
//! step that installs QEMU and the firmware from the job's own pinned set (`.github/debs/<set>`,
//! whose four steps `debs.rs` holds), and the one step that fetches its ISO by its platform's pin
//! of `packaging/netboot/pins.toml`, from its URL or a mirror. The shape of each job (one build, one run as root, its own test
//! targets) is the shape every privileged job keeps (`privileged_tests.rs`). Each rule is a
//! function over the workflow (and the pins text) that returns its problems;
//! `net_install_tests.rs` shows each to refuse its defect.

use std::collections::BTreeSet;

use super::debs::install_run;
use super::lint::{entries, entry, no_inputs, scalars};
use super::yaml::{self, Node, Value};

/// The workflow of the network-install jobs. No release waits for it (`release.yml` names
/// `ci`, `privileged` and `artifacts`), and it never runs for a pull request.
pub const FILE: &str = "net-install.yml";

/// The paths of a push to main that runs [`FILE`], exactly and in this order: the network-install
/// code. The nightly run covers what they leave out.
pub const PATHS: &[&str] = &[
    ".github/workflows/net-install.yml",
    "crates/hostkeeper-pxe*/**",
    "packaging/netboot/**",
    "packaging/linux/**",
    "packaging/pxe/**",
    "crates/hostkeeper-privileged/tests/net_install*.rs",
    "crates/hostkeeper-privileged/tests/netinstall/**",
    "crates/hostkeeper-privileged/tests/bin/pxe.rs",
    "crates/xtask/src/release/netboot.rs",
    "crates/xtask/src/release/netboot/**",
    "crates/xtask/src/netinstall_report.rs",
    "crates/xtask/src/netinstall_report/**",
    ".github/debs/**",
    "crates/xtask/src/debs.rs",
    "crates/xtask/src/debs/**",
    "crates/xtask/src/mirrored.rs",
    "crates/xtask/src/mirrored/**",
    "crates/xtask/src/netboot_iso.rs",
    "crates/xtask/src/netboot_iso/**",
];

/// The concurrency group of [`FILE`]: one per branch, so on main one for the workflow.
pub const GROUP: &str = "net-install-${{ github.ref }}";

/// What [`FILE`] is refused with when its `on` is anything else.
pub const ON_PROBLEM: &str = "net-install.yml: on must be push to main with exactly the network-install paths, one schedule, and workflow_dispatch with no inputs";
/// What [`FILE`] is refused with when its `concurrency` is anything else.
pub const CONCURRENCY_PROBLEM: &str = "net-install.yml: concurrency must be the group net-install-${{ github.ref }}, cancel-in-progress: true";

/// The job of the x86-64 network-install end-to-end test.
pub const NET_INSTALL_JOB: &str = "net-install";
/// The job of the arm64 network-install test.
pub const NET_INSTALL_ARM64_JOB: &str = "net-install-arm64";

/// The first line of a privileged test file of [`NET_INSTALL_JOB`].
pub const NET_INSTALL_FIRST_LINE: &str =
    "#![cfg(all(target_os = \"linux\", target_arch = \"x86_64\", feature = \"privileged\"))]";
/// The first line of a privileged test file of [`NET_INSTALL_ARM64_JOB`].
pub const NET_INSTALL_ARM64_FIRST_LINE: &str =
    "#![cfg(all(target_os = \"linux\", feature = \"privileged\", not(target_arch = \"aarch64\")))]";

/// The pins file the ISO step reads.
pub const ISO_PINS: &str = "packaging/netboot/pins.toml";
/// The directory the ISO step writes the ISO into.
pub const ISO_DIR: &str = "target/net-install";

/// A network-install job: its name, the set of pinned packages its dpkg step installs
/// (`.github/debs/<set>.toml`), and the platform of pins.toml whose ISO it fetches.
pub struct NetJob {
    pub name: &'static str,
    pub set: &'static str,
    pub platform: &'static str,
}

/// The two network-install jobs.
pub const NET_JOBS: [NetJob; 2] = [
    NetJob {
        name: NET_INSTALL_JOB,
        set: "qemu-x86",
        platform: "linux-x86_64",
    },
    NetJob {
        name: NET_INSTALL_ARM64_JOB,
        set: "qemu-arm",
        platform: "linux-arm64",
    },
];

/// Whether `word` stands in `run` as a word of its own.
fn has_word(run: &str, word: &str) -> bool {
    run.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .any(|w| w == word)
}

/// The dpkg steps of one job, whose `runs` are in order and whose build step is at `build`: the
/// problems with them, and the position of the allowed one (it names one `sudo`, which the
/// one-sudo rule and the root-step count of the job leave out). `net` is the job's entry of
/// [`NET_JOBS`] when it is a network-install job, whose one dpkg step is the install of its own
/// set ([`install_run`]), before the build; any other job has none.
pub fn install_steps(
    runs: &[&str],
    build: Option<usize>,
    net: Option<&NetJob>,
) -> (Vec<String>, Vec<usize>) {
    let mut problems = Vec::new();
    let mut allowed = Vec::new();
    for (at, run) in runs
        .iter()
        .enumerate()
        .filter(|(_, run)| has_word(run, "dpkg"))
    {
        let run = run.trim_end();
        match net {
            None => problems.push(format!(
                "the dpkg step `{run}` is not allowed outside the jobs {NET_INSTALL_JOB} and {NET_INSTALL_ARM64_JOB}"
            )),
            Some(job) if run == install_run(job.set) => {
                allowed.push(at);
                if build.is_some_and(|build| at > build) {
                    problems.push(format!("the dpkg step `{run}` comes after the build step"));
                }
            }
            Some(job) => problems.push(format!(
                "the dpkg step `{run}` is not `{}`, the install of its own set",
                install_run(job.set)
            )),
        }
    }
    if let Some(job) = net
        && allowed.len() != 1
    {
        problems.push(format!(
            "{} installs of the pinned set {}, not one",
            allowed.len(),
            job.set
        ));
    }
    (problems, allowed)
}

/// What the ISO step of `platform` runs before its `--mirror` flags.
pub fn iso_run(platform: &str) -> String {
    format!("cargo xtask netboot-iso --pins {ISO_PINS} --platform {platform} --dir {ISO_DIR}")
}

/// What is wrong with the ISO of the network-install jobs of the workflow `text`, the pins being
/// `pins`: per job, its platform has an ISO in the pins, and exactly one step fetches it,
/// running [`iso_run`] of the platform and then one or more `--mirror <base>`, each an
/// `https://` base with no trailing `/`. That no mirror is Canonical's is the rule of `debs.rs`,
/// which refuses a host of Canonical's in any value of a workflow.
pub fn iso_problems(text: &str, pins: &str) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let pins: toml::Table = match pins.parse() {
        Ok(pins) => pins,
        Err(e) => return vec![format!("pins.toml does not parse: {e}")],
    };
    let mut problems = Vec::new();
    for net in &NET_JOBS {
        let name = net.name;
        let Some(job) = entry(&doc, "jobs").and_then(|jobs| entry(jobs, name)) else {
            problems.push(format!("no job {name}"));
            continue;
        };
        let pinned = pins
            .get(net.platform)
            .and_then(|platform| platform.get("iso"))
            .and_then(|iso| iso.get("url"))
            .is_some_and(toml::Value::is_str);
        if !pinned {
            problems.push(format!(
                "job {name}: pins.toml has no url in [{}.iso]",
                net.platform
            ));
        }
        let mut values = Vec::new();
        if let Some(steps) = entry(job, "steps") {
            scalars(steps, &mut values);
        }
        let fetches: Vec<&str> = values
            .iter()
            .map(|v| v.trim_end())
            .filter(|v| has_word(v, "netboot-iso"))
            .collect();
        let [run] = fetches[..] else {
            problems.push(format!(
                "job {name}: {} steps fetch the ISO, not one",
                fetches.len()
            ));
            continue;
        };
        let want = iso_run(net.platform);
        let shaped = run.strip_prefix(want.as_str()).is_some_and(|rest| {
            rest.starts_with(" --mirror ")
                && rest.split(" --mirror ").skip(1).all(|m| {
                    m.strip_prefix("https://")
                        .is_some_and(|host| !host.is_empty())
                        && !m.ends_with('/')
                        && !m.contains([' ', '?', '#', '@', '"', '\''])
                })
        });
        if !shaped {
            problems.push(format!(
                "job {name}: the ISO step `{run}` is not `{want}` and one or more `--mirror https://<base>`"
            ));
        }
    }
    problems
}

/// What is wrong with the shape of [`FILE`], the document `doc`: its `on`, its `concurrency`, and
/// a job that is not one of [`NET_JOBS`] or one of them missing.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    if !push_schedule_and_dispatch(entry(doc, "on")) {
        problems.push(ON_PROBLEM.to_owned());
    }
    if !one_group_that_cancels(entry(doc, "concurrency")) {
        problems.push(CONCURRENCY_PROBLEM.to_owned());
    }
    let jobs = entry(doc, "jobs").map_or(&[][..], entries);
    let named: BTreeSet<&str> = jobs.iter().filter_map(|(name, _)| name.as_str()).collect();
    for wanted in NET_JOBS.iter().filter(|j| !named.contains(j.name)) {
        problems.push(format!("no job {}", wanted.name));
    }
    for name in named
        .iter()
        .filter(|name| !NET_JOBS.iter().any(|j| j.name == **name))
    {
        problems.push(format!("job {name} is not a job of {FILE}"));
    }
    problems
}

/// The scalars of the list `node`, or `None` when it is not a list of scalars.
fn strings(node: &Node) -> Option<Vec<&str>> {
    match &node.value {
        Value::Seq(items) => items.iter().map(Node::as_str).collect(),
        _ => None,
    }
}

/// Whether `on` is exactly, as a mapping: `push` with `branches: [main]` and `paths:` [`PATHS`] and
/// no other filter; `schedule` with one entry, of one `cron`; and `workflow_dispatch` with no
/// inputs. No other trigger, so none that runs for a pull request.
fn push_schedule_and_dispatch(on: Option<&Node>) -> bool {
    let Some(Value::Map(triggers)) = on.map(|on| &on.value) else {
        return false;
    };
    let push = |push: &Node| {
        let keys: BTreeSet<&str> = entries(push)
            .iter()
            .filter_map(|(k, _)| k.as_str())
            .collect();
        keys == BTreeSet::from(["branches", "paths"])
            && entries(push).len() == 2
            && entry(push, "branches").and_then(strings) == Some(vec!["main"])
            && entry(push, "paths").and_then(strings).as_deref() == Some(PATHS)
    };
    let schedule = |schedule: &Node| match &schedule.value {
        Value::Seq(items) => match &items[..] {
            [only] => {
                matches!(entries(only), [(key, cron)] if key.as_str() == Some("cron") && cron.as_str().is_some())
            }
            _ => false,
        },
        _ => false,
    };
    let has = |name: &str, holds: &dyn Fn(&Node) -> bool| {
        triggers
            .iter()
            .any(|(k, v)| k.as_str() == Some(name) && holds(v))
    };
    triggers.len() == 3
        && has("push", &push)
        && has("schedule", &schedule)
        && has("workflow_dispatch", &no_inputs)
}

/// Whether `concurrency` is exactly [`GROUP`] with `cancel-in-progress: true`.
fn one_group_that_cancels(concurrency: Option<&Node>) -> bool {
    let Some(concurrency) = concurrency else {
        return false;
    };
    let keys: Vec<&str> = entries(concurrency)
        .iter()
        .filter_map(|(k, _)| k.as_str())
        .collect();
    keys.len() == 2
        && keys.contains(&"group")
        && keys.contains(&"cancel-in-progress")
        && entry(concurrency, "group").and_then(Node::as_str) == Some(GROUP)
        && entry(concurrency, "cancel-in-progress").and_then(Node::as_str) == Some("true")
}
