//! The rule of [`super::release_assets`]: the repository's workflows pass, and each defect is
//! shown refused, by the exact problem, in `release.yml` with the defect put in, or in a small
//! workflow of one job on a release's commit (the coordinator updater's tarball, which release
//! 0.1.208 does not have, named with no `if:`).

use super::release_assets::{FLOOR_FILE, guard, problems};
use super::release_ref::FLOOR;
use super::{with_in, workflows, yaml};

/// A checkout of the commit of the release a run verifies.
const RELEASE_CHECKOUT: &str = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          ref: ${{ matrix.commit }}\n";

/// The step that holds `dist` to the released commit's own assets.
const CHECK: &str = "      - run: cargo xtask release check dist --version \"$RELEASE_VERSION\" --commit \"$COMMIT\"\n";

/// The coordinator updater's linux-x86_64 tarball, which release 0.1.208 does not have.
const UPDATER: &str = "hostkeeper-coordinator-update-<version>-linux-x86_64.tar.gz";

/// hostkeeper's linux-x86_64 tarball, which every release since 0.1.208 has.
const HOSTKEEPER: &str = "hostkeeper-<version>-linux-x86_64.tar.gz";

/// The problems of a workflow of one job `j` whose steps are `steps`.
fn job_problems(steps: &str) -> Vec<String> {
    let text = format!("jobs:\n  j:\n    runs-on: ubuntu-24.04\n    steps:\n{steps}");
    let doc = yaml::load(&text).expect("the workflow parses");
    problems("t.yml", &doc)
}

/// A step that unpacks `name` of dist into the binary directory as `program`, under `condition`.
fn unpack(name: &str, program: &str, condition: Option<&str>) -> String {
    let file = name.replace("<version>", "$RELEASE_VERSION");
    let run = format!("run: tar -xzf \"dist/{file}\" -C \"$RUNNER_TEMP/binary\" {program}");
    match condition {
        None => format!("      - {run}\n"),
        Some(c) => format!("      - if: {c}\n        {run}\n"),
    }
}

/// A step that runs the unpacked `program`, under `condition`.
fn check_binary(program: &str, condition: Option<&str>) -> String {
    let run = format!(
        "run: cargo xtask release check-binary \"$RUNNER_TEMP/binary/{program}\" \"$RELEASE_VERSION\" \"$COMMIT\""
    );
    match condition {
        None => format!("      - {run}\n"),
        Some(c) => format!("      - if: {c}\n        {run}\n"),
    }
}

/// The problem of step `n` of the job `job` of `file` naming `name`, which the floor lacks,
/// outside its guard.
fn unguarded(file: &str, job: &str, n: usize, name: &str) -> String {
    format!(
        "{file}: job {job} step {n} names dist/{name}, which release {} does not have, so the build of an older commit a release can be of did not make it: the step runs only under `if: {}`, after a step that runs `cargo xtask release check dist`",
        FLOOR.0,
        guard(name)
    )
}

fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml")
}

fn release_problems(text: &str) -> Vec<String> {
    problems(
        "release.yml",
        &yaml::load(text).expect("release.yml parses"),
    )
}

#[test]
fn the_repository_workflows_name_in_dist_only_files_every_released_commit_makes() {
    // Catches: a job on a release's commit (release.yml's verify) that unpacks or reads a file
    // of dist the build of an older commit it can release did not make, the way the verify of
    // v0.1.233 failed on the coordinator updater's tarball, a file its commit predates.
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("every workflow parses");
        assert_eq!(problems(&file, &doc), Vec::<String>::new(), "{file}");
    }
}

#[test]
fn release_yml_runs_the_coordinator_updater_only_when_the_release_has_its_tarball() {
    // Catches: the verify job's unpacking of the coordinator updater's tarball with no `if:`
    // (the defect that failed the release of a commit from before the updater), or its run of
    // the binary under another `if:` than its unpacking, so it runs a binary never unpacked.
    let text = release_yml();
    let condition = guard(UPDATER);
    let unpacking = format!("      - if: {condition}\n        run: tar -xzf");
    let running = format!(
        "        if: {condition}\n        run: cargo xtask release check-binary \"$RUNNER_TEMP/binary/hostkeeper-coordinator-update\""
    );
    assert_eq!(release_problems(&text), Vec::<String>::new());
    assert_eq!(
        release_problems(&with_in(&text, &unpacking, "      - run: tar -xzf")),
        vec![
            unguarded("release.yml", "verify", 17, UPDATER),
            format!(
                "release.yml: job verify step 18 runs $RUNNER_TEMP/binary/hostkeeper-coordinator-update under `if: {condition}`, not the `if:` of the step that unpacks it, `(none)`"
            ),
        ]
    );
    let both = with_in(
        &with_in(&text, &unpacking, "      - run: tar -xzf"),
        &running,
        "        run: cargo xtask release check-binary \"$RUNNER_TEMP/binary/hostkeeper-coordinator-update\"",
    );
    assert_eq!(
        release_problems(&both),
        vec![unguarded("release.yml", "verify", 17, UPDATER)]
    );
}

#[test]
fn a_file_the_floor_has_needs_no_if_and_one_it_lacks_runs_under_its_guard_after_the_check() {
    // Catches: the rule refusing a file every released commit makes, or passing a file the floor
    // lacks with no `if:`, with the `if:` of another file, or before the commit's own check has
    // held dist to its assets (when a commit whose build lost the file would skip its check).
    let floor = format!(
        "{RELEASE_CHECKOUT}{CHECK}{}",
        unpack(HOSTKEEPER, "hostkeeper", None)
    );
    assert_eq!(job_problems(&floor), Vec::<String>::new());
    let ok = format!(
        "{RELEASE_CHECKOUT}{CHECK}{}{}",
        unpack(UPDATER, "u", Some(&guard(UPDATER))),
        check_binary("u", Some(&guard(UPDATER)))
    );
    assert_eq!(job_problems(&ok), Vec::<String>::new());
    let cases = [
        (
            format!("{RELEASE_CHECKOUT}{CHECK}{}", unpack(UPDATER, "u", None)),
            3,
        ),
        (
            format!(
                "{RELEASE_CHECKOUT}{CHECK}{}",
                unpack(UPDATER, "u", Some(&guard(HOSTKEEPER)))
            ),
            3,
        ),
        (
            format!(
                "{RELEASE_CHECKOUT}{}{CHECK}",
                unpack(UPDATER, "u", Some(&guard(UPDATER)))
            ),
            2,
        ),
    ];
    for (steps, n) in cases {
        assert_eq!(
            job_problems(&steps),
            vec![unguarded("t.yml", "j", n, UPDATER)],
            "{steps}"
        );
    }
}

#[test]
fn a_withdrawn_unknown_or_unread_name_is_refused() {
    // Catches: a file the floor has but a release no longer makes (an archive package, withdrawn
    // since), a name of no release, or a version written so the rule cannot tell the file,
    // passed as present in every build.
    let cases = [
        (
            "dist/hostkeeper-$RELEASE_VERSION-linux-x86_64.tar.zst",
            "t.yml: job j step 2 names dist/hostkeeper-<version>-linux-x86_64.tar.zst, which is not an asset of a release (crates/xtask/tests/release/asset_names.baseline)",
        ),
        (
            "dist/nothing.json",
            "t.yml: job j step 2 names dist/nothing.json, which is not an asset of a release (crates/xtask/tests/release/asset_names.baseline)",
        ),
        (
            "dist/hostkeeper-${TAG#v}-linux-x86_64.tar.gz",
            "t.yml: job j step 2 names dist/hostkeeper-${TAG#v}-linux-x86_64.tar.gz, a name this rule cannot read (the version is written $RELEASE_VERSION)",
        ),
    ];
    for (path, problem) in cases {
        let steps = format!("{RELEASE_CHECKOUT}      - run: cat \"{path}\"\n");
        assert_eq!(job_problems(&steps), vec![problem.to_owned()], "{path}");
    }
    // `${RELEASE_VERSION}` is read as the version too, and a `dist/` inside a longer word is not
    // a file of dist.
    let steps = format!(
        "{RELEASE_CHECKOUT}      - run: cat dist/SHA256SUMS \"dist/hkctl-${{RELEASE_VERSION}}\" \"$RUNNER_TEMP/apt-dist/x\"\n"
    );
    assert_eq!(job_problems(&steps), Vec::<String>::new());
}

#[test]
fn only_a_job_whose_last_checkout_is_a_release_commit_is_held() {
    // Catches: the rule holding a job that builds its own commit (artifacts.yml writes the new
    // files into its own dist), or still holding a job after it checked out its own commit again
    // (or no longer holding it after a checkout of another release commit).
    let own = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n";
    let sha = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          ref: ${{ github.sha }}\n";
    let new_file = unpack(UPDATER, "u", None);
    for steps in [
        new_file.clone(),
        format!("{own}{new_file}"),
        format!("{sha}{new_file}"),
        format!("{RELEASE_CHECKOUT}{own}{new_file}"),
    ] {
        assert_eq!(job_problems(&steps), Vec::<String>::new(), "{steps}");
    }
    assert_eq!(
        job_problems(&format!("{own}{RELEASE_CHECKOUT}{new_file}")),
        vec![unguarded("t.yml", "j", 3, UPDATER)]
    );
    // A checkout of another release commit forgets the check of the first.
    assert_eq!(
        job_problems(&format!(
            "{RELEASE_CHECKOUT}{CHECK}{RELEASE_CHECKOUT}{}",
            unpack(UPDATER, "u", Some(&guard(UPDATER)))
        )),
        vec![unguarded("t.yml", "j", 4, UPDATER)]
    );
}

#[test]
fn a_binary_runs_only_after_its_unpacking_and_under_its_if() {
    // Catches: a check of a binary that no step unpacked (a renamed program), or one that runs
    // when its unpacking was skipped, failing every release of a commit without the binary.
    let condition = guard(UPDATER);
    assert_eq!(
        job_problems(&format!(
            "{RELEASE_CHECKOUT}{CHECK}{}",
            check_binary("u", None)
        )),
        vec![
            "t.yml: job j step 3 runs $RUNNER_TEMP/binary/u, which no step before it unpacks"
                .to_owned()
        ]
    );
    assert_eq!(
        job_problems(&format!(
            "{RELEASE_CHECKOUT}{CHECK}{}{}",
            unpack(UPDATER, "u", Some(&condition)),
            check_binary("u", None)
        )),
        vec![format!(
            "t.yml: job j step 4 runs $RUNNER_TEMP/binary/u under `if: (none)`, not the `if:` of the step that unpacks it, `{condition}`"
        )]
    );
}

#[test]
fn the_floors_assets_are_the_published_release_it_names() {
    // Catches: the recorded assets and the floor drifting apart (the floor moved without the
    // file, or the file without the floor), a file not sorted, or a file that lists the
    // coordinator updater, which the floor predates.
    let mut lines = FLOOR_FILE.lines();
    assert_eq!(
        lines.next(),
        Some(
            format!(
                "# The asset names of release {} (commit {}), its version written <version>, as gh release view lists them.",
                FLOOR.0, FLOOR.1
            )
            .as_str()
        )
    );
    let body: Vec<&str> = lines.collect();
    let mut sorted = body.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(body, sorted);
    assert!(body.contains(&HOSTKEEPER));
    assert!(!body.contains(&UPDATER));
}
