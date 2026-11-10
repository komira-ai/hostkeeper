//! The `gh`, `curl`, `cargo` and `rustup` the workflow lint's run tests put on the `PATH` of the
//! committed steps they run with bash (tests/workflows/targets_run_tests.rs and
//! tests/workflows/hello_run_tests.rs). A program of the tests, not shipped; it reaches no
//! network.
//!
//! Started as `gh`, it answers from `releases/` beside its own file, and appends each command
//! line to `calls` there:
//!
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
//! as curl does. It never waits, whatever retry flags it is given. Any other `curl` exits 2.
//!
//! Started as `cargo`, for both tests: `cargo xtask <args>` runs the program `xtask` beside
//! its own file (the test links this crate's real binary there) with `<args>`, and exits as it
//! exits; anything else exits 2. Started as `rustup`: `rustup toolchain install` does nothing and
//! succeeds (the toolchain is the one running the test); anything else exits 2.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
    ExitCode::from(2)
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
