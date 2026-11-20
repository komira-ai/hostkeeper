//! The upload of `hk xcode-push`: `PUT /v1/xcode/<sha256>?version=<version>&build=<build>` on the
//! coordinator's private listener, the archive as the body, unmodified and streamed from the file,
//! and the operator's token as the bearer, in the request's header and nowhere else.
//!
//! The request says `Expect: 100-continue`, so that a refusal the coordinator makes before it
//! reads the body (`not_listed`, `already_held`, `not_kept`, a token it does not take) is read
//! before the archive is sent. The coordinator's answer is `201` and the JSON of the archive
//! stored, which is printed, or `{"error":"<code>"}`, which ends the push with `refused: <code>`
//! on stderr and an exit code of its status: 3 for 401 and 403 (the caller), 2 for the other
//! 4xx (the request), 1 for anything else.

use std::fs::File;
use std::io::{Seek as _, SeekFrom, Write as _};
use std::time::Duration;

use super::{Target, failed, say};
use crate::call::is_reason;
use crate::encode::trim;
use crate::send::one_line;
use crate::{Flow, PROGRAM, Stop, World};

/// The most bytes of an answer that are read: the store's answers are a line of JSON.
const MAX_ANSWER_BYTES: u64 = 64 * 1024;

/// The least seconds the answer is waited for once the body is sent.
pub(crate) const ANSWER_SECS: u64 = 1800;

/// The archive to upload: the file, and the release and SHA-256 it was read to be.
pub(crate) struct Archive<'a> {
    pub(crate) file: File,
    pub(crate) sha256: &'a str,
    pub(crate) version: &'a str,
    pub(crate) build: &'a str,
}

/// The code of the store's error answer, `{"error":"<code>"}`.
pub(crate) fn error_code(body: &[u8]) -> Option<&[u8]> {
    trim(body)
        .strip_prefix(b"{\"error\":\"")?
        .strip_suffix(b"\"}")
}

/// The exit code of a refusal of HTTP `status`: 3 when it refuses the caller (401, 403), 2 when
/// it refuses the request (any other 4xx), 1 for anything else.
pub(crate) fn exit_of_status(status: u16) -> u8 {
    match status {
        401 | 403 => 3,
        400..=499 => 2,
        _ => 1,
    }
}

/// Uploads `archive` to the store of `target`; the answer on stdout, or the refusal.
pub(crate) fn put(w: &mut World<'_>, target: &Target, mut archive: Archive<'_>) -> Flow<()> {
    archive
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|error| failed(&format!("cannot read the archive ({error})")))?;
    let timeout = Duration::from_secs(u64::from(target.timeout_secs));
    let url = format!(
        "{}/v1/xcode/{}?version={}&build={}",
        target.endpoint, archive.sha256, archive.version, archive.build
    );
    say(w, &format!("uploading to {}", target.endpoint));
    // No limit on the whole request, which is as long as the archive takes to send. The body
    // waits up to `--timeout-secs` for the coordinator's `100 Continue`; once it is sent, the
    // answer is waited for `ANSWER_SECS` at least, as long as the coordinator takes to sync a
    // file of up to `--xcode-max-bytes`, audit it and put it in place.
    let answer = Duration::from_secs(ANSWER_SECS.max(u64::from(target.timeout_secs)));
    let sent = target
        .agent
        .put(&url)
        .config()
        .timeout_await_100(Some(timeout))
        .timeout_recv_response(Some(answer))
        .build()
        .header("Authorization", format!("Bearer {}", target.token))
        .header("Content-Type", "application/octet-stream")
        .header("Expect", "100-continue")
        .send(archive.file);
    let mut response = sent.map_err(|error| {
        Stop::fail(format!(
            "no answer from {} ({}); the coordinator keeps nothing of an upload that does not \
             end",
            target.endpoint,
            one_line(&error.to_string())
        ))
    })?;
    let status = response.status().as_u16();
    // An answer that cannot be read is as much of it as was read: the status says what happened.
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_vec()
        .unwrap_or_default();
    if status == 201 {
        let mut text = body;
        text.push(b'\n');
        return w
            .stdout
            .write_all(&text)
            .and_then(|()| w.stdout.flush())
            .map_err(|_| Stop::fail("the archive is stored, and the answer cannot be written"));
    }
    let Some(code) = error_code(&body) else {
        return Err(Stop::fail(format!(
            "the answer (HTTP {status}) is not the Xcode store's"
        )));
    };
    let _ = writeln!(
        w.stderr,
        "{PROGRAM}: the coordinator refused the upload (HTTP {status})"
    );
    if is_reason(code) {
        let _ = writeln!(w.stderr, "refused: {}", String::from_utf8_lossy(code));
    }
    Err(Stop::Exit(exit_of_status(status)))
}
