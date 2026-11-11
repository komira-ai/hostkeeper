//! The `gh`, `curl`, `cargo` and `rustup` the workflow lint's run tests put on the `PATH` of the
//! committed steps they run with bash (tests/workflows/targets_run_tests.rs and
//! tests/workflows/hello_run_tests.rs). A program of the tests, not shipped; it reaches no
//! network.
//!
//! Started as `gh`, it answers from `releases/` beside its own file, and appends each command
//! line to `calls` there:
//!
//! - `gh attestation verify <file> --bundle <b> --custom-trusted-root <root> --repo <r>
//!   --signer-workflow <w> --source-ref <ref> --source-digest <c> ... --format json` is the
//!   offline check of the coordinator-update unit's test (tests/coordinator_update_unit.rs). The
//!   bundle is the test's stand-in for a signed one, `{"signer": "<workflow>@<ref>", "commit":
//!   <c>, "sha256": <hex>}`, and the fixture judges it as `gh` judges a real one: the signer's
//!   workflow must be `<w>` in the repository `<r>`, its ref `<ref>`, its commit `<c>` and its
//!   digest `<file>`'s SHA-256, and `<root>` must be a file. Then it prints the attestation the
//!   bundle names, as `gh --format json` does; else it exits 1 with `gh`'s complaint. It writes
//!   the environment it was started with, one `NAME=value` a line in name order, to `env`;
//! - `gh release list --repo <o>/<n> ... --json tagName --jq <filter>` runs `jq --raw-output
//!   <filter>` over `releases/<o>/<n>/release-list.json` (`[{"tagName": ...}, ...]`, `[]` when
//!   there is none) and prints what it prints, as `gh` does with `--jq`.
//!
//! Anything else exits 2.
//!
//! Started as `curl` (a link named `curl`), it is the fake origin of hello-desired.yml's download:
//! `curl --remote-name-all --output-dir <dir> <url>...` takes each `https://` argument as a URL that may hold curl's globs, `{a,b,...}` sets
//! one after another and not nested, and copies each URL `https://<rest>` a glob expands to from
//! `channel/<rest>` beside its own file to `<dir>/<its last path segment>`, in order. A missing
//! one is a failed transfer, as curl's for an HTTP 404 with `--fail`: with `--fail-early` it ends
//! the run at once with exit 22; without it the run goes on, and exits as its last transfer did,
//! as curl does. It never waits, whatever retry flags it is given.
//!
//! A `curl` with both `--output <file>` and `--write-out <format>` is the coordinator's version
//! route as release.yml's job `coordinator` asks it (tests/workflows/coordinator_wait_run_tests.rs).
//! Each try appends `<METHOD> <path>` to `answers/calls` as below and reads the answer file of
//! that try, `answers/<key>-<n>` for the `n`th (from 1), else `answers/<key>`. A status of `000`
//! there, or no answer file at all, is no answer: curl's exit 7, when it cannot connect. Else the
//! try writes the body to `<file>`, except for a status of 400 or more with `--fail`, and fails
//! with curl's exit 22 for a status of 400 or more when given `--fail` or `--fail-with-body`.
//! A failed try is tried again, up to `--retry <n>` more times, when `--retry-all-errors` is
//! given (or the try had no answer, or a status curl calls transient: 408, 429, 500, 502, 503,
//! 504), as curl does; but a try after one that wrote a body into a `<file>` that is not a
//! regular file (`/dev/null`) ends the run with exit 23 and prints nothing more, as curl 8.5 (the
//! curl of ubuntu-24.04) does when it cannot truncate that file for the retry. After the last
//! try it prints `<format>` once, with `%{http_code}` made that try's status and `\n` a line
//! break, and exits as that try ended. It never waits, whatever `--retry-delay` or
//! `--retry-max-time` it is given.
//!
//! Any other `curl` with `--write-out` is the Docker Engine API and the coordinator's version route of
//! the coordinator-update unit's test (tests/coordinator_update_unit.rs): it takes the request's
//! method (`-X`, else `GET`) and the path and query of its URL, appends `<METHOD> <path>` to
//! `answers/calls` beside its own file, and prints the answer file `answers/<key>`, where the key
//! is `<METHOD> <path>` with every character but a letter or a digit made `_`: its first line is
//! the HTTP status, the rest the body, then a line break and the status, as `--write-out
//! '\n%{http_code}'` prints them. With no such file the answer is a 404. Any other `curl` exits
//! 2.
//!
//! Started as `cargo`, for both tests: `cargo xtask <args>` runs the program `xtask` beside
//! its own file (the test links this crate's real binary there) with `<args>`, and exits as it
//! exits; anything else exits 2. Started as `rustup`: `rustup toolchain install` does nothing and
//! succeeds (the toolchain is the one running the test); anything else exits 2.

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
        ["attestation", "verify", file, ..] => verify(data, file, args),
        ["release", "list", ..] => {
            let (Some(repo), Some(filter)) = (value(args, "--repo"), value(args, "--jq")) else {
                return ExitCode::from(2);
            };
            let list = std::fs::read(data.join(repo).join("release-list.json"));
            let list = list.unwrap_or_else(|_| b"[]".to_vec());
            let child = std::process::Command::new("jq")
                .args(["--raw-output", filter])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::inherit())
                .spawn();
            let Ok(mut child) = child else {
                return fail("cannot run jq");
            };
            let written = child
                .stdin
                .take()
                .is_some_and(|mut stdin| stdin.write_all(&list).is_ok());
            match child.wait() {
                Ok(status) if status.success() && written => ExitCode::SUCCESS,
                _ => fail("jq failed"),
            }
        }
        _ => ExitCode::from(2),
    }
}

/// The fake offline `gh attestation verify` (see the module).
fn verify(data: &Path, file: &str, args: &[String]) -> ExitCode {
    let flags = [
        "--bundle",
        "--custom-trusted-root",
        "--repo",
        "--signer-workflow",
        "--source-ref",
        "--source-digest",
    ]
    .map(|flag| value(args, flag));
    let [
        Some(bundle),
        Some(root),
        Some(repo),
        Some(workflow),
        Some(source_ref),
        Some(commit),
    ] = flags
    else {
        return ExitCode::from(2);
    };
    if value(args, "--format") != Some("json") {
        return ExitCode::from(2);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the fixture records the environment it was started with, for its test to read"
    )]
    let mut env: Vec<String> = std::env::vars()
        // An instrumented build's profile runtime sets this in the fixture's own process; no
        // caller gave it.
        .filter(|(k, _)| !k.starts_with("__LLVM_PROFILE_"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    env.sort();
    if std::fs::write(data.join("env"), env.join("\n") + "\n").is_err() {
        return fail("cannot write env");
    }
    let (Ok(bytes), Ok(claims)) = (std::fs::read(file), std::fs::read(bundle)) else {
        return fail("no such file");
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&claims) else {
        return fail("the bundle is not JSON");
    };
    let text = |key: &str| claims[key].as_str().unwrap_or_default().to_owned();
    let (signer, signed_commit, signed_digest) = (text("signer"), text("commit"), text("sha256"));
    let (signed_workflow, signed_ref) = signer.split_once('@').unwrap_or((&signer, ""));
    let digest = hex::encode(sha2::Sha256::digest(&bytes));
    let holds = Path::new(root).is_file()
        && signed_workflow == workflow
        && signed_workflow.starts_with(&format!("{repo}/"))
        && signed_ref == source_ref
        && signed_commit == commit
        && signed_digest == digest;
    if !holds {
        eprintln!("\nError: verifying with issuer \"sigstore.dev\"");
        return ExitCode::from(1);
    }
    let reply = serde_json::json!([{
        "verificationResult": {
            "signature": {"certificate": {
                "buildSignerURI": format!("https://github.com/{signer}"),
                "sourceRepositoryRef": signed_ref,
                "sourceRepositoryDigest": signed_commit,
            }},
            "statement": {"subject": [{"digest": {"sha256": signed_digest}}]},
        }
    }]);
    println!("{reply}");
    ExitCode::SUCCESS
}

/// Every URL the curl glob `url` names, in curl's order: the first set varies slowest.
fn expand(url: &str) -> Vec<String> {
    let Some((head, rest)) = url.split_once('{') else {
        return vec![url.to_owned()];
    };
    let Some((set, tail)) = rest.split_once('}') else {
        return vec![url.to_owned()];
    };
    set.split(',')
        .flat_map(|choice| expand(&format!("{head}{choice}{tail}")))
        .collect()
}

/// The fake `curl --remote-name-all --output-dir <dir> <url>...`: see the module's documentation.
fn curl_remote_names(dir: &Path, args: &[String]) -> ExitCode {
    let Some(into) = value(args, "--output-dir") else {
        return ExitCode::from(2);
    };
    if std::fs::create_dir_all(into).is_err() {
        return fail("cannot make the output directory");
    }
    let early = args.iter().any(|a| a == "--fail-early");
    let mut last = ExitCode::SUCCESS;
    for url in args.iter().filter(|a| a.starts_with("https://")) {
        for one in expand(url) {
            let rest = one.trim_start_matches("https://");
            let name = rest.rsplit('/').next().unwrap_or_default();
            last = ExitCode::SUCCESS;
            if std::fs::copy(dir.join("channel").join(rest), Path::new(into).join(name)).is_err() {
                eprintln!("curl: (22) The requested URL returned error: 404");
                if early {
                    return ExitCode::from(22);
                }
                last = ExitCode::from(22);
            }
        }
    }
    last
}

/// The fake `curl`: see the module's documentation.
fn curl(dir: &Path, args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--remote-name-all") {
        return curl_remote_names(dir, args);
    }
    if let (Some(file), Some(format)) = (value(args, "--output"), value(args, "--write-out")) {
        return curl_status(dir, args, file, format);
    }
    if args.iter().any(|a| a == "--write-out") {
        return curl_answer(dir, args);
    }
    ExitCode::from(2)
}

/// The key of a request's answer file: `<METHOD> <path>` with every character but a letter or a
/// digit made `_`.
fn answer_key(method: &str, path: &str) -> String {
    format!("{method} {path}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The request's method and the path (with its query) of its URL, appended to `answers/calls`;
/// none when it names no URL or the log cannot be written.
fn logged_request<'a>(dir: &Path, args: &'a [String]) -> Option<(&'a str, &'a str)> {
    let method = value(args, "-X").unwrap_or("GET");
    let url = args
        .iter()
        .find(|a| a.starts_with("http://") || a.starts_with("https://"))?;
    let after_scheme = url.split_once("://").map_or("", |(_, rest)| rest);
    let path = after_scheme.find('/').map_or("/", |at| &after_scheme[at..]);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("answers/calls"))
        .and_then(|mut log| writeln!(log, "{method} {path}"))
        .ok()?;
    Some((method, path))
}

/// The fake answers of an `--output <file> --write-out <format>` request and its retries (see
/// the module).
fn curl_status(dir: &Path, args: &[String], file: &str, format: &str) -> ExitCode {
    let retries: u32 = value(args, "--retry")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let flag = |name: &str| args.iter().any(|a| a == name);
    let regular = std::fs::metadata(file).map_or(true, |m| m.is_file());
    let mut written = false;
    let mut tried = 0;
    loop {
        tried += 1;
        let Some((method, path)) = logged_request(dir, args) else {
            return ExitCode::from(2);
        };
        let key = answer_key(method, path);
        let answers = dir.join("answers");
        let text = std::fs::read_to_string(answers.join(format!("{key}-{tried}")))
            .or_else(|_| std::fs::read_to_string(answers.join(&key)))
            .unwrap_or_else(|_| "000".to_owned());
        let (status, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
        let code: u16 = status.parse().unwrap_or(0);
        let exit = if code == 0 {
            eprintln!("curl: (7) Failed to connect");
            7
        } else {
            if !(code >= 400 && flag("--fail")) {
                if std::fs::write(file, body).is_err() {
                    return fail("cannot write --output");
                }
                written |= !body.is_empty();
            }
            if code >= 400 && (flag("--fail") || flag("--fail-with-body")) {
                eprintln!("curl: (22) The requested URL returned error: {status}");
                22
            } else {
                0
            }
        };
        let transient = code == 0 || [408, 429, 500, 502, 503, 504].contains(&code);
        if exit != 0 && tried <= retries && (flag("--retry-all-errors") || transient) {
            if written && !regular {
                eprintln!("curl: (23) Failed writing received data to disk/application");
                return ExitCode::from(23);
            }
            continue;
        }
        print!(
            "{}",
            format
                .replace("%{http_code}", if code == 0 { "000" } else { status })
                .replace("\\n", "\n")
        );
        return ExitCode::from(exit);
    }
}

/// The fake answer of one `--write-out` request (see the module).
fn curl_answer(dir: &Path, args: &[String]) -> ExitCode {
    let Some((method, path)) = logged_request(dir, args) else {
        return ExitCode::from(2);
    };
    let text = std::fs::read_to_string(dir.join("answers").join(answer_key(method, path)))
        .unwrap_or_else(|_| "404\n{\"message\":\"not found\"}".to_owned());
    let (status, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
    print!("{body}\n{status}");
    ExitCode::SUCCESS
}

/// The fake `cargo`: see the module's documentation.
fn cargo(dir: &Path, args: &[String]) -> ExitCode {
    let Some(("xtask", rest)) = args
        .split_first()
        .map(|(first, rest)| (first.as_str(), rest))
    else {
        return ExitCode::from(2);
    };
    match std::process::Command::new(dir.join("xtask"))
        .args(rest)
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(
            status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
        ),
        Err(_) => fail("cannot run xtask"),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Ok(exe) = std::env::current_exe() else {
        return fail("no file of its own");
    };
    let Some(dir) = exe.parent() else {
        return fail("no directory of its own");
    };
    match exe.file_name().and_then(|name| name.to_str()) {
        Some("curl") => return curl(dir, &args),
        Some("cargo") => return cargo(dir, &args),
        Some("rustup") if args == ["toolchain", "install"] => return ExitCode::SUCCESS,
        Some("rustup") => return ExitCode::from(2),
        _ => {}
    }
    let data: PathBuf = dir.join("releases");
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
