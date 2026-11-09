//! The `aws` and the `curl` the publisher tests (tests/channels_publish_cli.rs) hand `channels
//! apt|rpm|pkg` through `--aws` and `--curl`. A program of the tests, not shipped; it reaches no
//! network. The test links it into a scratch directory, and it keeps its state in directories
//! beside its own file:
//!
//! - `store/<key>` is the bucket: an object is a file at its key;
//! - `origin/<path>` is the public host: what `curl` serves for `https://<host>/<path>`;
//! - `calls` gets each command line, one per line, its arguments separated by a tab.
//!
//! The bucket answers as S3 does, for the calls the verbs make:
//!
//! - `s3api get-object --bucket <b> --key <k> <dest>` copies the object to `<dest>` and prints
//!   `{"ETag": "\"<first 12 hex digits of the object's SHA-256>\""}`; with no such object it
//!   fails with `NoSuchKey`;
//! - `s3api put-object --bucket <b> --key <k> --body <file> [--if-match <etag>]
//!   [--if-none-match *] ...` stores the body, or fails with `PreconditionFailed (412)` when
//!   `--if-none-match` meets an object or `--if-match` meets another ETag or none;
//! - `cloudfront create-invalidation ...` succeeds.
//!
//! Started as `curl` it copies `origin/<path>` to the file after `--output`, and fails as curl
//! does for an HTTP 404 with `--fail` (exit 22) when there is none. Anything else exits 2.

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
    eprintln!("aws: {why}");
    ExitCode::from(1)
}

/// The ETag of an object: quoted, as S3 prints it.
fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", &hex::encode(sha2::Sha256::digest(bytes))[..12])
}

fn get_object(store: &Path, args: &[String]) -> ExitCode {
    let (Some(key), Some(dest)) = (value(args, "--key"), args.last()) else {
        return ExitCode::from(2);
    };
    let Ok(bytes) = std::fs::read(store.join(key)) else {
        return fail("An error occurred (NoSuchKey) when calling the GetObject operation");
    };
    if std::fs::write(dest, &bytes).is_err() {
        return fail("cannot write the destination");
    }
    println!("{}", serde_json::json!({"ETag": etag(&bytes)}));
    ExitCode::SUCCESS
}

fn put_object(store: &Path, args: &[String]) -> ExitCode {
    let (Some(key), Some(body)) = (value(args, "--key"), value(args, "--body")) else {
        return ExitCode::from(2);
    };
    let Ok(bytes) = std::fs::read(body) else {
        return fail("cannot read the body");
    };
    let target = store.join(key);
    let there = std::fs::read(&target).ok();
    let lost = value(args, "--if-none-match").is_some_and(|_| there.is_some())
        || value(args, "--if-match")
            .is_some_and(|want| there.as_deref().map(etag).as_deref() != Some(want));
    if lost {
        return fail(
            "An error occurred (PreconditionFailed) when calling the PutObject operation: (412)",
        );
    }
    let parent = target.parent().unwrap_or(store);
    if std::fs::create_dir_all(parent).is_err() || std::fs::write(&target, bytes).is_err() {
        return fail("cannot store the object");
    }
    println!("{{}}");
    ExitCode::SUCCESS
}

fn curl(origin: &Path, args: &[String]) -> ExitCode {
    let (Some(output), Some(url)) = (value(args, "--output"), args.last()) else {
        return ExitCode::from(2);
    };
    let path = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
        .map_or("", |(_, path)| path);
    match std::fs::copy(origin.join(path), output) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("curl: (22) The requested URL returned error: 404");
            ExitCode::from(22)
        }
    }
}

fn record(calls: &Path, args: &[String]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(calls)
        .and_then(|mut f| writeln!(f, "{}", args.join("\t")))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Ok(exe) = std::env::current_exe() else {
        return fail("no own path");
    };
    let here: PathBuf = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if record(&here.join("calls"), &args).is_err() {
        return fail("cannot record the call");
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["s3api", "get-object", ..] => get_object(&here.join("store"), &args),
        ["s3api", "put-object", ..] => put_object(&here.join("store"), &args),
        ["cloudfront", "create-invalidation", ..] => ExitCode::SUCCESS,
        [first, ..] if first.starts_with("--") => curl(&here.join("origin"), &args),
        _ => ExitCode::from(2),
    }
}
