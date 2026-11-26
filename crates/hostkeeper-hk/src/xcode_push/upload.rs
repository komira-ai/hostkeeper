//! The upload of `hk xcode add`: `PUT /v1/xcode/<sha256>?version=<version>&build=<build>` on the
//! coordinator's private listener, the archive as the body, unmodified and streamed from the file,
//! and the operator's token as the bearer, in the request's header and nowhere else.
//!
//! The request says `Expect: 100-continue`, so that a refusal the coordinator makes before it
//! reads the body (`not_listed`, `already_held`, `not_kept`, a token it does not take) is read
//! before the archive is sent. While the body is sent, the progress is written to stderr
//! ([`crate::xcode_add::progress`]). The coordinator's answer is `201` and the JSON of the archive
//! stored, or `{"error":"<code>"}`, which ends the run with a line in words saying what the code
//! means and what to do ([`words`]), the line `hk: the coordinator refused the upload (HTTP <n>)`,
//! `refused: <code>` on stderr and an exit code of its status: 3 for 401 and 403 (the caller), 2
//! for the other 4xx (the request), 1 for anything else.
//!
//! [`put`] first asks the coordinator what it holds of the archive and sends the rest in one
//! request with a `Content-Range` ([`resume`]), so that an upload cut off continues from the last
//! byte the coordinator holds; a coordinator that cannot be asked gets the whole archive in one
//! request ([`whole`]).

use std::fs::File;
use std::io::{Read, Seek as _, SeekFrom, Write};
use std::time::Duration;

use ureq::SendBody;

use super::{Target, failed, resume, say};
use crate::call::is_reason;
use crate::encode::trim;
use crate::send::one_line;
use crate::xcode_add::progress::Progress;
use crate::{Flow, PROGRAM, Stop, World};

/// The most bytes of an answer that are read: the store's answers are a line of JSON.
pub(super) const MAX_ANSWER_BYTES: u64 = 64 * 1024;

/// The least seconds the answer is waited for once the body is sent.
pub(crate) const ANSWER_SECS: u64 = 1800;

/// The archive to upload: the file, and the release and SHA-256 it was read to be.
pub(crate) struct Archive<'a> {
    pub(crate) file: File,
    pub(crate) size: u64,
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

/// What a refusal `code` of the store means and what to do about it, for the release `version`
/// (`build`); `None` for a code this does not know.
pub(crate) fn words(code: &[u8], version: &str, build: &str) -> Option<String> {
    let text = match code {
        b"not_listed" => format!(
            "the coordinator's list of Xcode releases does not name Xcode {version} ({build}), and \
             the store takes only a release the list names; wait for the daily list to name it, \
             or check what {PROGRAM} xcode list shows as waiting"
        ),
        b"not_kept" => format!(
            "the store keeps only the newest releases, and Xcode {version} is older than they \
             ({PROGRAM} xcode list says how many it keeps); to keep more, raise xcode_keep of a \
             host group"
        ),
        b"already_held" => format!(
            "the store holds an archive of Xcode {version} already, from other bytes; {PROGRAM} \
             xcode list names it, and {PROGRAM} xcode remove --sha256 <its sha256> --reason <why> \
             removes it if these bytes are the ones to keep"
        ),
        b"xcode_version_taken" => format!(
            "the store holds another build of Xcode {version}; {PROGRAM} xcode list names it, and \
             {PROGRAM} xcode remove --sha256 <its sha256> --reason <why> removes it if build \
             {build} is the one to keep"
        ),
        b"upload_in_progress" => format!(
            "another upload of Xcode {version} is in progress; wait for it to end, then run \
             {PROGRAM} xcode add again"
        ),
        b"partial_differs" => format!(
            "the store keeps a partial upload of these bytes with another size, version or build \
             than Xcode {version} ({build}); remove it with {PROGRAM} xcode remove --sha256 \
             <this archive's sha256> --reason <why>, then run {PROGRAM} xcode add again"
        ),
        b"too_large" => format!(
            "the archive is larger than the store takes one upload to be ({PROGRAM} xcode list \
             shows max_bytes); the coordinator's --xcode-max-bytes sets it"
        ),
        b"digest_mismatch" => format!(
            "the bytes sent are not the ones whose SHA-256 was asked to be stored: the archive \
             changed after it was hashed; run {PROGRAM} xcode add again"
        ),
        b"unauthorized" => "the coordinator does not take the token; check the file --token-file \
                           (or token_file of the settings file) names, and that it holds an \
                           operator's token"
            .to_owned(),
        b"forbidden" => "the token is a CI token, which the Xcode store does not take; use an \
                        operator's token"
            .to_owned(),
        _ => return None,
    };
    Some(text)
}

/// The body of the request: the bytes of the archive's file from where it was positioned up to
/// `size`, which tell `progress` how many bytes of the archive are through.
struct Body<'a> {
    file: &'a mut File,
    /// The archive's size when it was hashed: no more than this is read, so that bytes added to
    /// the file since are not sent.
    size: u64,
    /// The archive's bytes before this one's first, and then those read.
    read: u64,
    progress: Progress,
    out: &'a mut dyn Write,
}

impl Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.size.saturating_sub(self.read);
        let end = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = self.file.read(buf.get_mut(..end).unwrap_or_default())?;
        self.read = self
            .read
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        self.progress.at(self.read, self.out);
        Ok(n)
    }
}

/// How one request of the upload ended.
pub(super) enum Sent {
    /// The coordinator answered: its status and as much of its body as was read.
    Answer { status: u16, text: Vec<u8> },
    /// There was no answer: the transport's words for why.
    Cut(String),
}

/// One `PUT` of the archive's bytes from `from` to its end, with its progress on stderr. With
/// `ranged` it carries `Content-Range: bytes <from>-<last>/<size>`, so that the coordinator keeps
/// what it received of a request that is cut off; without it, `from` is 0 and the request is the
/// whole upload of an older coordinator.
pub(super) fn send(
    w: &mut World<'_>,
    target: &Target,
    archive: &mut Archive<'_>,
    from: u64,
    ranged: bool,
) -> Flow<Sent> {
    archive
        .file
        .seek(SeekFrom::Start(from))
        .map_err(|error| failed(&format!("cannot read the archive ({error})")))?;
    let timeout = Duration::from_secs(u64::from(target.timeout_secs));
    let url = format!(
        "{}/v1/xcode/{}?version={}&build={}",
        target.endpoint, archive.sha256, archive.version, archive.build
    );
    if from > 0 {
        say(
            w,
            &format!(
                "continuing at {}",
                crate::xcode_add::progress::of_text(from, archive.size)
            ),
        );
    }
    // No limit on the whole request, which is as long as the archive takes to send. The body
    // waits up to `--timeout-secs` for the coordinator's `100 Continue`; once it is sent, the
    // answer is waited for `ANSWER_SECS` at least, as long as the coordinator takes to sync a
    // file of up to `--xcode-max-bytes`, audit it and put it in place.
    let answer = Duration::from_secs(ANSWER_SECS.max(u64::from(target.timeout_secs)));
    let (terminal, clock) = (w.stderr_terminal, w.mac.clock);
    let mut body = Body {
        file: &mut archive.file,
        size: archive.size,
        read: from,
        progress: Progress::new("uploading", archive.size, terminal, clock).from(from),
        out: &mut *w.stderr,
    };
    let mut request = target
        .agent
        .put(&url)
        .config()
        .timeout_await_100(Some(timeout))
        .timeout_recv_response(Some(answer))
        .build()
        .header("Authorization", format!("Bearer {}", target.token))
        .header("Content-Type", "application/octet-stream")
        .header(
            "Content-Length",
            archive.size.saturating_sub(from).to_string(),
        )
        .header("Expect", "100-continue");
    if ranged {
        request = request.header(
            "Content-Range",
            format!(
                "bytes {from}-{}/{}",
                archive.size.saturating_sub(1),
                archive.size
            ),
        );
    }
    let sent = request.send(SendBody::from_reader(&mut body));
    body.progress.finish(body.out);
    let mut response = match sent {
        Ok(response) => response,
        Err(error) => return Ok(Sent::Cut(one_line(&error.to_string()))),
    };
    let status = response.status().as_u16();
    // An answer that cannot be read is as much of it as was read: the status says what happened.
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_vec()
        .unwrap_or_default();
    Ok(Sent::Answer { status, text })
}

/// The run's end for an answer that is not a success: the refusal said in words, `refused: <code>`
/// and the exit code of its status.
pub(super) fn refused(w: &mut World<'_>, status: u16, text: &[u8], archive: &Archive<'_>) -> Stop {
    let Some(code) = error_code(text) else {
        return Stop::fail(format!(
            "the answer (HTTP {status}) is not the Xcode store's"
        ));
    };
    if let Some(said) = words(code, archive.version, archive.build) {
        let _ = writeln!(w.stderr, "{PROGRAM}: {said}");
    }
    let _ = writeln!(
        w.stderr,
        "{PROGRAM}: the coordinator refused the upload (HTTP {status})"
    );
    if is_reason(code) {
        let _ = writeln!(w.stderr, "refused: {}", String::from_utf8_lossy(code));
    }
    Stop::Exit(exit_of_status(status))
}

/// The whole archive in one request with no `Content-Range`: what a coordinator that cannot resume
/// takes. `Ok` when the store answers `201`, the refusal otherwise.
pub(super) fn whole(w: &mut World<'_>, target: &Target, archive: &mut Archive<'_>) -> Flow<()> {
    match send(w, target, archive, 0, false)? {
        Sent::Cut(why) => Err(Stop::fail(format!(
            "no answer from {} ({why}); the coordinator keeps nothing of an upload that does not \
             end",
            target.endpoint
        ))),
        Sent::Answer { status: 201, .. } => Ok(()),
        Sent::Answer { status, text } => Err(refused(w, status, &text, archive)),
    }
}

/// Uploads `archive` to the store of `target`, with its progress on stderr, resuming where the
/// store holds a part of it ([`resume`]); `Ok` when the store answers `201`, the refusal
/// otherwise.
pub(crate) fn put(w: &mut World<'_>, target: &Target, mut archive: Archive<'_>) -> Flow<()> {
    say(w, &format!("uploading to {}", target.endpoint));
    resume::put(w, target, &mut archive)
}
