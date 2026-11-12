//! The rule of Debian packages ([`super::debs`]): no apt, no host of Canonical's, and pinned
//! sets installed in their four steps. Each rule is shown to refuse its defect, on a small
//! workflow and on the repository's own.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::debs::{CANONICAL, names_canonical, problems, runs_apt};
use super::lint::{entries, entry, lint};
use super::workflows;
use super::yaml::{self, Node, Value};

const CACHE: &str = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9";

/// The four steps of the set `set`, under the restore id `debs`.
fn four(set: &str) -> String {
    format!(
        "      - uses: actions/cache/restore@{CACHE} # v6.1.0\n\
         \x20       id: debs\n\
         \x20       with:\n\
         \x20         path: ${{{{ runner.temp }}}}/debs/{set}\n\
         \x20         key: debs-{set}-${{{{ runner.arch }}}}-${{{{ hashFiles('.github/debs/{set}.toml') }}}}\n\
         \x20     - name: Check the pinned packages\n\
         \x20       run: cargo xtask debs --pins .github/debs/{set}.toml --dir \"$RUNNER_TEMP/debs/{set}\"\n\
         \x20     - uses: actions/cache/save@{CACHE} # v6.1.0\n\
         \x20       if: ${{{{ steps.debs.outputs.cache-hit != 'true' }}}}\n\
         \x20       with:\n\
         \x20         path: ${{{{ runner.temp }}}}/debs/{set}\n\
         \x20         key: ${{{{ steps.debs.outputs.cache-primary-key }}}}\n\
         \x20     - name: Install\n\
         \x20       run: sudo dpkg --install \"$RUNNER_TEMP\"/debs/{set}/*.deb\n"
    )
}

/// A workflow of one job `j` whose steps are `steps`.
fn workflow(steps: &str) -> String {
    format!(
        "name: t\non: push\npermissions: {{}}\njobs:\n  j:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: rustup toolchain install\n{steps}"
    )
}

/// The problems of the rule for the workflow `text`.
fn found(text: &str) -> Vec<String> {
    problems(&yaml::load(text).expect("the workflow parses"))
}

/// `four("qemu")` with `from` replaced by `to` (once; it must be there).
fn four_with(from: &str, to: &str) -> String {
    let steps = four("qemu");
    assert!(steps.contains(from), "{from}");
    workflow(&steps.replacen(from, to, 1))
}

const RESTORE_REFUSED: &str = "job j: the pinned set qemu: the step before its check is not `actions/cache/restore@` with an `id:`, no `if:`, and `with:` only `path: ${{ runner.temp }}/debs/qemu` and `key: debs-qemu-${{ runner.arch }}-${{ hashFiles('.github/debs/qemu.toml') }}`";
const SAVE_REFUSED: &str = "job j: the pinned set qemu: the step after its check is not `actions/cache/save@` with `if: ${{ steps.debs.outputs.cache-hit != 'true' }}` and `with:` only `path: ${{ runner.temp }}/debs/qemu` and `key: ${{ steps.debs.outputs.cache-primary-key }}`";
const INSTALL_REFUSED: &str = "job j: the pinned set qemu: the second step after its check is not `run: sudo dpkg --install \"$RUNNER_TEMP\"/debs/qemu/*.deb` with no `if:`";

#[test]
fn the_four_steps_pass() {
    // Catches: a rule that refuses the shape it asks for, for any set name it allows.
    for set in ["qemu", "qemu-x86", "0ad"] {
        assert_eq!(found(&workflow(&four(set))), Vec::<String>::new(), "{set}");
    }
    assert_eq!(found(&workflow("")), Vec::<String>::new());
}

#[test]
fn apt_is_refused_in_every_spelling() {
    // Catches: apt let through by `sudo` and its options, `env`, an assignment, a chain, a pipe,
    // a later line, `bash -c`, or another of apt's programs; and `cargo xtask channels apt`,
    // `check-apt`, `dpkg-query` or the word apt in an echo taken for apt.
    for run in [
        "sudo apt-get update",
        "apt-get install --yes ovmf",
        "sudo apt install --yes ovmf",
        "sudo -E apt-get install ovmf",
        "sudo -E -H apt update",
        "DEBIAN_FRONTEND=noninteractive apt-get install ovmf",
        "sudo env DEBIAN_FRONTEND=noninteractive apt install ovmf",
        "true && sudo apt-get update",
        "false || apt update",
        "echo y | sudo apt-get install ovmf",
        "bash -c \"apt-get install ovmf\"",
        "apt-cache policy ovmf",
        "sudo aptitude install ovmf",
        "sudo add-apt-repository ppa:x/y",
        "true\nsudo apt-get update",
        "sudo -u root apt install ovmf",
        "/usr/bin/apt install ovmf",
        "sudo bash -c 'apt install -y ovmf'",
        "timeout 60 apt update",
        "echo ovmf | xargs apt install",
    ] {
        assert!(runs_apt(run), "{run}");
        let block = run.replace('\n', "\n          ");
        let text = workflow(&format!("      - run: |\n          {block}\n"));
        assert_eq!(
            found(&text),
            [format!(
                "job j: the step `{run}` runs apt: install a pinned set of .github/debs with `cargo xtask debs` and dpkg"
            )],
            "{run}"
        );
    }
    for run in [
        "cargo xtask channels apt read --suite testing --bucket b --out o",
        "cargo xtask channels check-apt --package p --arch amd64 --version 1.2.3 --commit c",
        "dpkg-query --show ovmf",
        "echo aptly",
    ] {
        assert!(!runs_apt(run), "{run}");
        assert_eq!(
            found(&workflow(&format!("      - run: {run}\n"))),
            Vec::<String>::new()
        );
    }
}

#[test]
fn a_host_of_canonicals_is_refused_in_any_value_and_only_as_a_host() {
    // Catches: a step that fetches from Canonical's names by a run, an env value or an action's
    // input, and a host of Canonical's taken from a longer name that only contains one.
    for value in [
        "https://releases.ubuntu.com/24.04/x.iso",
        "http://archive.ubuntu.com/ubuntu",
        "ubuntu.com",
        "https://launchpad.net/~x",
        "ppa.launchpadcontent.net",
        "https://www.canonical.com/",
        "HTTPS://Releases.Ubuntu.COM/x.iso",
        "https://archive.ubuntu.com./ubuntu",
    ] {
        assert!(names_canonical(value), "{value}");
        let refused = format!(
            "`{value}` names a host of Canonical's: fetch from the pinned sources of .github/debs or the ISO's mirrors"
        );
        for steps in [
            format!("      - run: curl -o x \"{value}\"\n"),
            format!("      - env:\n          URL: {value}\n        run: true\n"),
            format!("      - uses: actions/x@{CACHE}\n        with:\n          url: {value}\n"),
        ] {
            let text = workflow(&steps);
            let want = if steps.contains("curl") {
                format!(
                    "`curl -o x \"{value}\"` names a host of Canonical's: fetch from the pinned sources of .github/debs or the ISO's mirrors"
                )
            } else {
                refused.clone()
            };
            assert_eq!(found(&text), [want], "{steps}");
        }
    }
    // Built from pieces, as names of real-looking hosts this file must not spell.
    let longer = [
        format!("https://not{}/x", CANONICAL[0]),
        format!("https://{}munity/x", CANONICAL[0]),
        format!("my-{}work", CANONICAL[1]),
    ];
    for value in longer
        .iter()
        .map(String::as_str)
        .chain([
            "ubuntu-24.04",
            "https://mirrors.edge.kernel.org/ubuntu-releases",
        ])
        .chain(
            [format!("https://{}.example/x", CANONICAL[0])]
                .iter()
                .map(String::as_str),
        )
    {
        assert!(!names_canonical(value), "{value}");
    }
    assert_eq!(
        CANONICAL,
        [
            "ubuntu.com",
            "launchpad.net",
            "launchpadcontent.net",
            "canonical.com"
        ]
    );
}

#[test]
fn a_dpkg_command_outside_the_four_steps_is_refused() {
    // Catches: packages installed from a directory no pinned set was checked into, by any
    // spelling of dpkg, or a dpkg install whose check is not two steps before it.
    for run in [
        "sudo dpkg --install x.deb",
        "sudo dpkg -i \"$RUNNER_TEMP\"/debs/qemu/*.deb",
        "dpkg --purge ovmf",
        "sudo /usr/bin/dpkg --install x.deb",
        "sudo -u root dpkg -i x.deb",
        "sudo sh -c 'dpkg -i x.deb'",
    ] {
        assert_eq!(
            found(&workflow(&format!("      - run: {run}\n"))),
            [format!(
                "job j: the dpkg step `{run}` does not install a pinned set: it is the fourth of restore, `cargo xtask debs`, save and install"
            )],
            "{run}"
        );
    }
    let moved = format!("{}      - run: true\n", four("qemu"));
    let install = "      - name: Install\n        run: sudo dpkg --install \"$RUNNER_TEMP\"/debs/qemu/*.deb\n";
    let late = moved.replacen(install, "", 1) + install;
    assert_eq!(
        found(&workflow(&late)),
        [
            INSTALL_REFUSED.to_owned(),
            "job j: the dpkg step `sudo dpkg --install \"$RUNNER_TEMP\"/debs/qemu/*.deb` does not install a pinned set: it is the fourth of restore, `cargo xtask debs`, save and install".to_owned(),
        ]
    );
}

#[test]
fn a_debs_step_of_another_shape_is_refused() {
    // Catches: a check of a set chained to another command, with another flag or directory,
    // pins outside .github/debs, a set name the rule does not allow, a multi-line block, or an
    // `if:` that skips it.
    let call = "cargo xtask debs --pins .github/debs/qemu.toml --dir \"$RUNNER_TEMP/debs/qemu\"";
    let not_shaped = |run: &str| {
        format!(
            "job j: the step `{run}` is not `cargo xtask debs --pins .github/debs/<set>.toml --dir \"$RUNNER_TEMP/debs/<set>\"` of one set"
        )
    };
    for run in [
        format!("{call} && true"),
        call.replace(
            "--dir \"$RUNNER_TEMP/debs/qemu\"",
            "--dir \"$RUNNER_TEMP/debs/other\"",
        ),
        call.replace(".github/debs/qemu.toml", "debs/qemu.toml"),
        call.replace("debs/qemu", "debs/Qemu"),
        call.replace("debs/qemu.toml", "debs/-qemu.toml"),
        call.replace("cargo xtask debs", "cargo xtask debs --ca-bundle b.pem"),
    ] {
        let text = four_with(call, &run);
        let got = found(&text);
        assert_eq!(
            got[0], "job j: a restore of a debs/ path is not followed by its `cargo xtask debs`",
            "{run}"
        );
        assert!(got.contains(&not_shaped(&run)), "{run}: {got:?}");
    }
    let block = format!("|\n          {call}\n          true\n");
    let text = four_with(&format!("run: {call}\n"), &format!("run: {block}"));
    assert!(
        found(&text).contains(&not_shaped(&format!("{call}\ntrue"))),
        "{:?}",
        found(&text)
    );
    let guarded = four_with(
        "      - name: Check the pinned packages\n",
        "      - name: Check the pinned packages\n        if: ${{ github.ref == 'refs/heads/main' }}\n",
    );
    assert_eq!(
        found(&guarded),
        ["job j: the pinned set qemu: its check has an `if:`"]
    );
}

#[test]
fn a_restore_of_another_shape_is_refused() {
    // Catches: a check with no restore before it, or a restore without an id, under an `if:`,
    // with `restore-keys` (a stale set restored), at another path, or under a key without the
    // pins file's hash, of another set, or without the runner's architecture.
    let restore_line = format!("      - uses: actions/cache/restore@{CACHE} # v6.1.0\n");
    let cases = [
        four_with(&restore_line, "      - uses: actions/checkout@x\n"),
        four_with("        id: debs\n", ""),
        four_with(
            "        id: debs\n",
            "        id: debs\n        if: ${{ always() }}\n",
        ),
        four_with(
            "          key: debs-qemu-",
            "          restore-keys: debs-qemu-\n          key: debs-qemu-",
        ),
        four_with(
            "          path: ${{ runner.temp }}/debs/qemu\n          key: debs",
            "          path: ${{ runner.temp }}/debs/other\n          key: debs",
        ),
        four_with("-${{ hashFiles('.github/debs/qemu.toml') }}", ""),
        four_with(
            ".github/debs/qemu.toml') }}\n",
            ".github/debs/other.toml') }}\n",
        ),
        four_with("-${{ runner.arch }}", ""),
    ];
    for text in &cases {
        assert!(
            found(text).contains(&RESTORE_REFUSED.to_owned()),
            "{text}\n{:?}",
            found(text)
        );
    }
    // Without an id the save's condition cannot name it either.
    assert_eq!(
        found(&four_with("        id: debs\n", "")),
        [
            RESTORE_REFUSED.to_owned(),
            SAVE_REFUSED.replace("steps.debs.", "steps.."),
        ]
    );
    let alone = workflow(&format!(
        "{restore_line}        id: x\n        with:\n          path: ${{{{ runner.temp }}}}/debs/qemu\n          key: k\n"
    ));
    assert_eq!(
        found(&alone),
        ["job j: a restore of a debs/ path is not followed by its `cargo xtask debs`"]
    );
}

#[test]
fn a_save_or_an_install_of_another_shape_is_refused() {
    // Catches: a set never saved, saved on a hit or whatever happened, keyed to another step's
    // hit or another key, at another path; and a set not installed, installed from another
    // directory or under an `if:`.
    let save = "        if: ${{ steps.debs.outputs.cache-hit != 'true' }}\n";
    for text in [
        four_with(
            &format!("      - uses: actions/cache/save@{CACHE} # v6.1.0\n"),
            "      - uses: actions/upload-artifact@x\n",
        ),
        four_with(save, ""),
        four_with(save, "        if: ${{ always() }}\n"),
        four_with(
            save,
            "        if: ${{ steps.other.outputs.cache-hit != 'true' }}\n",
        ),
        four_with(
            "key: ${{ steps.debs.outputs.cache-primary-key }}",
            "key: debs-qemu",
        ),
        four_with(
            "          path: ${{ runner.temp }}/debs/qemu\n          key: ${{",
            "          path: ${{ runner.temp }}/debs\n          key: ${{",
        ),
    ] {
        assert_eq!(found(&text), [SAVE_REFUSED], "{text}");
    }
    let install = "      - name: Install\n        run: sudo dpkg --install \"$RUNNER_TEMP\"/debs/qemu/*.deb\n";
    assert_eq!(
        found(&four_with(install, "      - run: true\n")),
        [INSTALL_REFUSED]
    );
    let guarded = four_with(
        "      - name: Install\n",
        "      - name: Install\n        if: ${{ success() }}\n",
    );
    assert_eq!(found(&guarded), [INSTALL_REFUSED]);
    let other = four_with("/debs/qemu/*.deb", "/debs/other/*.deb");
    assert_eq!(found(&other)[0], INSTALL_REFUSED);
}

#[test]
fn a_step_of_the_four_that_could_undo_the_check_is_refused() {
    // Catches: a failed check let pass (`continue-on-error`), a step run by another shell or in
    // another directory, or `RUNNER_TEMP` set to another directory, on any of the four.
    for (from, to, what) in [
        (
            "      - name: Check the pinned packages\n",
            "      - name: Check the pinned packages\n        continue-on-error: true\n",
            "continue-on-error",
        ),
        (
            "      - name: Install\n",
            "      - name: Install\n        shell: sh\n",
            "shell",
        ),
        (
            "        id: debs\n",
            "        id: debs\n        working-directory: x\n",
            "working-directory",
        ),
        (
            "      - name: Install\n",
            "      - name: Install\n        env:\n          RUNNER_TEMP: /x\n",
            "env: RUNNER_TEMP",
        ),
    ] {
        let got = found(&four_with(from, to));
        assert!(
            got.contains(&format!(
                "job j: the pinned set qemu: a step of its four sets `{what}`"
            )),
            "{what}: {got:?}"
        );
    }
}

/// The sets the repository's workflows install, as `<file> <job> <set>`.
fn installed_sets() -> Vec<String> {
    let mut out = Vec::new();
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("a workflow");
        for (name, job) in entry(&doc, "jobs").map_or(&[][..], entries) {
            let Some(Value::Seq(steps)) = entry(job, "steps").map(|s| &s.value) else {
                continue;
            };
            for step in steps {
                let run = entry(step, "run")
                    .and_then(Node::as_str)
                    .unwrap_or_default();
                if let Some(set) = run
                    .strip_prefix("cargo xtask debs --pins .github/debs/")
                    .and_then(|r| r.split_once(".toml"))
                {
                    out.push(format!(
                        "{file} {} {}",
                        name.as_str().unwrap_or_default(),
                        set.0
                    ));
                }
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_workflow_keeps_the_rule_and_installs_exactly_the_committed_sets() {
    // Catches: a rule that does not read the repository's workflows, a set a workflow installs
    // with no pins file, a pins file no workflow installs, and the repository's net-install.yml
    // with apt put back, a mirror of Canonical's for its ISO, or its save made unconditional.
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("a workflow");
        assert_eq!(problems(&doc), Vec::<String>::new(), "{file}");
    }
    assert_eq!(
        installed_sets(),
        [
            "net-install.yml net-install qemu-x86",
            "net-install.yml net-install-arm64 qemu-arm"
        ]
    );
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/debs");
    let files: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("read .github/debs")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        files,
        BTreeSet::from(["qemu-arm.toml".to_owned(), "qemu-x86.toml".to_owned()])
    );

    let net = workflows()
        .into_iter()
        .find(|(file, _)| file == "net-install.yml")
        .expect("net-install.yml")
        .1;
    let back = net.replacen(
        "      - run: rustup toolchain install\n",
        "      - run: sudo apt-get update\n      - run: rustup toolchain install\n",
        1,
    );
    assert!(
        lint("net-install.yml", &back)
            .iter()
            .any(|p| p.contains("runs apt"))
    );
    let canonical = net.replacen(
        "--mirror https://mirrors.edge.kernel.org/ubuntu-releases",
        "--mirror https://releases.ubuntu.com",
        1,
    );
    assert_ne!(canonical, net);
    assert!(
        lint("net-install.yml", &canonical)
            .iter()
            .any(|p| p.contains("names a host of Canonical's"))
    );
    let always = net.replacen(
        "if: ${{ steps.qemu-debs.outputs.cache-hit != 'true' }}",
        "if: ${{ always() }}",
        1,
    );
    assert_ne!(always, net);
    assert!(
        lint("net-install.yml", &always)
            .iter()
            .any(|p| p.contains("the pinned set qemu-x86: the step after its check"))
    );
}
