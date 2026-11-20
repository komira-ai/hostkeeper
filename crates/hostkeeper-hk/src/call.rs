//! The one call of a method: the checks of the global flags, the credential, the POST to the
//! coordinator's gateway, and what the answer is: printed, or an exit code and a reason.

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use hostkeeper_proto::ctl::Command;
use ureq::tls::Certificate;

use crate::args::{Given, Globals};
use crate::body::{Body, build, read_file, read_path, read_stdin};
use crate::encode::{seconds, trim, visible};
use crate::send::{Limits, Method, certificates, send};
use crate::table::{exit_of, is_hold};
use crate::{Flow, Stop, World};

/// Whether `text` is `https://<host>[:<port>]` with an optional `/` at the end: a name or an
/// address of letters, digits, `.` and `-`, or an IPv6 address in brackets, and no path.
pub(crate) fn is_endpoint(text: &[u8]) -> bool {
    let Some(rest) = text.strip_prefix(b"https://") else {
        return false;
    };
    let rest = rest.strip_suffix(b"/").unwrap_or(rest);
    let host_len = if rest.first() == Some(&b'[') {
        let inside = rest
            .iter()
            .position(|b| *b == b']')
            .map(|close| (close, rest.get(1..close).unwrap_or_default()));
        match inside {
            Some((close, inside))
                if !inside.is_empty()
                    && inside
                        .iter()
                        .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.')) =>
            {
                close + 1
            }
            _ => return false,
        }
    } else {
        let n = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
            .count();
        if n == 0 {
            return false;
        }
        n
    };
    match rest.get(host_len..).unwrap_or_default() {
        b"" => true,
        tail => tail.strip_prefix(b":").is_some_and(|port| {
            (1..=5).contains(&port.len()) && port.iter().all(u8::is_ascii_digit)
        }),
    }
}

/// The seconds a call may take in all: `timeout`, and, when the request's hold is a whole number
/// of seconds (1 to 999999, with one pair of quotes at most around it), the hold too.
pub(crate) fn total_secs(timeout: u32, hold_text: &[u8]) -> u64 {
    let text = hold_text.strip_prefix(b"\"").unwrap_or(hold_text);
    let text = text.strip_suffix(b"\"").unwrap_or(text);
    u64::from(timeout) + seconds(text).map_or(0, u64::from)
}

/// The text up to the first `needle`'s end is cut off; what is left after it.
fn after<'a>(text: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    let at = text.windows(needle.len()).position(|w| w == needle)?;
    text.get(at + needle.len()..)
}

/// `{"code":"<CODE>","reason":"<reason>"}`, the gateway's error body: the code and the reason as
/// written.
pub(crate) fn refusal(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = body.strip_prefix(b"{\"code\":\"")?;
    let n = rest
        .iter()
        .take_while(|b| b.is_ascii_uppercase() || **b == b'_')
        .count();
    let (code, rest) = rest.split_at(n);
    if code.is_empty() {
        return None;
    }
    let reason = rest
        .strip_prefix(b"\",\"reason\":\"")?
        .strip_suffix(b"\"}")?;
    Some((code, reason))
}

/// Whether `reason` is a token of `[a-z0-9_]{1,64}`.
pub(crate) fn is_reason(reason: &[u8]) -> bool {
    (1..=64).contains(&reason.len())
        && reason
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// The trust anchors of the certificates in `der`.
fn from_der(der: &[Vec<u8>]) -> Vec<Certificate<'static>> {
    der.iter()
        .map(|der| Certificate::from_der(der).to_owned())
        .collect()
}

/// Refuses a token file that group or others may read. A relative path is looked at as
/// `./<path>`, so that an empty one is the current directory.
fn private(path: &[u8]) -> Flow<()> {
    let at = if path.starts_with(b"/") {
        path.to_vec()
    } else {
        [b"./".as_slice(), path].concat()
    };
    let Ok(meta) = std::fs::metadata(OsStr::from_bytes(&at)) else {
        return Err(Stop::usage(
            [b"cannot read the mode of --token-file ".as_slice(), path].concat(),
        ));
    };
    if meta.permissions().mode() & 0o044 != 0 {
        return Err(Stop::usage(
            [
                b"--token-file ".as_slice(),
                path,
                b" may be read by its group or others; make it 0600",
            ]
            .concat(),
        ));
    }
    Ok(())
}

/// The percent-encoding of `text`, byte by byte: letters, digits, `.`, `_`, `~` and `-` as they
/// are.
pub(crate) fn percent(text: &[u8]) -> String {
    let mut out = String::new();
    for &byte in text {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A fresh ID token of the GitHub Actions runner, for the audience `--github-oidc-audience`
/// names; `::add-mask::<token>` is printed on stdout before the token is used.
fn oidc(w: &mut World<'_>, audience: &[u8], timeout: u32) -> Flow<String> {
    let url = w.oidc_url.clone().unwrap_or_default();
    let request = w.oidc_token.clone().unwrap_or_default();
    if url.is_empty() || request.is_empty() {
        return Err(Stop::fail(
            "--github-oidc-audience needs the job to have id-token: write: the runner set no \
             ACTIONS_ID_TOKEN_REQUEST_URL and ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ));
    }
    if !url.starts_with(b"https://") {
        return Err(Stop::fail(
            "ACTIONS_ID_TOKEN_REQUEST_URL is not an https URL",
        ));
    }
    if !visible(&url) || !visible(&request) {
        return Err(Stop::fail(
            "the runner set ACTIONS_ID_TOKEN_REQUEST_URL or ACTIONS_ID_TOKEN_REQUEST_TOKEN with \
             a character that is not visible ASCII",
        ));
    }
    let separator = if url.contains(&b'?') { '&' } else { '?' };
    let full = format!(
        "{}{separator}audience={}",
        String::from_utf8_lossy(&url),
        percent(audience)
    );
    let limits = Limits {
        connect: Duration::from_secs(u64::from(timeout)),
        total: Duration::from_secs(u64::from(timeout)),
    };
    let roots = from_der(&(w.web_roots)());
    let answer = send(
        &full,
        &Method::Get(Some(&String::from_utf8_lossy(&request))),
        &roots,
        limits,
    )
    .map_err(|error| Stop::fail(format!("the runner's token request failed ({error})")))?;
    if answer.status != 200 {
        return Err(Stop::fail(format!(
            "the runner's token request was answered HTTP {}",
            answer.status
        )));
    }
    let Some(rest) = after(&answer.body, b"\"value\"") else {
        return Err(Stop::fail("the runner's token reply has no value"));
    };
    let rest = trim(rest);
    let rest = trim(rest.strip_prefix(b":").unwrap_or(rest));
    let rest = rest.strip_prefix(b"\"").unwrap_or(rest);
    let end = rest.iter().position(|b| *b == b'"');
    let token = rest.get(..end.unwrap_or(rest.len())).unwrap_or_default();
    if end.is_none() || !visible(token) {
        return Err(Stop::fail(
            "the runner's token reply has no value that is a token",
        ));
    }
    let token = String::from_utf8_lossy(token).into_owned();
    // A write that fails (a closed stdout) is no reason to stop: the token is masked or it is not.
    let _ = writeln!(w.stdout, "::add-mask::{token}");
    let _ = w.stdout.flush();
    Ok(token)
}

/// The bearer token, from exactly one of `--token-file`, `--token-stdin` and
/// `--github-oidc-audience`.
fn credential(w: &mut World<'_>, globals: &Globals) -> Flow<String> {
    let given = usize::from(globals.token_file.is_some())
        + usize::from(globals.token_stdin)
        + usize::from(globals.audience.is_some());
    if given != 1 {
        return Err(Stop::usage(
            "a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience",
        ));
    }
    if let Some(audience) = &globals.audience {
        return oidc(w, audience, globals.timeout_secs());
    }
    let token = if let Some(path) = &globals.token_file {
        private(path)?;
        read_file(path, b"the token")?
    } else {
        read_stdin(w.stdin, b"the token")?
    };
    if !visible(&token) {
        return Err(Stop::usage(
            "the token is empty or holds a character that is not visible ASCII",
        ));
    }
    Ok(String::from_utf8_lossy(&token).into_owned())
}

/// After a call refused `invalid_request`: whether the coordinator is older than this `hk`, said
/// on stderr when it is. One more call, `GET /v1/version?at_least=<this release>` with no token:
/// `409` says it is older and which version it runs, `404` is a coordinator before that route.
/// Anything else (the coordinator is not older, no answer, an answer not read) leaves the
/// refusal as it was.
fn older(w: &mut World<'_>, endpoint: &str, roots: &[Certificate<'static>], timeout: u32) -> bool {
    let want = w.stamp.split('+').next().unwrap_or_default();
    if want.is_empty() {
        return false;
    }
    let limits = Limits {
        connect: Duration::from_secs(u64::from(timeout)),
        total: Duration::from_secs(u64::from(timeout)),
    };
    let url = format!("{endpoint}/v1/version?at_least={want}");
    let Ok(answer) = send(&url, &Method::Get(None), roots, limits) else {
        return false;
    };
    let now = after(&answer.body, b"\"version\":\"")
        .and_then(|rest| {
            rest.iter()
                .position(|b| *b == b'"')
                .and_then(|end| rest.get(..end))
        })
        .filter(|version| visible(version))
        .map(|version| String::from_utf8_lossy(version).into_owned());
    let name = w.name;
    match (answer.status, now) {
        (409, Some(now)) => {
            let _ = writeln!(
                w.stderr,
                "{name}: the coordinator is older than this {name} ({want}): it runs {now}; \
                 update the coordinator, then call again"
            );
            true
        }
        (404, _) => {
            let _ = writeln!(
                w.stderr,
                "{name}: the coordinator is older than this {name} ({want}): it does not say its \
                 version; update the coordinator, then call again"
            );
            true
        }
        (200, Some(now)) => {
            let _ = writeln!(
                w.stderr,
                "{name}: the coordinator runs {now}, not older than this {name} ({want})"
            );
            false
        }
        _ => false,
    }
}

/// Prints the gateway's answer to stdout, or writes it to `--response-file`.
fn deliver(w: &mut World<'_>, body: &[u8], response_file: Option<&[u8]>) -> Flow<()> {
    let mut text = body.to_vec();
    text.push(b'\n');
    if let Some(path) = response_file {
        let written = std::fs::File::create(OsStr::from_bytes(path))
            .and_then(|mut file| file.write_all(&text));
        return written
            .map_err(|_| Stop::fail([b"cannot write --response-file ".as_slice(), path].concat()));
    }
    w.stdout
        .write_all(&text)
        .and_then(|()| w.stdout.flush())
        .map_err(|_| Stop::fail("cannot write the answer"))
}

/// The one call of `command`: checks the flags, builds the body, gets the credential, posts, and
/// answers. `request` is the `--request` value.
pub(crate) fn call(
    w: &mut World<'_>,
    globals: &Globals,
    command: &Command,
    given: &Given<'_>,
    request: Option<&[u8]>,
) -> Flow<()> {
    let Some(endpoint) = &globals.endpoint else {
        return Err(Stop::usage("a call needs --endpoint"));
    };
    if !is_endpoint(endpoint) {
        return Err(Stop::usage("--endpoint takes https://<host>[:<port>]"));
    }
    let endpoint =
        String::from_utf8_lossy(endpoint.strip_suffix(b"/").unwrap_or(endpoint)).into_owned();
    let Some(ca_file) = &globals.ca_file else {
        return Err(Stop::usage("a call needs --ca-file"));
    };
    let Some(ca) = read_path(ca_file) else {
        return Err(Stop::usage(
            [b"cannot read --ca-file ".as_slice(), ca_file].concat(),
        ));
    };
    if globals.response_file.as_ref().is_some_and(Vec::is_empty) {
        return Err(Stop::usage("--response-file needs a path"));
    }
    let hold = command.fields.iter().position(is_hold);
    let Body { bytes, hold_text } = build(given, request, hold, w.stdin)?;
    let token = credential(w, globals)?;
    let timeout = globals.timeout_secs();
    let limits = Limits {
        connect: Duration::from_secs(u64::from(timeout)),
        total: Duration::from_secs(total_secs(timeout, &hold_text)),
    };
    let roots = certificates(&ca);
    let url = format!("{endpoint}{}", command.path);
    let answer = send(
        &url,
        &Method::Post {
            body: &bytes,
            bearer: &token,
        },
        &roots,
        limits,
    )
    .map_err(|error| Stop::fail(format!("no answer from {endpoint} ({error})")))?;
    if answer.status == 200 {
        return deliver(w, &answer.body, globals.response_file.as_deref());
    }
    let not_gateway = || {
        Stop::fail(format!(
            "the answer (HTTP {}) is not the gateway's",
            answer.status
        ))
    };
    let (code, reason) = refusal(&answer.body).ok_or_else(not_gateway)?;
    let status = exit_of(code).ok_or_else(not_gateway)?;
    let name = w.name;
    let _ = writeln!(
        w.stderr,
        "{name}: the coordinator answered {}",
        String::from_utf8_lossy(code)
    );
    let mut reason = reason.to_vec();
    if reason == b"invalid_request" && older(w, &endpoint, &roots, timeout) {
        reason = b"coordinator_older".to_vec();
    }
    if is_reason(&reason) {
        let _ = writeln!(w.stderr, "refused: {}", String::from_utf8_lossy(&reason));
    }
    Err(Stop::Exit(status))
}
