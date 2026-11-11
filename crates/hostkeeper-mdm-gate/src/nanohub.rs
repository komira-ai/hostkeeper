//! The client of NanoHUB's command queue.
//!
//! NanoHUB 0.2.0 mounts NanoMDM's API under `/api/v1/nanomdm/`, behind HTTP basic
//! authentication with the user `nanohub` and the key of its `-api-key` flag (its operations
//! guide). The gate calls one route: `PUT /api/v1/nanomdm/enqueue/<enrollment id>`, whose body
//! is the raw command plist. NanoMDM answers its `APIResult` JSON: `command_error` (at the top,
//! or per id under `status`) when the command was not queued, `push_error` when it was queued
//! but the APNs push failed. A queued command whose push failed is still queued, and the
//! device fetches it at its next check-in, so the gate counts it sent.
//!
//! The API is reached on loopback only, as NanoHUB and the design require (design section 3):
//! [`ApiUrl`] takes `http://` and a loopback address, nothing else. The key comes from a file
//! (`--mdm-api-key-file`) that passes the secret file rule, and never appears in an error or a
//! `Debug`.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures::future::BoxFuture;
use hostkeeper_wire::secret::{SecretError, read_secret};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::header::{AUTHORIZATION, CONNECTION, CONTENT_TYPE, HOST};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::TcpStream;

use crate::command::Command;
use crate::ids::{EnrollmentId, shown};

/// The flag that names the API key file, which error texts name instead of its path.
pub const KEY_FLAG: &str = "--mdm-api-key-file";

/// The flag of the API's URL.
pub const URL_FLAG: &str = "--mdm-api-url";

/// The longest API key file read.
const KEY_MAX: u64 = 4096;

/// How long one call to NanoHUB may take, connect included.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest answer read from NanoHUB.
const ANSWER_MAX: usize = 64 * 1024;

/// Where NanoHUB's API is: a loopback address and port, over plain HTTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiUrl {
    ip: IpAddr,
    port: u16,
}

/// Why an API URL is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{URL_FLAG}: {0}; it is http://<loopback address>:<port>, as http://127.0.0.1:9004, since NanoHUB's API is reached on loopback only"
)]
pub struct ApiUrlError(&'static str);

impl ApiUrl {
    /// `text` as the API's URL: `http://`, an IPv4 or bracketed IPv6 loopback address, a port,
    /// and at most a `/` after it. A name (even `localhost`), another address, a path, a query
    /// or user info is refused.
    ///
    /// # Errors
    /// [`ApiUrlError`], naming what is wrong.
    pub fn parse(text: &str) -> Result<Self, ApiUrlError> {
        let rest = text
            .strip_prefix("http://")
            .ok_or(ApiUrlError("it must start http://"))?;
        let authority = rest.strip_suffix('/').unwrap_or(rest);
        if authority.contains(['/', '?', '#', '@']) {
            return Err(ApiUrlError("it has a path, a query or user info"));
        }
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or(ApiUrlError("it names no port"))?;
        let port: u16 = port
            .parse()
            .ok()
            .filter(|port| *port != 0)
            .ok_or(ApiUrlError("its port is not 1 to 65535"))?;
        let bare = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let ip: IpAddr = bare
            .parse()
            .map_err(|_| ApiUrlError("its host is not an IP address"))?;
        if !ip.is_loopback() {
            return Err(ApiUrlError("its address is not a loopback address"));
        }
        Ok(Self { ip, port })
    }

    /// The `Host` header of a request to it.
    fn host(&self) -> String {
        match self.ip {
            IpAddr::V4(ip) => format!("{ip}:{}", self.port),
            IpAddr::V6(ip) => format!("[{ip}]:{}", self.port),
        }
    }
}

/// NanoHUB's API key. Its `Debug` shows none of it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(..)")
    }
}

/// Why the API key file is refused. The text names the flag and the rule, never the path or
/// the key.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyError {
    /// The file breaks the secret file rule or cannot be read.
    #[error(transparent)]
    File(#[from] SecretError),
    /// The file holds no key.
    #[error("{KEY_FLAG}: it must hold one key of 1 to 4096 visible ASCII characters")]
    Key,
}

impl ApiKey {
    /// The key in the file at `path`: a regular file owned by this process's user, mode 0600
    /// or 0400, not a link, holding one key of visible ASCII (surrounding whitespace dropped).
    ///
    /// # Errors
    /// [`ApiKeyError`].
    pub fn read(path: &Path) -> Result<Self, ApiKeyError> {
        let bytes = read_secret(path, KEY_FLAG, KEY_MAX)?;
        let text = String::from_utf8(bytes).map_err(|_| ApiKeyError::Key)?;
        let key = text.trim();
        let fits = text.len() as u64 <= KEY_MAX
            && !key.is_empty()
            && key.bytes().all(|b| b.is_ascii_graphic());
        if !fits {
            return Err(ApiKeyError::Key);
        }
        Ok(Self(key.to_owned()))
    }

    /// The `Authorization` header NanoHUB takes: basic, user `nanohub`.
    fn authorization(&self) -> String {
        format!("Basic {}", STANDARD.encode(format!("nanohub:{}", self.0)))
    }
}

/// Why a command was not queued. No text holds the key.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EnqueueError {
    /// The command's type is not one of the five the gate sends; NanoHUB was not called.
    #[error("the gate does not send {0} commands")]
    NotAllowed(String),
    /// NanoHUB could not be reached.
    #[error("NanoHUB's API at {url} cannot be reached: {why}")]
    Connect {
        /// The API's address.
        url: String,
        /// What the connection said.
        why: String,
    },
    /// NanoHUB did not answer within [`CALL_TIMEOUT`].
    #[error("NanoHUB's API did not answer within {}s", CALL_TIMEOUT.as_secs())]
    TimedOut,
    /// The HTTP exchange failed.
    #[error("NanoHUB's API: {0}")]
    Http(String),
    /// NanoHUB refused the API key.
    #[error("NanoHUB's API refused the key of {KEY_FLAG}")]
    Unauthorized,
    /// NanoHUB answered something other than its result.
    #[error("NanoHUB's API answered {status} with no result it reads")]
    Answer {
        /// The HTTP status.
        status: u16,
    },
    /// NanoHUB did not queue the command.
    #[error("NanoHUB did not queue the command: {0}")]
    NotQueued(String),
}

#[derive(Deserialize)]
struct ApiResult {
    #[serde(default)]
    status: std::collections::BTreeMap<String, IdResult>,
    command_error: Option<serde_json::Value>,
    push_error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct IdResult {
    command_error: Option<serde_json::Value>,
    push_error: Option<serde_json::Value>,
}

/// What a queued command's answer said besides that it is queued: the push failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    /// NanoHUB's text of a failed push, when it failed; the device fetches the command at its
    /// next check-in anyway.
    pub push_error: Option<String>,
}

/// Where the gate puts a command for a Mac: NanoHUB's queue ([`NanoHub`]), or the coordinator's own
/// (`docs/design/coordinator-mdm.md`, section 5.2), which holds the command until the Mac connects
/// and pushes it. A queue refuses a command whose type is off [`crate::COMMAND_TYPES`], and says
/// with [`EnqueueError`] why a command was not queued; the gate then asks again at the Mac's next
/// check-in.
pub trait CommandQueue: Send + Sync + std::fmt::Debug {
    /// Queues `command` for the device `id`. Queuing a command whose UUID is already queued for
    /// `id` changes nothing and succeeds.
    fn enqueue<'a>(
        &'a self,
        id: &'a EnrollmentId,
        command: &'a Command,
    ) -> BoxFuture<'a, Result<Queued, EnqueueError>>;
}

/// The client of NanoHUB's command queue.
#[derive(Clone, Debug)]
pub struct NanoHub {
    url: ApiUrl,
    key: ApiKey,
}

fn text_of(value: &serde_json::Value) -> String {
    shown(value.as_str().unwrap_or("an error"))
}

impl<T: CommandQueue + ?Sized> CommandQueue for std::sync::Arc<T> {
    fn enqueue<'a>(
        &'a self,
        id: &'a EnrollmentId,
        command: &'a Command,
    ) -> BoxFuture<'a, Result<Queued, EnqueueError>> {
        (**self).enqueue(id, command)
    }
}

impl CommandQueue for NanoHub {
    fn enqueue<'a>(
        &'a self,
        id: &'a EnrollmentId,
        command: &'a Command,
    ) -> BoxFuture<'a, Result<Queued, EnqueueError>> {
        Box::pin(NanoHub::enqueue(self, id, command))
    }
}

impl NanoHub {
    /// The client of the API at `url`, with the key `key`.
    #[must_use]
    pub fn new(url: ApiUrl, key: ApiKey) -> Self {
        Self { url, key }
    }

    /// Queues `command` for the device `id`. A type off [`crate::COMMAND_TYPES`] is refused
    /// before anything is sent.
    ///
    /// # Errors
    /// [`EnqueueError`].
    pub async fn enqueue(
        &self,
        id: &EnrollmentId,
        command: &Command,
    ) -> Result<Queued, EnqueueError> {
        if !command.is_allowed() {
            return Err(EnqueueError::NotAllowed(shown(command.request_type())));
        }
        let path = format!("/api/v1/nanomdm/enqueue/{id}");
        tokio::time::timeout(CALL_TIMEOUT, self.put(&path, command.to_plist()))
            .await
            .map_err(|_| EnqueueError::TimedOut)?
    }

    async fn put(&self, path: &str, body: Vec<u8>) -> Result<Queued, EnqueueError> {
        let host = self.url.host();
        let stream = TcpStream::connect((self.url.ip, self.url.port))
            .await
            .map_err(|why| EnqueueError::Connect {
                url: format!("http://{host}"),
                why: why.kind().to_string(),
            })?;
        let http = |why: hyper::Error| EnqueueError::Http(why.to_string());
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
                .await
                .map_err(http)?;
        let driver = tokio::spawn(connection);
        let request = Request::put(path)
            .header(HOST, &host)
            .header(AUTHORIZATION, self.key.authorization())
            .header(CONTENT_TYPE, "application/xml")
            .header(CONNECTION, "close")
            .body(Full::new(Bytes::from(body)))
            // The path is of a checked enrollment id and every header is ASCII the gate made, so
            // the builder takes them; a request it refused would be an empty `GET /`, which
            // NanoHUB answers 401, an error the caller sees.
            .unwrap_or_default();
        let answered = sender.send_request(request).await;
        let response = answered.map_err(http)?;
        let status = response.status();
        let body = Limited::new(response.into_body(), ANSWER_MAX)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes);
        driver.abort();
        if status == StatusCode::UNAUTHORIZED {
            return Err(EnqueueError::Unauthorized);
        }
        let answer = || EnqueueError::Answer {
            status: status.as_u16(),
        };
        let result: ApiResult = body
            .ok()
            .and_then(|body| serde_json::from_slice(&body).ok())
            .ok_or_else(answer)?;
        let mut command_errors = result.command_error.iter().chain(
            result
                .status
                .values()
                .filter_map(|id| id.command_error.as_ref()),
        );
        if let Some(why) = command_errors.next() {
            return Err(EnqueueError::NotQueued(text_of(why)));
        }
        if !(status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR) {
            return Err(answer());
        }
        let push_error = result
            .push_error
            .iter()
            .chain(
                result
                    .status
                    .values()
                    .filter_map(|id| id.push_error.as_ref()),
            )
            .next()
            .map(text_of);
        // NanoMDM answers 500 when the push failed, with the command queued all the same.
        if status != StatusCode::OK && push_error.is_none() {
            return Err(answer());
        }
        Ok(Queued { push_error })
    }
}
