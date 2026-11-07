//! The rules of the two network-install jobs of `privileged.yml` beyond those every job of the
//! file keeps (`privileged_tests.rs`): the apt steps that install QEMU and the firmware, and the
//! ISO each job downloads, held to the pin of `packaging/netboot/pins.toml`. Each is a function
//! over the workflow text (and the pins text) that returns its problems; `net_install_tests.rs`
//! shows each to refuse its defect.

use super::lint::{entry, scalars};
use super::yaml::{self, Node};

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
