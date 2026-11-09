use super::*;

// ---------------------------------------------------------------------------------------------
// the programs
// ---------------------------------------------------------------------------------------------

#[test]
fn a_verb_given_an_aws_or_curl_that_is_not_there_is_status_1_naming_the_program() {
    let d = dir("missing-program");
    release(&d, "rpms", "1.2.3", "rpm");
    let release = pkg_release(&d, "release", "1.2.3");
    let missing = d.join("no-such-program");
    let program = text(&missing);
    let (out_dir, work_dir) = (d.join("out"), d.join("work"));
    let (out, work) = (text(&out_dir), text(&work_dir));
    let current = empty_current(&d, "current");
    let rpm_next = d.join("rpm-next");
    let index = |args: &[&str]| ok(&d, args);
    index(&[
        "channels",
        "rpm",
        "repo",
        "index",
        "--current",
        text(&current),
        "--out",
        text(&rpm_next),
        "--rpms",
        text(&d.join("rpms")),
        "--version",
        "1.2.3",
        "--date",
        DATE,
    ]);
    sign_rpm(&rpm_next, "stable");
    let sums = release.join("SHA256SUMS");
    let commands: Vec<Vec<&str>> = vec![
        vec![
            "channels", "apt", "read", "--bucket", BUCKET, "--out", out, "--aws", program,
        ],
        vec![
            "channels", "rpm", "repo", "read", "--bucket", BUCKET, "--out", out, "--aws", program,
        ],
        vec![
            "channels",
            "rpm",
            "repo",
            "write",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1",
            "--hostname",
            HOST,
            "--next",
            text(&rpm_next),
            "--no-pool",
            "--aws",
            program,
        ],
        vec![
            "channels",
            "pkg",
            "upload",
            "--dir",
            text(&release),
            "--tag",
            "v1.2.3",
            "--commit",
            COMMIT,
            "--bucket",
            BUCKET,
            "--work",
            work,
            "--aws",
            program,
        ],
        vec![
            "channels",
            "pkg",
            "check",
            "--sums",
            text(&sums),
            "--version",
            "1.2.3",
            "--url",
            "https://packages.example.com",
            "--work",
            work,
            "--curl",
            program,
        ],
    ];
    for args in commands {
        let why = exits(&d, 1, &args);
        assert!(why.contains(program), "{args:?}: {why}");
    }
}
