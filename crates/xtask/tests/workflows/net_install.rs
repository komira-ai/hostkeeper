//! The rules of `net-install.yml` ([`FILE`]), the workflow of the two network-install jobs, beyond
//! those every workflow keeps ([`super::lint`]): its triggers (a push to main that changes one of
//! [`PATHS`], one schedule, and by hand; never a pull request), its concurrency ([`GROUP`], a newer
//! run cancelling the older), exactly its two jobs ([`NET_JOBS`]); and, of each job, the apt steps
//! that install QEMU and the firmware and the ISO it downloads, held to the pin of
//! `packaging/netboot/pins.toml`. The shape of each job (one build, one run as root, its own test
//! targets) is the shape every privileged job keeps (`privileged_tests.rs`). Each rule is a
//! function over the workflow (and the pins text) that returns its problems;
//! `net_install_tests.rs` shows each to refuse its defect.

use std::collections::BTreeSet;

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

/// The one apt step that refreshes the package lists.
pub const APT_UPDATE: &str = "sudo apt-get update";
/// What the one apt install step starts with; the package names follow.
pub const APT_INSTALL: &str = "sudo apt-get install --yes --no-install-recommends ";

/// A network-install job: its name, the packages its apt install step may name, and the table of
/// pins.toml whose `url` its `ISO_URL` equals.
pub struct NetJob {
    pub name: &'static str,
    pub packages: &'static [&'static str],
    pub platform: &'static str,
}

/// The two network-install jobs.
pub const NET_JOBS: [NetJob; 2] = [
    NetJob {
        name: NET_INSTALL_JOB,
        packages: &["qemu-system-x86", "ovmf"],
        platform: "linux-x86_64",
    },
    NetJob {
        name: NET_INSTALL_ARM64_JOB,
        packages: &["qemu-system-arm", "qemu-efi-aarch64"],
        platform: "linux-arm64",
    },
];

/// Whether `word` stands in `run` as a word of its own.
fn has_word(run: &str, word: &str) -> bool {
    run.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .any(|w| w == word)
}

/// Whether `run` uses apt at all.
fn mentions_apt(run: &str) -> bool {
    ["apt", "apt-get", "apt-cache", "aptitude", "dpkg"]
        .iter()
        .any(|word| has_word(run, word))
}

/// Which of the two allowed apt steps `run` is, for a job whose packages are `packages`.
#[derive(PartialEq)]
enum Apt {
    Update,
    Install,
}

fn allowed_apt(run: &str, packages: &[&str]) -> Option<Apt> {
    let run = run.trim_end();
    if run == APT_UPDATE {
        return Some(Apt::Update);
    }
    let names = run.strip_prefix(APT_INSTALL)?;
    let words: Vec<&str> = names.split(' ').collect();
    words
        .iter()
        .all(|word| packages.contains(word))
        .then_some(Apt::Install)
}

/// The apt steps of one job, whose `runs` are in order and whose build step is at `build`: the
/// problems with them, and the positions of the allowed ones (each names one `sudo`, which the
/// one-sudo rule and the root-step count of the job leave out). `net` is the job's entry of [`NET_JOBS`] when it is a network-install job.
pub fn apt_steps(
    runs: &[&str],
    build: Option<usize>,
    net: Option<&NetJob>,
) -> (Vec<String>, Vec<usize>) {
    let mut problems = Vec::new();
    let mut allowed = Vec::new();
    let (mut updates, mut installs) = (0, 0);
    for (at, run) in runs.iter().enumerate().filter(|(_, run)| mentions_apt(run)) {
        let kind = net.and_then(|job| allowed_apt(run, job.packages));
        let Some(kind) = kind else {
            problems.push(match net {
                Some(job) => format!(
                    "the apt step `{run}` is not `{APT_UPDATE}` or `{APT_INSTALL}...` of only {:?}",
                    job.packages
                ),
                None => format!(
                    "the apt step `{run}` is not allowed outside the jobs {NET_INSTALL_JOB} and {NET_INSTALL_ARM64_JOB}"
                ),
            });
            continue;
        };
        allowed.push(at);
        match kind {
            Apt::Update => updates += 1,
            Apt::Install => installs += 1,
        }
        if build.is_some_and(|build| at > build) {
            problems.push(format!("the apt step `{run}` comes after the build step"));
        }
    }
    for (what, count) in [("update", updates), ("install", installs)] {
        if count > 1 {
            problems.push(format!("{count} apt-get {what} steps, not at most one"));
        }
    }
    (problems, allowed)
}

/// What is wrong with the ISO of the network-install jobs of the workflow `text`, held to the
/// `url` of the ISO table of each job's platform in `pins`: per job, `env.ISO_URL` equal to the
/// pin and a step whose run uses `"$ISO_URL"`.
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
        let pin = pins
            .get(net.platform)
            .and_then(|platform| platform.get("iso"))
            .and_then(|iso| iso.get("url"))
            .and_then(toml::Value::as_str);
        let Some(pin) = pin else {
            problems.push(format!(
                "job {name}: pins.toml has no url in [{}.iso]",
                net.platform
            ));
            continue;
        };
        match entry(job, "env")
            .and_then(|env| entry(env, "ISO_URL"))
            .and_then(Node::as_str)
        {
            None => problems.push(format!("job {name}: no env ISO_URL")),
            Some(url) if url != pin => problems.push(format!(
                "job {name}: ISO_URL is `{url}`, not the url `{pin}` of [{}.iso] in pins.toml",
                net.platform
            )),
            Some(_) => {}
        }
        let mut values = Vec::new();
        if let Some(steps) = entry(job, "steps") {
            scalars(steps, &mut values);
        }
        if !values.iter().any(|v| v.contains("\"$ISO_URL\"")) {
            problems.push(format!("job {name}: no step uses \"$ISO_URL\""));
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
