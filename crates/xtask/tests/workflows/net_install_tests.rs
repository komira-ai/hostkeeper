//! The rules of the two network-install jobs of `privileged.yml` ([`super::net_install`]): the
//! test targets of each, the apt steps that install QEMU and the firmware, and the ISO held to the
//! pin of `packaging/netboot/pins.toml`. Each rule is shown to refuse its defect by putting it
//! into the committed workflow and reading the exact problems it reports.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::lint::PRIVILEGED_WORKFLOW;
use super::net_install::{NET_INSTALL_ARM64_FIRST_LINE, NET_INSTALL_FIRST_LINE};
use super::net_install::{
    NET_INSTALL_ARM64_JOB, NET_INSTALL_JOB, NET_JOBS, apt_steps, iso_problems,
};
use super::privileged_tests::{committed_targets, shape_problems, starts_with_attribute};
use super::workflows;

const UPDATE: &str = "sudo apt-get update";
const INSTALL_X86: &str = "sudo apt-get install --yes --no-install-recommends qemu-system-x86 ovmf";
const INSTALL_ARM: &str =
    "sudo apt-get install --yes --no-install-recommends qemu-system-arm qemu-efi-aarch64";

/// The committed privileged.yml.
fn committed() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == PRIVILEGED_WORKFLOW)
        .expect("privileged.yml is in the repository")
        .1
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
fn replace_in_job(text: &str, job: &str, from: &str, to: &str) -> String {
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
    shape_problems(text, &committed_targets())
}

fn not_allowed_here(run: &str) -> String {
    format!(
        "the apt step `{run}` is not allowed outside the jobs net-install and net-install-arm64"
    )
}

fn refused_x86(run: &str) -> String {
    format!(
        "job net-install: the apt step `{run}` is not `sudo apt-get update` or `sudo apt-get install --yes --no-install-recommends ...` of only [\"qemu-system-x86\", \"ovmf\"]"
    )
}

fn refused_arm(run: &str) -> String {
    format!(
        "job net-install-arm64: the apt step `{run}` is not `sudo apt-get update` or `sudo apt-get install --yes --no-install-recommends ...` of only [\"qemu-system-arm\", \"qemu-efi-aarch64\"]"
    )
}

#[test]
fn the_committed_network_install_jobs_pass_the_shape_apt_and_iso_rules() {
    // Catches: a rule set that refuses the committed privileged.yml, which names one `sudo apt-get
    // update` and one install of its listed packages in each network-install job before the build,
    // the ISO of each job at the pin, and the test file of each job under its own first line.
    let text = committed();
    assert_eq!(problems(&text), Vec::<String>::new());
    assert_eq!(iso_problems(&text, &pins()), Vec::<String>::new());
    for (job, install) in [
        (NET_INSTALL_JOB, INSTALL_X86),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM),
    ] {
        let mut seen = (false, false);
        edit_job(&text, job, |section| {
            seen = (
                section.contains(&step_value(UPDATE)),
                section.contains(&step_value(install)),
            );
            section.to_owned()
        });
        assert_eq!(seen, (true, true), "{job} updates and installs");
    }
    let targets = committed_targets();
    assert_eq!(
        targets[NET_INSTALL_JOB],
        BTreeSet::from(["net_install".to_owned()])
    );
    assert_eq!(
        targets[NET_INSTALL_ARM64_JOB],
        BTreeSet::from(["net_install_arm64".to_owned()])
    );
    // No apt step at all is also allowed: at most one of each.
    let bare = [NET_INSTALL_JOB, NET_INSTALL_ARM64_JOB]
        .iter()
        .fold(text, |text, job| {
            let text = replace_in_job(&text, job, &step_value(UPDATE), "run: true");
            let install = NET_JOBS
                .iter()
                .find(|n| n.name == *job)
                .map(|n| n.packages.join(" "));
            replace_in_job(
                &text,
                job,
                &step_value(&format!(
                    "sudo apt-get install --yes --no-install-recommends {}",
                    install.expect("a network-install job")
                )),
                "run: true",
            )
        });
    assert_eq!(problems(&bare), Vec::<String>::new());
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
    let swapped = BTreeMap::from_iter(committed_targets().into_iter().map(|(job, targets)| {
        let swap = match job {
            NET_INSTALL_JOB => "net_install_arm64",
            NET_INSTALL_ARM64_JOB => "net_install",
            _ => return (job, targets),
        };
        (job, BTreeSet::from([swap.to_owned()]))
    }));
    assert_eq!(shape_problems(&text, &swapped).len(), 4);
    let privileged_only = replace_in_job(
        &text,
        NET_INSTALL_JOB,
        " --test net_install",
        " --test observer",
    );
    assert_eq!(problems(&privileged_only).len(), 2);
    let missing = text.replace("\n  net-install-arm64:\n", "\n  net-install-arm64-gone:\n");
    assert_eq!(
        problems(&missing),
        [
            "no job net-install-arm64",
            "job net-install-arm64-gone is not a job of privileged.yml"
        ]
    );
}

#[test]
fn an_apt_step_outside_the_network_install_jobs_is_refused() {
    // Catches: the apt allowance let into the `privileged` job (or any job that is not one of the
    // two), where an apt install is refused as an apt step and as a second `sudo`; and an apt step
    // that does not use `sudo`.
    let text = committed();
    for (job, run) in [
        ("privileged", INSTALL_X86),
        ("privileged", UPDATE),
        ("first-boot", UPDATE),
        ("macos-device", INSTALL_X86),
    ] {
        let found = problems(&with_first_step(&text, job, run));
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
    let plain = "apt-get install --yes ovmf";
    assert_eq!(
        problems(&with_first_step(&text, "privileged", plain)),
        [format!("job privileged: {}", not_allowed_here(plain))]
    );
}

#[test]
fn an_apt_install_naming_a_package_outside_the_jobs_list_is_refused() {
    // Catches: an install of a package the job does not list: an extra one, the other job's, none
    // at all, a pinned version, or an option (`--allow-unauthenticated`, or one before the list).
    let text = committed();
    let cases = [
        (NET_INSTALL_JOB, INSTALL_X86, format!("{INSTALL_X86} curl")),
        (NET_INSTALL_JOB, INSTALL_X86, INSTALL_ARM.to_owned()),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            "sudo apt-get install --yes --no-install-recommends ".to_owned(),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            "sudo apt-get install --yes --no-install-recommends ovmf=2024.02".to_owned(),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            INSTALL_X86.replace(
                "--no-install-recommends ",
                "--no-install-recommends --allow-unauthenticated ",
            ),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            "sudo apt-get install --yes qemu-system-x86 ovmf".to_owned(),
        ),
        (
            NET_INSTALL_JOB,
            INSTALL_X86,
            "sudo apt-get install --yes --no-install-recommends -t noble-proposed ovmf".to_owned(),
        ),
        (
            NET_INSTALL_ARM64_JOB,
            INSTALL_ARM,
            format!("{INSTALL_ARM} ovmf"),
        ),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM, INSTALL_X86.to_owned()),
    ];
    for (job, from, to) in cases {
        let refused = if job == NET_INSTALL_JOB {
            refused_x86(to.trim_end())
        } else {
            refused_arm(&to)
        };
        let found = problems(&replace_in_job(&text, job, from, &to));
        assert_eq!(
            found,
            [
                refused,
                format!("job {job}: sudo is named 2 times, not once"),
                format!("job {job}: 2 run as root steps, not one"),
            ],
            "{to}"
        );
    }
}

#[test]
fn another_apt_command_or_a_chained_one_is_refused() {
    // Catches: `sudo apt-get upgrade` or `remove` taken for the update, an update with a flag or
    // chained to another command, and a `sudo` step of any other kind in a network-install job,
    // which the one-sudo rule still counts.
    let text = committed();
    for run in [
        "sudo apt-get upgrade",
        "sudo apt-get remove --yes snapd",
        "sudo apt-get update --allow-insecure-repositories",
        "sudo apt-get dist-upgrade --yes",
        "sudo apt install --yes ovmf",
    ] {
        let found = problems(&replace_in_job(&text, NET_INSTALL_JOB, UPDATE, run));
        assert_eq!(
            found,
            [
                refused_x86(run),
                "job net-install: sudo is named 2 times, not once".to_owned(),
                "job net-install: 2 run as root steps, not one".to_owned(),
            ],
            "{run}"
        );
    }
    let chained = "sudo apt-get update && sudo apt-get install --yes --no-install-recommends ovmf";
    assert_eq!(
        problems(&replace_in_job(&text, NET_INSTALL_JOB, UPDATE, chained)),
        [
            refused_x86(chained),
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
fn a_second_apt_step_or_one_after_the_build_is_refused() {
    // Catches: a second `sudo apt-get update` or install step (at most one of each is allowed,
    // and each is a `sudo` the one-sudo rule leaves out), and an apt step placed after the build
    // step of the privileged tests instead of before it.
    let text = committed();
    for (job, run, what) in [
        (NET_INSTALL_JOB, UPDATE, "update"),
        (NET_INSTALL_JOB, INSTALL_X86, "install"),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM, "install"),
    ] {
        assert_eq!(
            problems(&with_first_step(&text, job, run)),
            [format!(
                "job {job}: 2 apt-get {what} steps, not at most one"
            )]
        );
    }
    for (job, run) in [
        (NET_INSTALL_JOB, UPDATE),
        (NET_INSTALL_JOB, INSTALL_X86),
        (NET_INSTALL_ARM64_JOB, UPDATE),
        (NET_INSTALL_ARM64_JOB, INSTALL_ARM),
    ] {
        let without = replace_in_job(&text, job, &format!("{}\n", step_value(run)), "\n");
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
                "job {job}: the apt step `{run}` comes after the build step"
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
fn the_apt_steps_are_judged_against_the_job_they_are_in() {
    // Catches: an apt rule that is wrong about order or counts when called on its own: no build
    // step makes no order problem, only the allowed steps are left out of the `sudo` count.
    let net = &NET_JOBS[0];
    assert_eq!(
        apt_steps(&[UPDATE, INSTALL_X86], None, Some(net)),
        (vec![], vec![0, 1])
    );
    assert_eq!(
        apt_steps(&["cargo build"], Some(0), Some(net)),
        (vec![], vec![])
    );
    assert_eq!(
        apt_steps(&[INSTALL_ARM], None, Some(net)),
        (
            vec![refused_x86(INSTALL_ARM)["job net-install: ".len()..].to_owned()],
            vec![]
        )
    );
    assert_eq!(
        apt_steps(&[UPDATE, "build"], Some(1), Some(net)).0,
        Vec::<String>::new()
    );
    assert_eq!(
        apt_steps(&["build", UPDATE], Some(0), Some(net)).0,
        [format!(
            "the apt step `{UPDATE}` comes after the build step"
        )]
    );
}

/// The `ISO_URL` line of `job` in `text`.
fn iso_line(text: &str, job: &str) -> String {
    let mut found = String::new();
    edit_job(text, job, |section| {
        found = section
            .lines()
            .find(|line| line.trim_start().starts_with("ISO_URL:"))
            .expect("an ISO_URL line")
            .to_owned();
        section.to_owned()
    });
    found
}

#[test]
fn the_iso_of_each_network_install_job_is_held_to_the_pin() {
    // Catches: an ISO_URL that differs from the `url` of the job's platform in pins.toml (the
    // other platform's, a changed pin, or a plain typo), no ISO_URL, a step that does not use
    // "$ISO_URL", a missing job, a pins file without the table, and one that does not parse.
    let text = committed();
    let pins = pins();
    let x86 = iso_line(&text, NET_INSTALL_JOB);
    let arm = iso_line(&text, NET_INSTALL_ARM64_JOB);
    let x86_url = x86.trim_start().trim_start_matches("ISO_URL: ");
    let arm_url = arm.trim_start().trim_start_matches("ISO_URL: ");
    assert_ne!(x86_url, arm_url);

    let swapped = replace_in_job(&text, NET_INSTALL_JOB, x86_url, arm_url);
    assert_eq!(
        iso_problems(&swapped, &pins),
        [format!(
            "job net-install: ISO_URL is `{arm_url}`, not the url `{x86_url}` of [linux-x86_64.iso] in pins.toml"
        )]
    );
    let typo = replace_in_job(&text, NET_INSTALL_ARM64_JOB, "24.04.5", "24.04.4");
    assert_eq!(
        iso_problems(&typo, &pins).len(),
        1,
        "a URL that is not the pin's, by one character"
    );
    let moved = pins.replace(x86_url, &format!("{x86_url}.new"));
    assert_eq!(
        iso_problems(&text, &moved),
        [format!(
            "job net-install: ISO_URL is `{x86_url}`, not the url `{x86_url}.new` of [linux-x86_64.iso] in pins.toml"
        )]
    );
    let no_env = replace_in_job(&text, NET_INSTALL_ARM64_JOB, &format!("{arm}\n"), "");
    assert_eq!(
        iso_problems(&no_env, &pins),
        ["job net-install-arm64: no env ISO_URL"]
    );
    let unused = replace_in_job(&text, NET_INSTALL_JOB, "\"$ISO_URL\"", "\"$OTHER_URL\"");
    assert_eq!(
        iso_problems(&unused, &pins),
        ["job net-install: no step uses \"$ISO_URL\""]
    );
    let unquoted = replace_in_job(&text, NET_INSTALL_JOB, "\"$ISO_URL\"", "$ISO_URL");
    assert_eq!(iso_problems(&unquoted, &pins).len(), 1);
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
