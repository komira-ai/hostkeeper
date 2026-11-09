//! The rule of `linux_releases.rs` on the committed workflows and list, and each defect it
//! refuses, put into the committed `privileged.yml` (or the list) one at a time.

use std::path::PathBuf;

use super::lint::PRIVILEGED_WORKFLOW;
use super::linux_releases::{PULL, linux_releases_problems};
use super::pr_device;
use super::workflows;

/// The committed workflow `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
        .1
}

/// The committed list.
fn list() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-privileged/linux-releases.toml");
    std::fs::read_to_string(path).expect("read linux-releases.toml")
}

/// The images of the committed list, in order.
fn images() -> Vec<String> {
    let table: toml::Table = list().parse().expect("the list parses");
    table["release"]
        .as_array()
        .expect("releases")
        .iter()
        .map(|r| r["image"].as_str().expect("an image").to_owned())
        .collect()
}

/// `text` with the step that pulls `image` taken out.
fn without_pull(text: &str, image: &str) -> String {
    let at = text
        .find(&format!("        run: {PULL} {image}\n"))
        .expect("the pull is in the text");
    let start = text[..at].rfind("      - name:").expect("its step");
    let end = at + text[at..].find('\n').expect("a line end") + 1;
    format!("{}{}", &text[..start], &text[end..])
}

#[test]
fn the_committed_workflows_pull_exactly_the_listed_images() {
    // Catches: an image added to the list without its pull step (the test would run a container
    // of an image it never pulled), and the copy in pr-device.yml drifting from privileged.yml.
    for file in [PRIVILEGED_WORKFLOW, pr_device::FILE] {
        assert_eq!(
            linux_releases_problems(&committed(file), &list()),
            Vec::<String>::new(),
            "{file}"
        );
    }
    assert_eq!(images().len(), 8);
}

#[test]
fn a_listed_image_without_a_pull_or_with_two_is_refused() {
    // Catches: a release the test runs without its image pulled, and a pull step repeated.
    let text = committed(PRIVILEGED_WORKFLOW);
    let images = images();
    let image = &images[3];
    assert_eq!(
        linux_releases_problems(&without_pull(&text, image), &list()),
        [format!("job privileged: no step runs `{PULL} {image}`")]
    );
    let step = format!("      - name: Pull ubuntu:26.04\n        run: {PULL} {image}\n");
    assert!(text.contains(&step));
    assert_eq!(
        linux_releases_problems(&text.replacen(&step, &format!("{step}{step}"), 1), &list()),
        [format!(
            "job privileged: 2 steps run `{PULL} {image}`, not one"
        )]
    );
}

#[test]
fn a_pull_of_an_unlisted_or_unpinned_image_or_in_a_chain_is_refused() {
    // Catches: an image pulled by tag (a moved tag changes what runs), a pull of an image the
    // list does not hold, and a pull chained into another command.
    let text = committed(PRIVILEGED_WORKFLOW);
    let images = images();
    let image = &images[0];
    let by_tag = format!("{PULL} debian:12");
    let unpinned = text.replacen(&format!("{PULL} {image}"), &by_tag, 1);
    assert_eq!(
        linux_releases_problems(&unpinned, &list()),
        [
            format!(
                "job privileged: the step `{by_tag}` pulls what linux-releases.toml does not list (a step is `{PULL} <image>`, the image listed)"
            ),
            format!("job privileged: no step runs `{PULL} {image}`"),
        ]
    );
    let chained = format!("{PULL} {image} && true");
    let text = committed(PRIVILEGED_WORKFLOW).replacen(&format!("{PULL} {image}"), &chained, 1);
    assert_eq!(
        linux_releases_problems(&text, &list()),
        [
            format!(
                "job privileged: the step `{chained}` pulls what linux-releases.toml does not list (a step is `{PULL} <image>`, the image listed)"
            ),
            format!("job privileged: no step runs `{PULL} {image}`"),
        ]
    );
}

#[test]
fn a_pull_after_the_run_as_root_is_refused() {
    // Catches: an image pulled after the test that needs it has run (`--pull=never` would fail it).
    let text = committed(PRIVILEGED_WORKFLOW);
    let images = images();
    let image = &images[7];
    let step = format!("      - name: Pull opensuse/tumbleweed\n        run: {PULL} {image}\n");
    assert!(text.contains(&step));
    let moved = text.replacen(&step, "", 1).replacen(
        "      - name: The journal of the test units\n",
        &format!("{step}      - name: The journal of the test units\n"),
        1,
    );
    assert_eq!(
        linux_releases_problems(&moved, &list()),
        [format!(
            "job privileged: the pull of `{image}` comes after the run as root"
        )]
    );
}

#[test]
fn a_list_that_is_not_a_list_of_pinned_releases_is_refused() {
    // Catches: a list that does not parse, one without releases, and a release without a text
    // key, with another format, or with an image a tag moves.
    let text = committed(PRIVILEGED_WORKFLOW);
    let list = list();
    let image = &images()[0];
    let problems = |list: &str| linux_releases_problems(&text, list);
    assert!(
        problems("not toml [")[0].starts_with("linux-releases.toml does not parse: "),
        "{:?}",
        problems("not toml [")
    );
    assert_eq!(
        problems("x = 1"),
        ["linux-releases.toml has no [[release]] tables"]
    );
    assert_eq!(
        problems("release = []"),
        ["linux-releases.toml lists no release"]
    );
    assert_eq!(
        problems("release = [1]"),
        ["linux-releases.toml: release 1 is not a table"]
    );
    assert_eq!(
        problems(&list.replacen("name = \"debian:12\"\n", "", 1)),
        ["linux-releases.toml: release 1 has no text name"]
    );
    assert_eq!(
        problems(&list.replacen("format = \"deb\"", "format = \"apk\"", 1)),
        ["linux-releases.toml: release 1 has the format `apk`, not deb or rpm"]
    );
    // The Arch package is withdrawn: a release of that format is refused.
    assert_eq!(
        problems(&list.replacen("format = \"deb\"", "format = \"arch\"", 1)),
        ["linux-releases.toml: release 1 has the format `arch`, not deb or rpm"]
    );
    assert_eq!(
        problems(&list.replacen(image, "debian:12", 1)),
        ["linux-releases.toml: release 1 has the image `debian:12`, which is not pinned by digest"]
    );
    assert_eq!(
        problems(&list.replacen(&format!("image = \"{image}\"\n"), "image = 3\n", 1)),
        ["linux-releases.toml: release 1 has no text image"]
    );
}

#[test]
fn a_workflow_that_does_not_load_or_has_no_job_is_refused() {
    // Catches: a rule that passes on a file it could not read, or on a file without the job.
    assert!(!linux_releases_problems("a: [", &list()).is_empty());
    assert_eq!(
        linux_releases_problems("name: x\njobs:\n  other:\n    steps: []\n", &list()),
        ["no job privileged"]
    );
}
