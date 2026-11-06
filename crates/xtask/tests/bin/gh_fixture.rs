//! The `gh` the channel-sync deploy test (tests/channel_sync.rs) puts on the `PATH` of the
//! committed unit's and sidecar's commands. A program of the tests, not shipped; it reaches no
//! network.
//!
//! It answers from `releases/` beside its own file, and appends each command line to `calls`
//! there:
//!
//! - `gh api repos/<o>/<n>/releases?per_page=100` prints `releases/<o>/<n>/list.json`, or `[]`
//!   when there is none;
//! - `gh release download <tag> --repo <o>/<n> --pattern <name> --dir <dir>` copies
//!   `releases/<o>/<n>/<tag>/<name>` into `<dir>`, and fails like GitHub (HTTP 404) when there is
//!   no such file;
//! - `gh attestation verify <file> [--bundle <b>] --repo <r> --signer-workflow <w> --source-ref
//!   <ref> --source-digest <c> ...` prints one attestation of `<file>`'s SHA-256 by `<w>` on
//!   `<ref>` at `<c>`: every file verifies as signed by whom it is asked about.
//!
//! Anything else exits 2.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sha2::Digest;

/// The value after `flag` in `args`.
fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(String::as_str)
}

fn fail(why: &str) -> ExitCode {
    eprintln!("gh: {why}");
    ExitCode::from(1)
}

fn answer(data: &Path, args: &[String]) -> ExitCode {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["api", path] => {
            let Some(repo) = path
                .strip_prefix("repos/")
                .and_then(|p| p.strip_suffix("/releases?per_page=100"))
            else {
                return ExitCode::from(2);
            };
            let list = std::fs::read(data.join(repo).join("list.json"));
            let list = list.unwrap_or_else(|_| b"[]".to_vec());
            if std::io::stdout().write_all(&list).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        ["release", "download", tag, ..] => {
            let (Some(repo), Some(name), Some(dir)) = (
                value(args, "--repo"),
                value(args, "--pattern"),
                value(args, "--dir"),
            ) else {
                return ExitCode::from(2);
            };
            let from = data.join(repo).join(tag).join(name);
            match std::fs::copy(&from, Path::new(dir).join(name)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(_) => fail("HTTP 404: Not Found"),
            }
        }
        ["attestation", "verify", file, ..] => {
            let (Some(workflow), Some(source_ref), Some(commit)) = (
                value(args, "--signer-workflow"),
                value(args, "--source-ref"),
                value(args, "--source-digest"),
            ) else {
                return ExitCode::from(2);
            };
            let Ok(bytes) = std::fs::read(file) else {
                return fail("no such file");
            };
            let reply = serde_json::json!([{
                "verificationResult": {
                    "signature": {"certificate": {
                        "buildSignerURI": format!("https://github.com/{workflow}@{source_ref}"),
                        "sourceRepositoryRef": source_ref,
                        "sourceRepositoryDigest": commit,
                    }},
                    "statement": {"subject": [{"digest": {
                        "sha256": hex::encode(sha2::Sha256::digest(&bytes)),
                    }}]},
                }
            }]);
            println!("{reply}");
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(2),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(data) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("releases")))
    else {
        return fail("no directory of its own");
    };
    let data: PathBuf = data;
    let logged = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("calls"))
        .and_then(|mut log| writeln!(log, "{}", args.join(" ")));
    if logged.is_err() {
        return fail("cannot write calls");
    }
    answer(&data, &args)
}
