//! The consumer document, `docs/deploy/verify-a-release.md`, held to what the release tooling
//! does:
//!
//! - its list of every asset name ("Every name"), with the platform placeholders written out for
//!   every platform its sentence lists ([`documented_names`]), is [`asset_names`]; the test is
//!   `contract::tests::asset_names_are_the_documented_contract`;
//! - the commands it gives a consumer are the ones the `verify` job of `release.yml` runs
//!   ([`command_problems`]): step 2 in the job, steps 3 and 4 as `release check` runs them through
//!   [`Gh`], and the outputs of step 6 as `release check-binary` holds the shipped binaries
//!   ([`PROGRAMS`]) to.
//!
//! Each check is shown to refuse a change to either side alone. Everything is read as text: no
//! command runs.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use super::contract::{Commit, PROGRAMS, Platform, Signer, Version, asset_names, stamp_of};
use super::github::Gh;
use super::ports::{Exec, Ran, Verifier};

/// The document, as committed.
pub(super) const DOC: &str = include_str!("../../../../docs/deploy/verify-a-release.md");

/// The release workflow, as committed.
const RELEASE_YML: &str = include_str!("../../../../.github/workflows/release.yml");

/// The heading of the list of every name.
const EVERY_NAME: &str = "\n### Every name\n";

/// The start of the sentence that lists the platforms and their deb and rpm architectures.
const PLATFORMS: &str = "`<platform>` is ";

/// What the document writes for the commit a consumer pinned.
const PIN: &str = "\"$PIN\"";

/// The flag of step 4 the release job leaves out: the runner fetches the trusted root itself.
const TRUSTED_ROOT: [&str; 2] = ["--custom-trusted-root", "trusted_root.jsonl"];

/// What the release job adds to the commands of steps 3 and 4, to read back what verified.
const JSON: [&str; 2] = ["--format", "json"];

/// The text of `doc` from the heading `heading` (which starts with a line break) up to the next
/// heading.
fn section<'a>(doc: &'a str, heading: &str) -> Option<&'a str> {
    let start = doc.find(heading)? + 1;
    let rest = &doc[start..];
    let end = rest[1..].find("\n#").map_or(rest.len(), |at| at + 1);
    Some(&rest[..end])
}

/// The contents of the fenced blocks of `text` that open with ```` ```kind ````.
fn blocks<'a>(text: &'a str, kind: &str) -> Vec<&'a str> {
    text.split(&format!("```{kind}\n"))
        .skip(1)
        .map(|b| b.split("```").next().unwrap_or_default())
        .collect()
}

/// The lines of a shell block, each continued line (ending in `\`) joined to the next.
fn logical_lines(block: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in block.lines() {
        match line.strip_suffix('\\') {
            Some(start) => current.push_str(start),
            None => {
                current.push_str(line);
                lines.push(std::mem::take(&mut current));
            }
        }
    }
    lines
}

/// The words of the first command of the `sh` blocks of `section` that starts with `start`.
fn command(section: &str, start: &str) -> Option<Vec<String>> {
    blocks(section, "sh")
        .into_iter()
        .flat_map(logical_lines)
        .find(|l| l.trim_start().starts_with(start))
        .map(|l| l.split_whitespace().map(str::to_owned).collect())
}

/// The texts between backquotes in `text`.
fn quoted(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// Each placeholder of the platform sentence with its values, in order: `<platform>`,
/// `<deb-arch>` and `<rpm-arch>`. None when the sentence is missing or a clause of it quotes
/// nothing.
fn placeholders(doc: &str) -> Option<Vec<(String, Vec<String>)>> {
    let (_, rest) = doc.split_once(PLATFORMS)?;
    let sentence = format!(
        "{PLATFORMS}{}`",
        rest.split("`.").next().unwrap_or_default()
    );
    sentence
        .split(';')
        .map(|clause| {
            let words = quoted(clause);
            let (name, values) = words.split_first()?;
            Some((name.clone(), values.to_vec()))
        })
        .collect()
}

/// Every asset name `doc` lists for `version`, sorted: each line of the list under
/// "Every name" with `<version>` written out, once when it holds no placeholder of the platform
/// sentence and otherwise once per value of the placeholders it holds, taken together. Fails
/// saying what of the document is missing, or which line holds placeholders of unequal length.
pub(super) fn documented_names(doc: &str, version: &Version) -> Result<Vec<String>, String> {
    let list = section(doc, EVERY_NAME)
        .and_then(|s| blocks(s, "text").into_iter().next())
        .ok_or("the document has no ```text block under \"### Every name\"")?;
    let places = placeholders(doc).ok_or("the document has no sentence listing the platforms")?;
    let mut names = Vec::new();
    for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let line = line.replace("<version>", &version.to_string());
        let here: Vec<&(String, Vec<String>)> = places
            .iter()
            .filter(|(name, _)| line.contains(name.as_str()))
            .collect();
        let count = here.first().map_or(1, |(_, values)| values.len());
        if here.iter().any(|(_, values)| values.len() != count) {
            return Err(format!(
                "the line {line} holds {here:?}, not as many of each"
            ));
        }
        for i in 0..count {
            names.push(here.iter().fold(line.clone(), |text, (name, values)| {
                text.replace(name.as_str(), &values[i])
            }));
        }
    }
    names.sort();
    Ok(names)
}

/// An [`Exec`] that records each command line and starts nothing.
#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<Vec<String>>>>);

impl Exec for Recorder {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let mut line = vec![program.to_owned()];
        line.extend(args.iter().cloned());
        self.0.borrow_mut().push(line);
        Err(std::io::Error::other("recorded, not started"))
    }
}

/// The command line `release check` runs to verify `dist/<file>` for `commit`: online, or with
/// its bundle.
fn verify_line(file: &str, offline: bool, commit: &Commit) -> Vec<String> {
    let recorder = Recorder::default();
    let gh = Gh::new(recorder.clone());
    let path = Path::new("dist").join(file);
    let bundle = Path::new("dist").join(format!("{file}.sigstore.json"));
    let _not_started = gh.verify(
        &Signer::hostkeeper(),
        &path,
        offline.then_some(bundle.as_path()),
        commit,
    );
    let lines = recorder.0.borrow();
    lines.first().cloned().unwrap_or_default()
}

/// The text of the job `name` of `workflow`: from the line after its key up to the next job's
/// key (a line indented by exactly two spaces). Empty when there is no such job.
fn job<'a>(workflow: &'a str, name: &str) -> &'a str {
    let head = format!("\n  {name}:\n");
    let Some(start) = workflow.find(&head) else {
        return "";
    };
    let body = &workflow[start + head.len()..];
    let end = body
        .match_indices('\n')
        .map(|(at, _)| at + 1)
        .find(|&at| {
            body[at..].starts_with("  ")
                & body
                    .get(at + 2..)
                    .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphanumeric()))
        })
        .unwrap_or(body.len());
    &body[..end]
}

/// One line per way the commands of the document `doc` and those the `verify` job of
/// `release_yml` runs (itself, and through `release check` and `release check-binary`) differ.
fn command_problems(doc: &str, release_yml: &str) -> Vec<String> {
    let version: Version = "0.1.5".parse().expect("a version");
    let commit: Commit = "0123456789abcdef0123456789abcdef01234567"
        .parse()
        .expect("a commit");
    let asset = format!("hostkeeper-{version}-linux-x86_64.tar.gz");
    let verify = job(release_yml, "verify");
    let in_job = |line: &str| verify.lines().any(|l| l.trim() == line);
    let mut problems = Vec::new();

    // Step 2, run by the job itself on the whole download.
    match section(doc, "\n### 2. ").and_then(|s| command(s, "cd release && sha256sum")) {
        Some(words) => {
            let words: Vec<&str> = words
                .iter()
                .skip(3)
                .map(String::as_str)
                .filter(|w| *w != "--ignore-missing")
                .collect();
            let want = format!("run: {}", words.join(" "));
            if !in_job("working-directory: dist") | !in_job(&want) {
                problems.push(format!("the verify job does not `{want}` in dist (step 2)"));
            }
        }
        None => problems.push("the document has no command in step 2".to_owned()),
    }

    // Steps 3 and 4, run by `release check` on each file of dist.
    let named = |words: Vec<String>| -> Vec<String> {
        words
            .into_iter()
            .map(|w| match w.as_str() {
                "release/<asset>" => format!("dist/{asset}"),
                "release/<asset>.sigstore.json" => format!("dist/{asset}.sigstore.json"),
                PIN => commit.to_string(),
                _ => w,
            })
            .chain(JSON.map(str::to_owned))
            .collect()
    };
    match section(doc, "\n### 3. ").and_then(|s| command(s, "gh attestation verify")) {
        Some(words) => {
            let want = named(words);
            let got = verify_line(&asset, false, &commit);
            if got != want {
                problems.push(format!(
                    "release check runs `{}`, not step 3's `{}`",
                    got.join(" "),
                    want.join(" ")
                ));
            }
        }
        None => problems.push("the document has no command in step 3".to_owned()),
    }
    match section(doc, "\n### 4. ").and_then(|s| command(s, "gh attestation verify")) {
        Some(mut words) => {
            let at = words.windows(2).position(|w| w == TRUSTED_ROOT);
            if let Some(at) = at {
                words.drain(at..at + 2);
            }
            let want = named(words);
            let got = verify_line(&asset, true, &commit);
            if at.is_none() | (got != want) {
                problems.push(format!(
                    "release check runs `{}`, not step 4's `{}` without `{}`",
                    got.join(" "),
                    want.join(" "),
                    TRUSTED_ROOT.join(" ")
                ));
            }
        }
        None => problems.push("the document has no command in step 4".to_owned()),
    }
    let check = "run: cargo xtask release check dist --version \"$RELEASE_VERSION\" \
                 --commit \"$COMMIT\"";
    if !in_job(check) {
        problems.push(format!("the verify job does not `{check}`"));
    }

    // Step 6: the outputs `check-binary` holds the shipped binaries to, and the job checks each.
    let mut said: Vec<String> = quoted(section(doc, "\n### 6. ").unwrap_or_default())
        .into_iter()
        .filter(|q| q.contains("<X.Y.Z>+"))
        .map(|q| {
            q.replace("<X.Y.Z>", &version.to_string())
                .replace("<first 12 hex digits of $PIN>", commit.short())
        })
        .collect();
    said.sort();
    said.dedup();
    let want: Vec<String> = PROGRAMS
        .iter()
        .map(|p| stamp_of(p, &version, &commit))
        .collect();
    if said != want {
        problems.push(format!(
            "step 6 expects {said:?}, not what check-binary holds the binaries to, {want:?}"
        ));
    }
    for program in PROGRAMS {
        let line = format!(
            "run: cargo xtask release check-binary \"$RUNNER_TEMP/binary/{program}\" \
             \"$RELEASE_VERSION\" \"$COMMIT\""
        );
        if !in_job(&line) {
            problems.push(format!("the verify job does not check {program} (step 6)"));
        }
    }
    problems
}

/// `text` with the one occurrence of `from` replaced by `to`.
fn changed(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "{from:?} is not there once");
    text.replacen(from, to, 1)
}

#[test]
fn verify_a_release_commands_match_the_ones_the_verify_job_runs() {
    // Catches: the consumer document and the release job drifting apart, from either side: a
    // flag of step 3 or 4 that `release check` does not pass (or passes and the document does
    // not show), a checksum the job does not run, an expected `--version` output that
    // `check-binary` does not hold a binary to, or a binary the job does not check.
    assert_eq!(command_problems(DOC, RELEASE_YML), Vec::<String>::new());

    let doc_cases = [
        (
            "  --source-digest \"$PIN\" \\\n  --deny-self-hosted-runners\n```\n\n- `--repo`",
            "  --deny-self-hosted-runners\n```\n\n- `--repo`",
            "not step 3's",
        ),
        (
            "  --custom-trusted-root trusted_root.jsonl \\\n",
            "",
            "not step 4's",
        ),
        (
            "--bundle release/<asset>.sigstore.json \\\n",
            "--bundle release/<asset>.bundle \\\n",
            "not step 4's",
        ),
        (
            "cd release && sha256sum --check --strict --ignore-missing SHA256SUMS",
            "cd release && sha256sum --check --ignore-missing SHA256SUMS",
            "does not `run: sha256sum --check SHA256SUMS` in dist",
        ),
        (
            "`hostkeeper-coordinator <X.Y.Z>+<first 12 hex digits of $PIN>`.\n",
            "`hostkeeper-coordinator <X.Y.Z>+<first 8 hex digits of $PIN>`.\n",
            "step 6 expects",
        ),
        (
            "`hostkeeper-mdm <X.Y.Z>+<first 12 hex digits of $PIN>`.\n",
            "`hostkeeper-mdm <X.Y.Z>`.\n",
            "step 6 expects",
        ),
        (
            "\n### 2. Checksums",
            "\n#### 2. Checksums",
            "no command in step 2",
        ),
        (
            "\n### 3. Attestation, online",
            "\n#### 3. Attestation, online",
            "no command in step 3",
        ),
        (
            "\n### 4. Attestation, offline",
            "\n#### 4. Attestation, offline",
            "no command in step 4",
        ),
    ];
    for (from, to, want) in doc_cases {
        let found = command_problems(&changed(DOC, from, to), RELEASE_YML);
        assert!(
            found.iter().any(|p| p.contains(want)),
            "{from:?} -> {to:?}: {found:?}"
        );
    }

    let yml_cases = [
        (
            "        working-directory: dist\n        run: sha256sum --check --strict SHA256SUMS\n      - name: Verify every file and attestation as a consumer does",
            "        working-directory: dist\n        run: sha256sum --check SHA256SUMS\n      - name: Verify every file and attestation as a consumer does",
            "does not `run: sha256sum --check --strict SHA256SUMS` in dist",
        ),
        (
            "\"$RUNNER_TEMP/binary/hostkeeper-coordinator\" \"$RELEASE_VERSION\" \"$COMMIT\"",
            "\"$RUNNER_TEMP/binary/hostkeeper-coordinator\" \"$RELEASE_VERSION\" \"$GITHUB_SHA\"",
            "does not check hostkeeper-coordinator",
        ),
        (
            "\"$RUNNER_TEMP/binary/hostkeeper-mdm\" \"$RELEASE_VERSION\" \"$COMMIT\"",
            "\"$RUNNER_TEMP/binary/hostkeeper-mdm\" \"$RELEASE_VERSION\" \"$GITHUB_SHA\"",
            "does not check hostkeeper-mdm",
        ),
        (
            "--commit \"$COMMIT\"\n      # The runner is linux-x86_64",
            "--commit \"$GITHUB_SHA\"\n      # The runner is linux-x86_64",
            "does not `run: cargo xtask release check dist",
        ),
        (
            "\n  verify:\n",
            "\n  checks:\n",
            "does not check hostkeeper (step 6)",
        ),
    ];
    for (from, to, want) in yml_cases {
        let found = command_problems(DOC, &changed(RELEASE_YML, from, to));
        assert!(
            found.iter().any(|p| p.contains(want)),
            "{from:?} -> {to:?}: {found:?}"
        );
    }
}

#[test]
fn a_document_without_the_list_or_the_platforms_has_no_names() {
    // Catches: a name check that passes because it found nothing to compare.
    let v: Version = "0.1.5".parse().expect("a version");
    let without = |from: &str, to: &str| documented_names(&changed(DOC, from, to), &v);
    assert_eq!(
        without("### Every name", "### All names"),
        Err("the document has no ```text block under \"### Every name\"".to_owned())
    );
    let no_sentence = Err("the document has no sentence listing the platforms".to_owned());
    assert_eq!(without(PLATFORMS, "`<platforms>` are "), no_sentence);
    let rpm = "`<rpm-arch>` is `x86_64` or `aarch64`";
    assert_eq!(without(rpm, "rpm-arch is x86_64 or aarch64"), no_sentence);
    let mixed = without(
        "hostkeeper-<version>-1.<rpm-arch>.rpm\n",
        "hostkeeper-<version>-1.<rpm-arch>.<platform>.rpm\n",
    );
    assert!(mixed.is_err_and(|e| e.contains("not as many of each")));
}

#[test]
fn the_platform_sentence_lists_the_released_platforms() {
    // Catches: a document that names a platform the release does not ship, or misses one, pairs
    // a platform with another's deb or rpm architecture, misses the Arch package, or lists the coordinator or the MDM kit
    // for macOS.
    fn values(list: &[&str]) -> Vec<String> {
        list.iter().map(|v| (*v).to_owned()).collect()
    }
    let places = placeholders(DOC).expect("the platform sentence");
    let released: Vec<&str> = Platform::RELEASED.iter().map(|p| p.name()).collect();
    assert_eq!(
        places,
        vec![
            ("<platform>".to_owned(), values(&released)),
            (
                "<linux-platform>".to_owned(),
                values(&["linux-x86_64", "linux-arm64"])
            ),
            ("<deb-arch>".to_owned(), values(&["amd64", "arm64"])),
            ("<rpm-arch>".to_owned(), values(&["x86_64", "aarch64"])),
        ]
    );
    let v: Version = "0.1.5".parse().expect("a version");
    let names = documented_names(DOC, &v).expect("the names");
    for name in [
        "hostkeeper_0.1.5_amd64.deb",
        "hostkeeper-0.1.5-1.aarch64.rpm",
        "hostkeeper-0.1.5-1-aarch64.pkg.tar.zst",
        "hostkeeper-0.1.5-1-x86_64.pkg.tar.zst.sigstore.json",
        "hostkeeper-coordinator-0.1.5-linux-arm64.image.tar",
        "hostkeeper-mdm-0.1.5-linux-x86_64.tar.gz",
        "hostkeeper-mdm-0.1.5-linux-arm64.cdx.json",
        "hostkeeper-0.1.5-macos-arm64.pkg",
    ] {
        assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
    }
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("hostkeeper-coordinator-0.1.5-macos")
                | n.starts_with("hostkeeper-mdm-0.1.5-macos")),
        "{names:?}"
    );
    assert_eq!(names.len(), asset_names(&v, &Platform::RELEASED).len());
}
