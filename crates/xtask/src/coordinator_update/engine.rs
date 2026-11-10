//! What `coordinator-update` asks of Docker and of the coordinator, and the adapters that ask it
//! through `curl`: the Docker Engine API over its unix socket, and the coordinator's
//! `GET /v1/version` over HTTPS. Each runs `curl` through an [`Exec`], so a test checks the exact
//! command line against a fake one and nothing reaches Docker or the network.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use serde_json::Value;

use crate::release::contract::Version;
use crate::release::ports::{Exec, PortError};

/// The Engine API version every request names.
const API: &str = "v1.43";

/// The file in the download directory a request body is written to (mode 0600) and removed from.
const BODY: &str = "engine-request.json";

/// What the verb asks of Docker. Every method answers what it did or the error of the call; a
/// container or image that is not there is `None`, not an error.
pub(crate) trait Engine {
    /// The inspect JSON of the container `name_or_id`.
    fn container(&self, name_or_id: &str) -> Result<Option<Value>, PortError>;
    /// The inspect JSON of the image `reference` (a tag or an id).
    fn image(&self, reference: &str) -> Result<Option<Value>, PortError>;
    /// Loads the image tarball `tar`; an error line in docker's stream is an error.
    fn load(&self, tar: &Path) -> Result<(), PortError>;
    /// Creates the container `name` from `body`; its id.
    fn create(&self, name: &str, body: &Value) -> Result<String, PortError>;
    /// Stops the container, SIGKILL after `timeout_secs`.
    fn stop(&self, id: &str, timeout_secs: u64) -> Result<(), PortError>;
    fn start(&self, id: &str) -> Result<(), PortError>;
    /// Deletes the container; its volumes are kept.
    fn remove(&self, id: &str) -> Result<(), PortError>;
    fn rename(&self, id: &str, name: &str) -> Result<(), PortError>;
    /// The last `tail` lines the container wrote, standard output and error together.
    fn logs(&self, id: &str, tail: u32) -> Result<String, PortError>;
}

/// What the coordinator's `GET /v1/version` said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Probed {
    /// 200: it runs this version (and, with `at_least`, that version is at least it).
    Running(Version),
    /// 409: it runs an older version than `at_least`.
    Older,
    /// 404: an older coordinator that has no such route.
    NotFound,
    /// Anything else: why no answer counts.
    Failed(String),
}

/// The coordinator's version route.
pub(crate) trait Probe {
    /// `GET /v1/version`, with `?at_least=` when given.
    fn version(&self, at_least: Option<&Version>) -> Probed;
}

/// What curl printed: the status and the body.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

/// Splits what curl printed with `--write-out '\n%{http_code}'`: the body, a line break, the
/// status.
fn split_reply(stdout: &[u8]) -> Option<Reply> {
    let cut = stdout.iter().rposition(|&b| b == b'\n')?;
    let status = std::str::from_utf8(&stdout[cut + 1..]).ok()?.parse().ok()?;
    Some(Reply {
        status,
        body: stdout[..cut].to_vec(),
    })
}

/// Runs `curl` with `args`; the reply, or why there is none.
fn curl(exec: &dyn Exec, program: &str, args: &[String], what: &str) -> Result<Reply, PortError> {
    let ran = exec
        .spawn(program, args)
        .map_err(|e| PortError::new(what, format!("{program} could not be started: {e}")))?;
    if !ran.success {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(PortError::new(
            what,
            format!("curl failed: {}", said.trim()),
        ));
    }
    split_reply(&ran.stdout).ok_or_else(|| PortError::new(what, "curl printed no HTTP status"))
}

/// The text of a reply body, for an error.
fn said(reply: &Reply) -> String {
    String::from_utf8_lossy(&reply.body).trim().to_owned()
}

/// A request body.
enum Body<'a> {
    None,
    Json(&'a Value),
    Tar(&'a Path),
}

/// The Engine API through `curl --unix-socket`.
pub(crate) struct CurlEngine<'a> {
    exec: &'a dyn Exec,
    curl: &'a str,
    socket: &'a Path,
    /// The directory a JSON body is written to for the length of a request.
    scratch: &'a Path,
}

impl<'a> CurlEngine<'a> {
    pub(crate) fn new(
        exec: &'a dyn Exec,
        curl: &'a str,
        socket: &'a Path,
        scratch: &'a Path,
    ) -> CurlEngine<'a> {
        CurlEngine {
            exec,
            curl,
            socket,
            scratch,
        }
    }

    /// One request; the reply, whatever its status.
    fn request(&self, method: &str, path: &str, body: &Body<'_>) -> Result<Reply, PortError> {
        let what = format!("docker {method} {path}");
        let socket = self
            .socket
            .to_str()
            .ok_or_else(|| PortError::new(&what, "the docker socket path is not UTF-8"))?;
        let mut args: Vec<String> = ["-q", "--silent", "--show-error", "--unix-socket", socket]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        args.extend(["-X".to_owned(), method.to_owned()]);
        let file = self.scratch.join(BODY);
        match body {
            Body::None => {}
            Body::Json(_) => args.extend(data("application/json", &file)),
            Body::Tar(tar) => args.extend(data("application/x-tar", tar)),
        }
        args.extend([
            "--write-out".to_owned(),
            "\n%{http_code}".to_owned(),
            format!("http://localhost/{API}{path}"),
        ]);
        if let Body::Json(json) = body {
            write_body(&file, json).map_err(|e| PortError::new(&what, e))?;
        }
        let reply = curl(self.exec, self.curl, &args, &what);
        if matches!(body, Body::Json(_)) {
            // A body that cannot be removed is replaced by the next request's.
            let _ = std::fs::remove_file(&file);
        }
        reply
    }

    /// A request that must answer one of `ok`; the reply.
    fn expect(
        &self,
        (method, path): (&str, &str),
        body: &Body<'_>,
        ok: &[u16],
    ) -> Result<Reply, PortError> {
        let reply = self.request(method, path, body)?;
        if ok.contains(&reply.status) {
            return Ok(reply);
        }
        Err(PortError::new(
            format!("docker {method} {path}"),
            format!("HTTP {}: {}", reply.status, said(&reply)),
        ))
    }

    /// An inspect: the JSON, none for a 404.
    fn inspect(&self, path: &str) -> Result<Option<Value>, PortError> {
        let reply = self.expect(("GET", path), &Body::None, &[200, 404])?;
        if reply.status == 404 {
            return Ok(None);
        }
        serde_json::from_slice(&reply.body)
            .map(Some)
            .map_err(|e| PortError::new(format!("docker GET {path}"), format!("not JSON: {e}")))
    }
}

/// The arguments that send the file `path` as the request body of type `content_type`.
fn data(content_type: &str, path: &Path) -> [String; 4] {
    [
        "--header".to_owned(),
        format!("Content-Type: {content_type}"),
        "--data-binary".to_owned(),
        format!("@{}", path.display()),
    ]
}

/// Writes `json` to `file`, which is made new with mode 0600.
fn write_body(file: &Path, json: &Value) -> Result<(), String> {
    let _ = std::fs::remove_file(file);
    let failed = |e: std::io::Error| format!("write {}: {e}", file.display());
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)
        .map_err(failed)?;
    out.write_all(json.to_string().as_bytes()).map_err(failed)
}

impl Engine for CurlEngine<'_> {
    fn container(&self, name_or_id: &str) -> Result<Option<Value>, PortError> {
        self.inspect(&format!("/containers/{name_or_id}/json"))
    }

    fn image(&self, reference: &str) -> Result<Option<Value>, PortError> {
        self.inspect(&format!("/images/{reference}/json"))
    }

    fn load(&self, tar: &Path) -> Result<(), PortError> {
        let reply = self.expect(("POST", "/images/load"), &Body::Tar(tar), &[200])?;
        // The answer is a stream of JSON lines; one with an `error` is a load that failed.
        for line in String::from_utf8_lossy(&reply.body).lines() {
            if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(line)
                && let Some(why) = fields.get("error")
            {
                let why = why.as_str().map_or_else(|| why.to_string(), str::to_owned);
                return Err(PortError::new("docker POST /images/load", why));
            }
        }
        Ok(())
    }

    fn create(&self, name: &str, body: &Value) -> Result<String, PortError> {
        let path = format!("/containers/create?name={name}");
        let what = format!("docker POST {path}");
        let reply = self.expect(("POST", &path), &Body::Json(body), &[201])?;
        let made: Value = serde_json::from_slice(&reply.body)
            .map_err(|e| PortError::new(&what, format!("not JSON: {e}")))?;
        made["Id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| PortError::new(&what, "the answer has no Id"))
    }

    fn stop(&self, id: &str, timeout_secs: u64) -> Result<(), PortError> {
        let path = format!("/containers/{id}/stop?t={timeout_secs}");
        self.expect(("POST", &path), &Body::None, &[204, 304])
            .map(|_| ())
    }

    fn start(&self, id: &str) -> Result<(), PortError> {
        let path = format!("/containers/{id}/start");
        self.expect(("POST", &path), &Body::None, &[204, 304])
            .map(|_| ())
    }

    fn remove(&self, id: &str) -> Result<(), PortError> {
        let path = format!("/containers/{id}");
        self.expect(("DELETE", &path), &Body::None, &[204])
            .map(|_| ())
    }

    fn rename(&self, id: &str, name: &str) -> Result<(), PortError> {
        let path = format!("/containers/{id}/rename?name={name}");
        self.expect(("POST", &path), &Body::None, &[204])
            .map(|_| ())
    }

    fn logs(&self, id: &str, tail: u32) -> Result<String, PortError> {
        let path = format!("/containers/{id}/logs?stdout=1&stderr=1&tail={tail}");
        let reply = self.expect(("GET", &path), &Body::None, &[200])?;
        Ok(demultiplex(&reply.body))
    }
}

/// The text of a multiplexed log stream: frames of an 8-byte header (the stream, three zero
/// bytes, the payload's length as a big-endian u32) and the payload. A stream that does not parse
/// as frames (a container with a terminal writes plain text) is returned as it is.
fn demultiplex(stream: &[u8]) -> String {
    let mut text = Vec::new();
    let mut rest = stream;
    while !rest.is_empty() {
        let frame = rest.split_first_chunk::<8>().and_then(|(head, tail)| {
            let size = usize::try_from(u32::from_be_bytes([head[4], head[5], head[6], head[7]]));
            let framed = head[0] <= 2 && head[1..4] == [0, 0, 0];
            let size = size.ok().filter(|&size| framed && tail.len() >= size)?;
            Some(tail.split_at(size))
        });
        let Some((payload, tail)) = frame else {
            return String::from_utf8_lossy(stream).into_owned();
        };
        text.extend_from_slice(payload);
        rest = tail;
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// The coordinator's version route through `curl` over HTTPS, trusting the CA file alone.
pub(crate) struct CurlProbe<'a> {
    exec: &'a dyn Exec,
    curl: &'a str,
    /// `https://host[:port]`.
    url: &'a str,
    ca_file: &'a Path,
}

impl<'a> CurlProbe<'a> {
    pub(crate) fn new(
        exec: &'a dyn Exec,
        curl: &'a str,
        url: &'a str,
        ca_file: &'a Path,
    ) -> CurlProbe<'a> {
        CurlProbe {
            exec,
            curl,
            url,
            ca_file,
        }
    }
}

impl Probe for CurlProbe<'_> {
    fn version(&self, at_least: Option<&Version>) -> Probed {
        let query = at_least.map_or_else(String::new, |v| format!("?at_least={v}"));
        let url = format!("{}/v1/version{query}", self.url);
        let what = format!("GET {url}");
        let ca = self.ca_file.display().to_string();
        let args: Vec<String> = [
            "-q",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-redirs",
            "0",
            "--cacert",
            &ca,
            "--max-time",
            "10",
            "--write-out",
            "\n%{http_code}",
            "--url",
            &url,
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
        let reply = match curl(self.exec, self.curl, &args, &what) {
            Ok(reply) => reply,
            Err(e) => return Probed::Failed(e.to_string()),
        };
        match reply.status {
            200 => serde_json::from_slice::<Value>(&reply.body)
                .ok()
                .and_then(|v| v["version"].as_str()?.parse().ok())
                .map_or_else(
                    || Probed::Failed(format!("{what}: no version in {:?}", said(&reply))),
                    Probed::Running,
                ),
            409 => Probed::Older,
            404 => Probed::NotFound,
            other => Probed::Failed(format!("{what}: HTTP {other}")),
        }
    }
}
