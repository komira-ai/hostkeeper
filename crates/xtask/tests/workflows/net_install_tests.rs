//! The rules of `net-install.yml` ([`super::net_install`]), the workflow of the two
//! network-install jobs: its triggers, paths and concurrency, its two jobs and their runner, the
//! test targets of each, the dpkg step that installs QEMU and the firmware from the job's own
//! pinned set, and the step that fetches the ISO by the pin of `packaging/netboot/pins.toml`;
//! and that `privileged.yml`, which a release waits for, has
//! none of these jobs. Each rule is shown to refuse its defect by putting it into the committed
//! workflow and reading the exact problems it reports.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::lint::{PRIVILEGED_WORKFLOW, entry, lint};
use super::net_install::{
    CONCURRENCY_PROBLEM, FILE, GROUP, NET_INSTALL_ARM64_JOB, NET_INSTALL_JOB, NET_JOBS, ON_PROBLEM,
    PATHS, install_steps, iso_problems, iso_run,
};
use super::net_install::{NET_INSTALL_ARM64_FIRST_LINE, NET_INSTALL_FIRST_LINE};
use super::privileged_tests::{
    committed_targets, net_install_targets, shape_problems, starts_with_attribute,
};
use super::release::REQUIRED_WORKFLOWS;
use super::workflows;
use super::yaml::{self, Node};

const INSTALL_X86: &str = "sudo dpkg --install \"$RUNNER_TEMP\"/debs/qemu-x86/*.deb";
const INSTALL_ARM: &str = "sudo dpkg --install \"$RUNNER_TEMP\"/debs/qemu-arm/*.deb";

/// The committed workflow `name`.
fn committed_file(name: &str) -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == name)
        .unwrap_or_else(|| panic!("{name} is in the repository"))
        .1
}

/// The committed net-install.yml.
fn committed() -> String {
    committed_file(FILE)
}

/// The committed packaging/netboot/pins.toml.
fn pins() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/netboot/pins.toml");
    std::fs::read_to_string(path).expect("read pins.toml")
}

/// `text` with `edit` applied to the lines of the job `job` alone (up to the blank line that
/// ends it).
fn edit_job(text: &str, job: &str, mut edit: impl FnMut(&str) -> String) -> String {
    let head = format!("\n  {job}:\n");
    let start = text.find(&head).expect("the job is in the text") + 1;
    let end = text[start..]
        .match_indices("\n\n  ")
        .find(|(at, _)| !text[start + at + 4..].starts_with(' '))
        .map_or(text.len(), |(at, _)| start + at + 1);
    format!(
        "{}{}{}",
        &text[..start],
        edit(&text[start..end]),
        &text[end..]
    )
}

/// `text` with the `from` of the job `job` replaced by `to` (every occurrence, at least one).
pub fn replace_in_job(text: &str, job: &str, from: &str, to: &str) -> String {
    edit_job(text, job, |section| {
        assert!(section.contains(from), "{from:?} is not in job {job}");
        section.replace(from, to)
    })
}

/// A step that runs `run`, as a line of a job's steps.
fn step(run: &str) -> String {
    format!("      - run: {run}\n")
}

/// `text` with the step `run` added as the first step of `job`.
fn with_first_step(text: &str, job: &str, run: &str) -> String {
    replace_in_job(
        text,
        job,
        "    steps:\n",
        &format!("    steps:\n{}", step(run)),
    )
}

fn problems(text: &str) -> Vec<String> {
    shape_problems(FILE, text, &net_install_targets())
}

/// The shape problems of privileged.yml's `text`, held to its own four jobs.
fn privileged_problems(text: &str) -> Vec<String> {
    shape_problems(PRIVILEGED_WORKFLOW, text, &committed_targets())
}

fn not_allowed_here(run: &str) -> String {
    format!(
        "the dpkg step `{run}` is not allowed outside the jobs net-install and net-install-arm64"
    )
}

/// The refusal of the dpkg step `run` in the job `job`, whose own set's install is `own`.
fn not_its_own(job: &str, run: &str, own: &str) -> String {
    format!("job {job}: the dpkg step `{run}` is not `{own}`, the install of its own set")
}

#[test]
fn the_committed_network_install_jobs_pass_the_shape_install_and_iso_rules() {
    // Catches: a rule set that refuses the committed net-install.yml, which installs its own
    // pinned set once in each network-install job before the build, fetches the ISO of each
    // job's platform with mirrors, and keeps the test file of each job under its own first line;
    // and a job left without the install of its set.
    let text = committed();
    assert_eq!(lint(FILE, &text), Vec::<String>::new());
    assert_eq!(problems(&text), Vec::<String>::new());
    assert_eq!(iso_problems(&text, &pins()), Vec::<String>::new());
    for (job, install, set) in [
        (NET_INSTALL_JOB, INSTALL_X86, "qemu-x86"),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM, "qemu-arm"),
    ] {
        let mut seen = false;
        edit_job(&text, job, |section| {
            seen = section.contains(&step_value(install));
            section.to_owned()
        });
        assert!(seen, "{job} installs its set");
        let without = replace_in_job(&text, job, &step_value(install), "run: true");
        assert_eq!(
            problems(&without),
            [format!(
                "job {job}: 0 installs of the pinned set {set}, not one"
            )]
        );
    }
    let targets = net_install_targets();
    assert_eq!(
        targets[NET_INSTALL_JOB],
        BTreeSet::from(["net_install".to_owned()])
    );
    assert_eq!(
        targets[NET_INSTALL_ARM64_JOB],
        BTreeSet::from(["net_install_arm64".to_owned()])
    );
    assert_eq!(
        NET_JOBS.map(|n| (n.name, n.set, n.platform)),
        [
            (NET_INSTALL_JOB, "qemu-x86", "linux-x86_64"),
            (NET_INSTALL_ARM64_JOB, "qemu-arm", "linux-arm64")
        ]
    );
}

/// The `run: ...` of a step that runs `run`, as the committed file writes it.
fn step_value(run: &str) -> String {
    format!("run: {run}")
}

#[test]
fn the_shape_check_holds_each_network_install_job_to_its_own_targets() {
    // Catches: a network-install job that builds or runs the other one's test target (or the
    // privileged job's), or neither, or a table of targets that swaps them.
    let text = committed();
    let wrong = replace_in_job(
        &text,
        NET_INSTALL_JOB,
        " --test net_install",
        " --test net_install_arm64",
    );
    assert_eq!(
        problems(&wrong),
        [
            "job net-install: the build step names the test targets {\"net_install_arm64\"}, not {\"net_install\"}",
            "job net-install: the run as root step names the test targets {\"net_install_arm64\"}, not {\"net_install\"}",
        ]
    );
    let none = replace_in_job(
        &text,
        NET_INSTALL_ARM64_JOB,
        " --test net_install_arm64",
        "",
    );
    assert_eq!(
        problems(&none),
        [
            "job net-install-arm64: the build step names the test targets {}, not {\"net_install_arm64\"}",
            "job net-install-arm64: the run as root step names the test targets {}, not {\"net_install_arm64\"}",
        ]
    );
    let swapped = BTreeMap::from_iter(net_install_targets().into_keys().map(|job| {
        let swap = if job == NET_INSTALL_JOB {
            "net_install_arm64"
        } else {
            "net_install"
        };
        (job, BTreeSet::from([swap.to_owned()]))
    }));
    let swapped_want: Vec<String> = [
        (NET_INSTALL_JOB, "net_install", "net_install_arm64"),
        (NET_INSTALL_ARM64_JOB, "net_install_arm64", "net_install"),
    ]
    .iter()
    .flat_map(|(job, has, wants)| {
        ["build", "run as root"].map(|step| {
            format!("job {job}: the {step} step names the test targets {{\"{has}\"}}, not {{\"{wants}\"}}")
        })
    })
    .collect();
    assert_eq!(shape_problems(FILE, &text, &swapped), swapped_want);
    let privileged_only = replace_in_job(
        &text,
        NET_INSTALL_JOB,
        " --test net_install",
        " --test observer",
    );
    assert_eq!(
        problems(&privileged_only),
        ["build", "run as root"].map(|step| format!(
            "job net-install: the {step} step names the test targets {{\"observer\"}}, not {{\"net_install\"}}"
        ))
    );
    let missing = text.replace("\n  net-install-arm64:\n", "\n  net-install-arm64-gone:\n");
    assert_eq!(
        problems(&missing),
        [
            "no job net-install-arm64",
            "job net-install-arm64-gone is not a job of net-install.yml"
        ]
    );
}

#[test]
fn a_dpkg_step_outside_the_network_install_jobs_is_refused() {
    // Catches: the install allowance let into a job of privileged.yml (the `privileged` job, or
    // any job that is not one of the two network-install jobs), where it is refused as a dpkg
    // step and as a second `sudo`; and a dpkg step that does not use `sudo`.
    let text = committed_file(PRIVILEGED_WORKFLOW);
    for (job, run) in [
        ("privileged", INSTALL_X86),
        ("first-boot", INSTALL_ARM),
        ("openrc-device", INSTALL_X86),
    ] {
        let found = privileged_problems(&with_first_step(&text, job, run));
        assert_eq!(
            found,
            [
                format!("job {job}: {}", not_allowed_here(run)),
                format!("job {job}: sudo is named 2 times, not once"),
                format!("job {job}: 2 run as root steps, not one"),
            ],
            "{job}: {run}"
        );
    }
    let plain = "dpkg --install ovmf_2024.02-2_all.deb";
    assert_eq!(
        privileged_problems(&with_first_step(&text, "privileged", plain)),
        [format!("job privileged: {}", not_allowed_here(plain))]
    );
    // The allowance is the job's in net-install.yml, not its name's: the committed jobs, judged
    // as jobs of another workflow that has them, have their dpkg steps refused.
    let net = committed();
    let found = shape_problems(PRIVILEGED_WORKFLOW, &net, &net_install_targets());
    let want: Vec<String> = [
        (NET_INSTALL_JOB, INSTALL_X86),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM),
    ]
    .iter()
    .flat_map(|(job, install)| {
        [
            format!("job {job}: {}", not_allowed_here(install)),
            format!("job {job}: sudo is named 2 times, not once"),
            format!("job {job}: 2 run as root steps, not one"),
        ]
    })
    .collect();
    assert_eq!(found, want);
}

/// The section of the job `job` in `text`, from its key to the blank line that ends it.
fn job_section(text: &str, job: &str) -> String {
    let mut found = String::new();
    edit_job(text, job, |section| {
        found = section.to_owned();
        section.to_owned()
    });
    found
}

#[test]
fn a_network_install_job_back_in_privileged_yml_is_refused() {
    // Catches: the network-install jobs moved back into privileged.yml, whose successful run each
    // release waits for: a release would wait hours on an emulated install again. Each job is
    // refused as no job of that file by the shape check; and net-install.yml's workflow is not
    // among those release.yml waits for (release_tests.rs holds this list to release.yml's
    // `workflow_run.workflows`; the release verbs' own list is held in src/release/tests.rs).
    let privileged = committed_file(PRIVILEGED_WORKFLOW);
    let net = committed();
    for job in [NET_INSTALL_JOB, NET_INSTALL_ARM64_JOB] {
        let back = format!("{privileged}\n{}", job_section(&net, job));
        assert_eq!(
            privileged_problems(&back),
            [format!("job {job} is not a job of privileged.yml")],
            "{job}"
        );
    }
    let doc = yaml::load(&net).expect("net-install.yml parses");
    let name = entry(&doc, "name").and_then(Node::as_str);
    assert_eq!(name, Some("net-install"));
    assert_eq!(REQUIRED_WORKFLOWS, ["artifacts", "ci", "privileged"]);
}

#[test]
fn an_install_of_another_set_or_another_dpkg_command_is_refused() {
    // Catches: a network-install job installing the other job's set, a set by another spelling
    // (`-i`, an option, a file beside the set, another directory), a dpkg command other than the
    // install (`--purge`), and a chained one; each is no install of its own set, and a `sudo`
    // the one-sudo rule counts.
    let text = committed();
    let cases = [
        (NET_INSTALL_JOB, INSTALL_X86, INSTALL_ARM.to_owned()),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM, INSTALL_X86.to_owned()),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            INSTALL_X86.replace("--install", "-i"),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            INSTALL_X86.replace("--install", "--install --force-all"),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            format!("{INSTALL_X86} extra.deb"),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            "sudo dpkg --install target/qemu-x86/*.deb".to_owned(),
        ),
        (
            NET_INSTALL_ARM64_JOB,
            INSTALL_ARM,
            "sudo dpkg --purge qemu-system-arm".to_owned(),
        ),
    ];
    for (job, from, to) in cases {
        let (set, own) = if job == NET_INSTALL_JOB {
            ("qemu-x86", INSTALL_X86)
        } else {
            ("qemu-arm", INSTALL_ARM)
        };
        let found = problems(&replace_in_job(&text, job, from, &to));
        assert_eq!(
            found,
            [
                not_its_own(job, &to, own),
                format!("job {job}: 0 installs of the pinned set {set}, not one"),
                format!("job {job}: sudo is named 2 times, not once"),
                format!("job {job}: 2 run as root steps, not one"),
            ],
            "{to}"
        );
    }
    let chained = format!("{INSTALL_X86} && sudo dpkg --purge ovmf");
    assert_eq!(
        problems(&replace_in_job(
            &text,
            NET_INSTALL_JOB,
            INSTALL_X86,
            &chained
        )),
        [
            not_its_own(NET_INSTALL_JOB, &chained, INSTALL_X86),
            "job net-install: 0 installs of the pinned set qemu-x86, not one".to_owned(),
            "job net-install: sudo is named 3 times, not once".to_owned(),
            format!("job net-install: the step `{chained}` runs more than one command"),
            "job net-install: 2 run as root steps, not one".to_owned(),
        ]
    );
    let other = with_first_step(&text, NET_INSTALL_ARM64_JOB, "sudo journalctl --no-pager");
    assert_eq!(
        problems(&other),
        [
            "job net-install-arm64: sudo is named 2 times, not once",
            "job net-install-arm64: 2 run as root steps, not one",
        ]
    );
}

#[test]
fn a_second_install_or_one_after_the_build_is_refused() {
    // Catches: a second install of the set (one is allowed, and its `sudo` is the one the
    // one-sudo rule leaves out), and the install placed after the build step of the privileged
    // tests instead of before it.
    let text = committed();
    for (job, run, set) in [
        (NET_INSTALL_JOB, INSTALL_X86, "qemu-x86"),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM, "qemu-arm"),
    ] {
        assert_eq!(
            problems(&with_first_step(&text, job, run)),
            [format!(
                "job {job}: 2 installs of the pinned set {set}, not one"
            )]
        );
        let without = replace_in_job(&text, job, &format!("        {}\n", step_value(run)), "");
        let build = without
            .lines()
            .find(|line| {
                line.contains("--no-run --test net_install") && {
                    (job == NET_INSTALL_JOB) != line.contains("_arm64")
                }
            })
            .expect("the build step");
        let after = with_after(&without, build, &step(run));
        assert_eq!(
            problems(&after),
            [format!(
                "job {job}: the dpkg step `{run}` comes after the build step"
            )],
            "{job}: {run}"
        );
    }
}

/// `text` with the lines `add` inserted after the one line `line`.
fn with_after(text: &str, line: &str, add: &str) -> String {
    assert_eq!(text.matches(&format!("{line}\n")).count(), 1);
    text.replacen(&format!("{line}\n"), &format!("{line}\n{add}"), 1)
}

#[test]
fn the_install_steps_are_judged_against_the_job_they_are_in() {
    // Catches: an install rule that is wrong about order or counts when called on its own: no
    // build step makes no order problem, only the allowed step is left out of the `sudo` count,
    // and a job that is not a network-install job may have none and needs none.
    let net = &NET_JOBS[0];
    assert_eq!(
        install_steps(&[INSTALL_X86], None, Some(net)),
        (vec![], vec![0])
    );
    assert_eq!(
        install_steps(&["cargo build", INSTALL_X86], Some(0), Some(net)).0,
        [format!(
            "the dpkg step `{INSTALL_X86}` comes after the build step"
        )]
    );
    assert_eq!(
        install_steps(&[INSTALL_X86, "build"], Some(1), Some(net)),
        (vec![], vec![0])
    );
    assert_eq!(
        install_steps(&["cargo build"], Some(0), None),
        (vec![], vec![])
    );
    assert_eq!(
        install_steps(&["dpkg-query --show ovmf"], None, None),
        (vec![], vec![])
    );
}

/// The ISO step's `run:` value of `job` in `text`.
fn iso_step(text: &str, job: &str) -> String {
    let mut found = String::new();
    edit_job(text, job, |section| {
        found = section
            .lines()
            .find(|line| line.contains("netboot-iso"))
            .expect("an ISO step")
            .trim_start()
            .trim_start_matches("run: ")
            .to_owned();
        section.to_owned()
    });
    found
}

#[test]
fn the_iso_of_each_network_install_job_is_fetched_by_its_platforms_pin_with_mirrors() {
    // Catches: an ISO step of the other platform, of another pins file or directory, with no
    // mirror, with a mirror that is not a plain https:// base, or a second or missing ISO step;
    // a missing job, a pins file without the platform's ISO, and one that does not parse.
    let text = committed();
    let pins = pins();
    let x86 = iso_step(&text, NET_INSTALL_JOB);
    let arm = iso_step(&text, NET_INSTALL_ARM64_JOB);
    assert!(x86.starts_with(&format!("{} --mirror https://", iso_run("linux-x86_64"))));
    assert!(arm.starts_with(&format!("{} --mirror https://", iso_run("linux-arm64"))));
    let refused = |job: &str, run: &str, platform: &str| {
        format!(
            "job {job}: the ISO step `{run}` is not `{}` and one or more `--mirror https://<base>`",
            iso_run(platform)
        )
    };
    let mirrors = x86.split_once(" --mirror ").expect("a mirror").1;
    for wrong in [
        x86.replace("linux-x86_64", "linux-arm64"),
        x86.replace("packaging/netboot/pins.toml", "pins.toml"),
        x86.replace("--dir target/net-install", "--dir target"),
        iso_run("linux-x86_64"),
        x86.replace(" --mirror https://", " --mirror http://"),
        format!("{x86} --mirror https://mirror.example/u/"),
        format!("{x86} --mirror https://"),
        format!("{x86} --mirror https://me@mirror.example/u"),
        x86.replace(mirrors, &format!("{mirrors} && true")),
    ] {
        let edited = replace_in_job(&text, NET_INSTALL_JOB, &x86, &wrong);
        assert_eq!(
            iso_problems(&edited, &pins),
            [refused(NET_INSTALL_JOB, &wrong, "linux-x86_64")],
            "{wrong}"
        );
    }
    let second = with_first_step(&text, NET_INSTALL_ARM64_JOB, &arm);
    assert_eq!(
        iso_problems(&second, &pins),
        ["job net-install-arm64: 2 steps fetch the ISO, not one"]
    );
    let none = replace_in_job(&text, NET_INSTALL_ARM64_JOB, &arm, "true");
    assert_eq!(
        iso_problems(&none, &pins),
        ["job net-install-arm64: 0 steps fetch the ISO, not one"]
    );
    let gone = text.replace("\n  net-install:\n", "\n  net-install-gone:\n");
    assert_eq!(iso_problems(&gone, &pins), ["no job net-install"]);
    let no_table = pins.replace("[linux-arm64.iso]", "[linux-arm64.disc]");
    assert_eq!(
        iso_problems(&text, &no_table),
        ["job net-install-arm64: pins.toml has no url in [linux-arm64.iso]"]
    );
    assert_eq!(iso_problems(&text, "[linux-x86_64").len(), 1);
    assert_eq!(iso_problems("on: [unclosed\n", &pins).len(), 1);
}

#[test]
fn a_test_file_belongs_to_the_job_whose_first_line_it_starts_with() {
    // Catches: a first-line check that takes a file for the wrong job: the x86-64 file for the
    // arm64 job or the reverse, a file whose attribute only starts like the job's, or a file
    // whose first line rustfmt broke over several lines (the same attribute, so still the job's).
    let x86 = format!("{NET_INSTALL_FIRST_LINE}\n//! Doc.\n");
    let arm = format!("{NET_INSTALL_ARM64_FIRST_LINE}\n//! Doc.\n");
    assert!(starts_with_attribute(&x86, NET_INSTALL_FIRST_LINE));
    assert!(starts_with_attribute(&arm, NET_INSTALL_ARM64_FIRST_LINE));
    assert!(!starts_with_attribute(&x86, NET_INSTALL_ARM64_FIRST_LINE));
    assert!(!starts_with_attribute(&arm, NET_INSTALL_FIRST_LINE));
    let broken = "#![cfg(all(\n    target_os = \"linux\",\n    feature = \"privileged\",\n    not(target_arch = \"aarch64\")\n))]\n";
    assert!(starts_with_attribute(broken, NET_INSTALL_ARM64_FIRST_LINE));
    let looser = NET_INSTALL_FIRST_LINE.replace("all(target_os", "any(target_os");
    assert!(!starts_with_attribute(&looser, NET_INSTALL_FIRST_LINE));
    assert!(!starts_with_attribute(
        "//! No attribute.\n",
        NET_INSTALL_FIRST_LINE
    ));
}

/// The `on` block of the committed net-install.yml: from `on:` to the line before
/// `permissions: {}`.
fn on_block(text: &str) -> String {
    let start = text.find("\non:\n").expect("an `on:` block") + 1;
    let end = text
        .find("\npermissions: {}\n")
        .expect("permissions after `on`")
        + 1;
    text[start..end].to_owned()
}

/// An `on` whose `push` is the lines `push` and whose other triggers are the lines `rest`.
fn on_with(push: &str, rest: &str) -> String {
    format!("on:\n  push:\n{push}{rest}\n")
}

/// `branches: [main]` and `paths`, one `- ` entry each, as lines of `push`.
fn push_lines(paths: &[&str]) -> String {
    let listed: String = paths.iter().map(|p| format!("      - {p}\n")).collect();
    format!("    branches: [main]\n    paths:\n{listed}")
}

const SCHEDULE_AND_DISPATCH: &str = "  schedule:\n    - cron: '41 2 * * *'\n  workflow_dispatch:\n";

/// The pull-request rule's refusal of each committed job, which runs as root.
fn root_on_pull_request(text: &str) -> Vec<String> {
    let doc = yaml::load(text).expect("parses");
    let jobs = entry(&doc, "jobs").expect("jobs");
    [NET_INSTALL_JOB, NET_INSTALL_ARM64_JOB]
        .iter()
        .map(|job| {
            let mut values = Vec::new();
            super::lint::scalars(entry(jobs, job).expect("the job"), &mut values);
            let line = super::lint::root_line(&values).expect("the job runs as root");
            format!("job {job}: privileged on pull_request (`{line}`)")
        })
        .collect()
}

#[test]
fn the_committed_triggers_are_a_push_to_main_with_the_paths_a_schedule_and_dispatch() {
    // Catches: a committed `on` that drifted from the rule (another path, one left out, a second
    // schedule), and the rule's own spelling of it refusing the committed file; and the order of
    // the triggers, a quoted branch or `{}` for no inputs taken for another workflow.
    let text = committed();
    let on = on_block(&text);
    assert_eq!(on, on_with(&push_lines(PATHS), SCHEDULE_AND_DISPATCH));
    let same = [
        format!(
            "on:\n  workflow_dispatch: {{}}\n  schedule:\n    - cron: '0 1 * * *'\n  push:\n{}\n",
            push_lines(PATHS).replace("[main]", "[\"main\"]")
        ),
        on_with(
            &push_lines(PATHS),
            "  schedule:\n    - cron: '41 2 * * *'\n  workflow_dispatch: {}\n",
        ),
    ];
    for same in same {
        assert_eq!(
            lint(FILE, &with_on(&text, &same)),
            Vec::<String>::new(),
            "{same}"
        );
    }
    // Every path the brief of the workflow names is among them, the workflow itself included.
    for path in [
        ".github/workflows/net-install.yml",
        "crates/hostkeeper-pxe*/**",
        "packaging/netboot/**",
        "packaging/linux/**",
        "packaging/pxe/**",
        "crates/hostkeeper-privileged/tests/net_install*.rs",
        "crates/hostkeeper-privileged/tests/netinstall/**",
    ] {
        assert!(PATHS.contains(&path), "{path}");
    }
}

/// `text` with its `on` block replaced by `on`.
fn with_on(text: &str, on: &str) -> String {
    text.replacen(&on_block(text), on, 1)
}

#[test]
fn net_install_yml_on_anything_but_its_three_triggers_is_refused() {
    // Catches: net-install.yml run for every push to main again (no paths, or another list of
    // them), for a push of another branch or of tags, with a paths filter that is not a list,
    // without its schedule or with two, a schedule that is not one cron, dispatch inputs, another
    // trigger, an `on` that is not a mapping, or no `on`.
    let text = committed();
    let paths = push_lines(PATHS);
    let fewer = push_lines(&PATHS[1..]);
    let mut more = PATHS.to_vec();
    more.push("crates/**");
    let more = push_lines(&more);
    let mut reordered = PATHS.to_vec();
    reordered.swap(0, 1);
    let reordered = push_lines(&reordered);
    let cases = [
        on_with("    branches: [main]\n", SCHEDULE_AND_DISPATCH),
        on_with(&fewer, SCHEDULE_AND_DISPATCH),
        on_with(&more, SCHEDULE_AND_DISPATCH),
        on_with(&reordered, SCHEDULE_AND_DISPATCH),
        on_with(
            &paths.replace("[main]", "[main, next]"),
            SCHEDULE_AND_DISPATCH,
        ),
        on_with(&paths.replace("[main]", "[next]"), SCHEDULE_AND_DISPATCH),
        on_with(&paths.replace("[main]", "main"), SCHEDULE_AND_DISPATCH),
        on_with(&format!("{paths}    tags: ['v*']\n"), SCHEDULE_AND_DISPATCH),
        on_with(
            &paths.replace("    branches: [main]\n", "    branches-ignore: [next]\n"),
            SCHEDULE_AND_DISPATCH,
        ),
        on_with(
            "    branches: [main]\n    paths: crates/**\n",
            SCHEDULE_AND_DISPATCH,
        ),
        on_with(&paths, "  workflow_dispatch:\n"),
        on_with(
            &paths,
            "  schedule:\n    - cron: '41 2 * * *'\n    - cron: '41 14 * * *'\n  workflow_dispatch:\n",
        ),
        on_with(
            &paths,
            "  schedule:\n    - cron: [x]\n  workflow_dispatch:\n",
        ),
        on_with(
            &paths,
            "  schedule:\n    - cron: '41 2 * * *'\n      timezone: UTC\n  workflow_dispatch:\n",
        ),
        on_with(&paths, "  schedule: '41 2 * * *'\n  workflow_dispatch:\n"),
        on_with(&paths, "  schedule:\n    - cron: '41 2 * * *'\n"),
        on_with(
            &paths,
            "  schedule:\n    - cron: '41 2 * * *'\n  workflow_dispatch:\n    inputs:\n      target:\n        type: string\n",
        ),
        on_with(
            &paths,
            "  schedule:\n    - cron: '41 2 * * *'\n  workflow_dispatch:\n  workflow_call:\n",
        ),
        "on: [push, schedule, workflow_dispatch]\n".to_owned(),
        "on: push\n".to_owned(),
        String::new(),
    ];
    for on in cases {
        assert_eq!(lint(FILE, &with_on(&text, &on)), [ON_PROBLEM], "{on}");
    }
}

#[test]
fn net_install_yml_run_for_a_pull_request_is_refused() {
    // Catches: the network-install jobs run for a pull request, by `pull_request` beside the three
    // triggers, in place of them, by a review event or the merge queue: the file is refused for
    // its `on`, and each job, which runs as root, by the pull-request rule, as in any workflow but
    // pr-device.yml.
    let text = committed();
    let jobs = root_on_pull_request(&text);
    let paths = push_lines(PATHS);
    for trigger in [
        "pull_request",
        "pull_request_review",
        "pull_request_review_comment",
        "merge_group",
    ] {
        let beside = on_with(&paths, &format!("{SCHEDULE_AND_DISPATCH}  {trigger}:\n"));
        let found = lint(FILE, &with_on(&text, &beside));
        let mut want = jobs.clone();
        want.push(ON_PROBLEM.to_owned());
        assert_eq!(found, want, "{trigger}");
    }
    let alone = lint(
        FILE,
        &with_on(
            &text,
            "on:\n  pull_request:\n    paths: [packaging/netboot/**]\n",
        ),
    );
    let mut want = jobs;
    want.push(ON_PROBLEM.to_owned());
    assert_eq!(alone, want);
}

#[test]
fn the_concurrency_of_net_install_yml_is_one_group_per_branch_that_cancels() {
    // Catches: superseded runs left to pile up on main (`cancel-in-progress: false`, or none), a
    // group per commit (each commit's run kept, so none is ever cancelled), one group shared with
    // another workflow, an extra key, or no concurrency at all.
    let text = committed();
    let block = format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: true\n");
    assert_eq!(text.matches(&block).count(), 1, "the committed concurrency");
    let cases = [
        block.replace("cancel-in-progress: true", "cancel-in-progress: false"),
        block.replace("  cancel-in-progress: true\n", ""),
        block.replace(GROUP, "net-install-${{ github.sha }}"),
        block.replace(GROUP, "privileged-${{ github.ref }}"),
        block.replace(GROUP, "net-install"),
        format!("{block}  extra: x\n"),
        "concurrency: net-install\n".to_owned(),
        String::new(),
    ];
    for concurrency in cases {
        let found = lint(FILE, &text.replacen(&block, &concurrency, 1));
        assert_eq!(found, [CONCURRENCY_PROBLEM], "{concurrency}");
    }
}

#[test]
fn net_install_yml_holds_exactly_its_two_jobs_on_the_listed_runner() {
    // Catches: another job let into the one workflow besides privileged.yml that runs as root, a
    // network-install job missing (renamed or removed), and a job on a runner whose image moves
    // under us (`ubuntu-latest`), on arm (no KVM), or on macOS.
    let text = committed();
    let renamed = text.replace("\n  net-install-arm64:\n", "\n  net-install-arm:\n");
    assert_eq!(
        lint(FILE, &renamed),
        [
            "no job net-install-arm64",
            "job net-install-arm is not a job of net-install.yml"
        ]
    );
    let extra = format!(
        "{text}\n{}",
        job_section(&text, NET_INSTALL_JOB).replacen("  net-install:\n", "  another:\n", 1)
    );
    assert_eq!(
        lint(FILE, &extra),
        ["job another is not a job of net-install.yml"]
    );
    for runner in ["ubuntu-latest", "ubuntu-24.04-arm", "macos-15"] {
        let moved = replace_in_job(
            &text,
            NET_INSTALL_ARM64_JOB,
            "    runs-on: ubuntu-24.04\n",
            &format!("    runs-on: {runner}\n"),
        );
        assert_eq!(
            lint(FILE, &moved),
            ["job net-install-arm64: privileged job on a runner outside the privileged list"],
            "{runner}"
        );
    }
}

#[test]
fn no_job_of_net_install_yml_holds_a_write_scope() {
    // Catches: a write scope granted in the network-install workflow, which no entry of the table
    // of write scopes names.
    let text = committed();
    let found = lint(
        FILE,
        &replace_in_job(
            &text,
            NET_INSTALL_JOB,
            "      contents: read\n",
            "      contents: write\n",
        ),
    );
    assert_eq!(
        found,
        ["job net-install: `contents: write` is not allowed in net-install.yml"]
    );
}
