//! NanoHUB's command API, as `send` reaches it: `PUT /api/v1/nanomdm/enqueue/<enrollment id>`
//! with the command plist as the body (`Content-Type: application/xml`) and HTTP Basic
//! authentication as the user `nanohub` with the API key, on a loopback address only. NanoMDM's
//! enqueue route pushes the Mac itself. Its answer is JSON: 200 when the command is queued and
//! pushed; 500 with a `push_error` when it is queued but the push failed, which the Mac makes up
//! at its next check-in; a `command_error`, at the top or for the id, when it is not queued.
//! These are the calls of the gate's NanoHUB client (task M8) against NanoHUB 0.2.0.
//!
//! The client ([`NanoHubHttp`]) speaks HTTP/1.1 over one `TcpStream` per call, with
//! `Connection: close`, and reads the answer to its end, at most [`ANSWER_MAX`] bytes.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::Error;

/// The most of an answer the client reads.
const ANSWER_MAX: u64 = 64 * 1024;

/// How long one read or write of a call may wait before the call fails.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// NanoHUB's API: `http://<loopback IP address>:<port>`, with an optional `/` and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApiUrl(SocketAddr);

impl FromStr for ApiUrl {
    type Err = String;

    fn from_str(s: &str) -> Result<ApiUrl, String> {
        let refuse = |why: &str| format!("{s:?} is not NanoHUB's loopback API URL: {why}");
        let rest = s
            .strip_prefix("http://")
            .ok_or_else(|| refuse("it must start with http://"))?;
        let authority = rest.strip_suffix('/').unwrap_or(rest);
        let addr: SocketAddr = authority
            .parse()
            .map_err(|_| refuse("it must be http://<IP address>:<port> with no path"))?;
        if !addr.ip().is_loopback() || addr.port() == 0 {
            return Err(refuse(
                "the address must be loopback and the port not 0: the API is never served \
                 beyond the host",
            ));
        }
        Ok(ApiUrl(addr))
    }
}

impl ApiUrl {
    pub(crate) fn addr(&self) -> SocketAddr {
        self.0
    }
}

/// A Mac's enrollment id in NanoHUB: 1 to 64 ASCII letters, digits and `-` (its UDID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnrollmentId(String);

impl FromStr for EnrollmentId {
    type Err = String;

    fn from_str(s: &str) -> Result<EnrollmentId, String> {
        let good = (1..=64).contains(&s.len())
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if good {
            Ok(EnrollmentId(s.to_owned()))
        } else {
            Err(format!(
                "{s:?} is not an enrollment id (1 to 64 letters, digits and -)"
            ))
        }
    }
}

impl EnrollmentId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The API key in the file `path`: its one line, without the line end. The file must be a
/// regular file no one but its owner may read or write.
pub(crate) fn read_key(path: &Path) -> Result<String, Error> {
    let what = format!("read the API key file {}", path.display());
    let meta = std::fs::symlink_metadata(path).map_err(Error::io(&what))?;
    if !meta.file_type().is_file() {
        return Err(Error::Refused(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::Refused(format!(
            "{} is open to its group or others (mode {:o}): an API key file is 0600",
            path.display(),
            meta.permissions().mode() & 0o777
        )));
    }
    let text = std::fs::read_to_string(path).map_err(Error::io(&what))?;
    let key = text.strip_suffix('\n').unwrap_or(&text);
    let key = key.strip_suffix('\r').unwrap_or(key);
    if key.is_empty()
        || key
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(Error::Refused(format!(
            "{} does not hold one key: it is empty or holds a space, a control character or \
             a second line",
            path.display()
        )));
    }
    Ok(key.to_owned())
}

/// A command NanoHUB queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Queued {
    /// Why the push failed, when it did.
    pub(crate) push_error: Option<String>,
}

/// Queues a command for a Mac.
pub(crate) trait Enqueue {
    fn enqueue(&self, id: &EnrollmentId, command: &[u8]) -> Result<Queued, Error>;
}

/// NanoHUB's API over HTTP on loopback.
pub(crate) struct NanoHubHttp {
    addr: SocketAddr,
    key: String,
}

impl NanoHubHttp {
    pub(crate) fn new(url: &ApiUrl, key: String) -> NanoHubHttp {
        NanoHubHttp {
            addr: url.addr(),
            key,
        }
    }

    /// The bytes of the call's answer.
    fn call(&self, path: &str, body: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut stream = TcpStream::connect(self.addr)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let auth = STANDARD.encode(format!("nanohub:{}", self.key));
        let head = format!(
            "PUT {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Basic {auth}\r\n\
             Content-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.addr,
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        let mut answer = Vec::new();
        stream.take(ANSWER_MAX).read_to_end(&mut answer)?;
        Ok(answer)
    }
}

impl Enqueue for NanoHubHttp {
    fn enqueue(&self, id: &EnrollmentId, command: &[u8]) -> Result<Queued, Error> {
        let path = format!("/api/v1/nanomdm/enqueue/{}", id.as_str());
        let answer = self
            .call(&path, command)
            .map_err(|e| Error::NanoHub(format!("PUT {path} on {}: {e}", self.addr)))?;
        let (status, body) = parse_answer(&answer)?;
        judge(status, &body)
    }
}

/// The status and the body of the HTTP/1.1 answer `answer`, a chunked body joined.
fn parse_answer(answer: &[u8]) -> Result<(u16, Vec<u8>), Error> {
    let bad = |why: &str| Error::NanoHub(format!("the answer is not HTTP/1.1: {why}"));
    let split = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("its head does not end"))?;
    let head = std::str::from_utf8(&answer[..split]).map_err(|_| bad("its head is not UTF-8"))?;
    let body = &answer[split + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| bad("no status line"))?;
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
        })
    });
    if chunked {
        Ok((
            status,
            dechunk(body).ok_or_else(|| bad("a chunk is malformed"))?,
        ))
    } else {
        Ok((status, body.to_vec()))
    }
}

/// The chunked body `body` joined; none when a chunk is malformed or the last one is missing.
fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&body[..end]).ok()?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        body = &body[end + 2..];
        if size == 0 {
            return Some(out);
        }
        let chunk = body.get(..size)?;
        out.extend_from_slice(chunk);
        body = body.get(size..)?.strip_prefix(b"\r\n")?;
    }
}

/// What NanoHUB's answer of `status` and `body` means.
fn judge(status: u16, body: &[u8]) -> Result<Queued, Error> {
    if status == 401 {
        return Err(Error::NanoHub(
            "401: the API key is not NanoHUB's (NANOHUB_API_KEY of the unit's environment file)"
                .to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(body);
    let json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| Error::NanoHub(format!("{status}, and the answer is not JSON: {text}")))?;
    let field = |v: &serde_json::Value, name: &str| {
        v.get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let mut command_error = field(&json, "command_error");
    let mut push_error = field(&json, "push_error");
    if let Some(ids) = json.get("status").and_then(serde_json::Value::as_object) {
        for result in ids.values() {
            command_error = command_error.or_else(|| field(result, "command_error"));
            push_error = push_error.or_else(|| field(result, "push_error"));
        }
    }
    if let Some(why) = command_error {
        return Err(Error::NanoHub(format!(
            "{status}: the command was not queued: {why}"
        )));
    }
    match (status, push_error) {
        (200, push_error) => Ok(Queued { push_error }),
        (500, Some(why)) => Ok(Queued {
            push_error: Some(why),
        }),
        (status, _) => Err(Error::NanoHub(format!(
            "{status}: an answer NanoHUB's enqueue does not give: {text}"
        ))),
    }
}

#[cfg(test)]
pub(super) mod tests;
