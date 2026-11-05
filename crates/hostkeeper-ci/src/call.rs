//! One call of `hostkeeper ctl`, in-process, with a token of its own.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use hostkeeper_ctl::{Connect, Io};
use serde_json::Value;

use crate::flags::Settings;
use crate::ports::Ports;

/// The token file, in the work directory.
pub(crate) const TOKEN_FILE: &str = "hostkeeper-oidc-token";

/// What a call came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The coordinator answered, with this JSON.
    Reply(Value),
    /// The coordinator refused, with this token (`[a-z0-9_]{1,64}`).
    Refused(String),
    /// Nothing was sent: no token, or no token file. The text says why.
    NotSent(String),
    /// A call was made and no answer came out of it. The text is ctl's, with the token left out.
    Lost(String),
}

/// Whether `text` is a refusal token: 1 to 64 of `a-z 0-9 _`.
fn is_refusal_token(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// The token of the line `refused: <token>` in ctl's stderr, when there is one.
fn refusal(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find_map(|line| line.strip_prefix("refused: "))
        .filter(|token| is_refusal_token(token))
        .map(str::to_owned)
}

/// Writes `token` to a file at `path` that is made now, readable by its owner only.
fn write_token(path: &Path, token: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())
}

/// Runs `hostkeeper ctl releases <rpc>` with a new token, and removes the token file after.
///
/// The line `::add-mask::<token>` goes out first, so that the runner masks the token in every
/// log from then on.
pub(crate) fn call(ports: &mut dyn Ports, settings: &Settings, rpc: Vec<OsString>) -> Answer {
    let token = match ports.token(&settings.audience) {
        Ok(token) => token,
        Err(e) => return Answer::NotSent(format!("cannot get an OIDC token: {e}")),
    };
    ports.say(&format!("::add-mask::{token}"));
    let path = settings.work_dir.join(TOKEN_FILE);
    // A file left by an earlier run is not ours to reuse: the new one is created fresh.
    drop(std::fs::remove_file(&path));
    let answer = match write_token(&path, &token) {
        Ok(()) => through_ctl(ports.connect(), settings, &path, &token, rpc),
        Err(e) => Answer::NotSent(format!(
            "cannot write the token file {}: {e}",
            path.display()
        )),
    };
    drop(std::fs::remove_file(&path));
    answer
}

/// The call itself, with the token in the file at `token_file`.
fn through_ctl(
    connect: &dyn Connect,
    settings: &Settings,
    token_file: &Path,
    token: &str,
    rpc: Vec<OsString>,
) -> Answer {
    let mut args: Vec<OsString> = vec![
        "hostkeeper".into(),
        "--endpoint".into(),
        settings.endpoint.clone().into(),
        "--ca-file".into(),
        settings.ca_file.clone().into_os_string(),
        "--token-file".into(),
        token_file.as_os_str().to_owned(),
        "--output".into(),
        "json".into(),
        "releases".into(),
    ];
    args.extend(rpc);
    let mut stdin = io::empty();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = hostkeeper_ctl::run(
        args,
        connect,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    match (code, refusal(&stderr)) {
        // ctl prints a reply as canonical JSON; anything else reads as an empty reply, whose
        // missing fields the flow refuses.
        (0, _) => Answer::Reply(serde_json::from_slice(&stdout).unwrap_or(Value::Null)),
        (2 | 3, Some(token)) => Answer::Refused(token),
        _ => Answer::Lost(format!(
            "ctl exited {code}: {}",
            stderr.replace(token, "<token>").trim()
        )),
    }
}
