//! The table of the programs against the files that spell it out too: the signing job of
//! `artifacts.yml` and hostkeeper's embedded `Info.plist`.

use std::path::PathBuf;

use super::{APPLICATION_IDENTITY, MAC_PROGRAMS, MacProgram};

fn repo_file(path: &str) -> String {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
}

/// The text of the job `name` of `artifacts.yml`: from its header to the next job's.
fn job(workflow: &str, name: &str) -> String {
    let header = format!("\n  {name}:\n");
    let start = workflow
        .find(&header)
        .unwrap_or_else(|| panic!("artifacts.yml has no job {name}"));
    let rest = &workflow[start + header.len()..];
    let mut end = rest.len();
    let mut at = 0;
    for line in rest.lines() {
        let is_job = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.starts_with("  #")
            && line.trim_end().ends_with(':');
        if is_job {
            end = at;
            break;
        }
        at += line.len() + 1;
    }
    rest[..end].to_owned()
}

#[test]
fn the_signing_job_signs_each_program_of_the_table_under_its_identifier_and_the_identity() {
    // Catches: a program added to or renamed in the table without the workflow signing it under
    // that identifier (or the reverse), a program signed without the hardened runtime or a
    // timestamp, or the job's identity drifting from the table's.
    let sign = job(&repo_file(".github/workflows/artifacts.yml"), "sign-code");
    assert!(
        sign.contains(&format!("      IDENTITY: \"{APPLICATION_IDENTITY}\"\n")),
        "sign-code's IDENTITY is not {APPLICATION_IDENTITY}"
    );
    let signings: Vec<&str> = sign
        .lines()
        .filter(|l| l.contains("codesign --force"))
        .collect();
    assert_eq!(signings.len(), MAC_PROGRAMS.len(), "{signings:?}");
    for program in MAC_PROGRAMS {
        let wanted = format!(
            "--identifier {} --options runtime --timestamp signed/{}",
            program.identifier, program.name
        );
        let lines: Vec<&&str> = signings.iter().filter(|l| l.ends_with(&wanted)).collect();
        assert_eq!(
            lines.len(),
            1,
            "{} is not signed as `{wanted}`",
            program.name
        );
        assert!(
            lines[0].contains("--sign \"$IDENTITY\""),
            "{} is not signed with the job's identity",
            program.name
        );
        assert!(
            lines[0].contains(&format!("cp unsigned/{0} signed/{0} &&", program.name)),
            "{} is not copied from unsigned/",
            program.name
        );
    }
}

#[test]
fn the_job_finder_stops_at_the_next_job_and_skips_comments() {
    // Catches: a job text that runs on into the next job, which would let one job's signing
    // line satisfy another's check.
    let workflow = "jobs:\n  a:\n    steps:\n      - run: one\n\n  # about b\n  b:\n    steps:\n      - run: two\n";
    let a = job(workflow, "a");
    assert!(a.contains("one") && !a.contains("two"), "{a}");
    let b = job(workflow, "b");
    assert!(b.contains("two") && !b.contains("one"), "{b}");
}

#[test]
fn hostkeepers_embedded_info_plist_names_the_bundle_by_the_signing_identifier() {
    // Catches: a bundle identifier that is not hostkeeper's signing identifier, which the
    // privacy profile grants by, so a program signed under one and named by the other would not
    // be matched.
    let text = repo_file("crates/hostkeeper/macos/Info.plist");
    let plist = plist::Value::from_reader_xml(text.as_bytes()).expect("Info.plist parses");
    let dict = plist.as_dictionary().expect("a dictionary");
    let hostkeeper = MacProgram::named("hostkeeper").expect("hostkeeper is in the table");
    assert_eq!(
        dict.get("CFBundleIdentifier")
            .and_then(plist::Value::as_string),
        Some(hostkeeper.identifier)
    );
    assert_eq!(
        dict.get("CFBundleName").and_then(plist::Value::as_string),
        Some("hostkeeper")
    );
}
