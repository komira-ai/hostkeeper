//! The MDM services' HTTP APIs on loopback: NanoHUB's (the push certificate) and NanoDEP's
//! depserver (the Apple Business Manager token and the ADE profile).
//!
//! A call is printed as one line, `DRY-RUN: <call>` or `+ <call>`, with its method, URL and the
//! size of its body, never the body (a private key or a token travels in it). It is made only
//! with `--apply`. Each call carries HTTP Basic authentication with the service's API key, read
//! from its secret file under `MDM_ETC`, in a header: never on an argv, never printed. The
//! address is always `127.0.0.1` and the port the settings give; nothing else is dialled.
//!
//! The calls go through [`Loopback`], so a verb's tests answer them in memory; [`HttpLoopback`]
//! is the real client.

use std::fmt;
use std::io::Write;
use std::time::Duration;

use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_step::Mode;
use ureq::Agent;

use crate::secret::{self, Secret};
use crate::{Env, MdmError};

/// How long one call may take, whole: depserver's `/proxy/` calls wait on Apple.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// The largest answer read, in bytes. The answers are a certificate, a profile UUID or a short
/// JSON object.
pub const MAX_ANSWER: u64 = 1 << 20;
/// The most of a refusal's body an error quotes.
const QUOTED: usize = 300;

/// One service's API on `127.0.0.1`.
pub struct Api {
    /// What the service is called in messages: `NanoHUB`, `NanoDEP`.
    pub name: &'static str,
    /// Its loopback port.
    pub port: u16,
    /// The Basic authentication user.
    pub user: &'static str,
    /// The API key, from the service's secret file.
    pub key: Secret,
}

impl Api {
    /// NanoHUB's API, with `NANOHUB_API_KEY` from `MDM_ETC/nanohub.env`.
    ///
    /// # Errors
    /// The secret file is not a 0600 file of root's with the key.
    pub fn nanohub(env: &Env<'_>, c: &MdmConfig) -> Result<Api, MdmError> {
        let path = format!("{}/nanohub.env", c.etc());
        Ok(Api {
            name: "NanoHUB",
            port: c.nanohub_port(),
            user: "nanohub",
            key: secret::read(env.host, &path, "NANOHUB_API_KEY")?,
        })
    }

    /// depserver's API, with `NANODEP_API` from `MDM_ETC/nanodep.env`.
    ///
    /// # Errors
    /// The secret file is not a 0600 file of root's with the key.
    pub fn nanodep(env: &Env<'_>, c: &MdmConfig) -> Result<Api, MdmError> {
        let path = format!("{}/nanodep.env", c.etc());
        Ok(Api {
            name: "NanoDEP",
            port: c.dep_port(),
            user: "depserver",
            key: secret::read(env.host, &path, "NANODEP_API")?,
        })
    }

    /// The URL of `path` on this API.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

/// An HTTP method the kit calls with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// `GET`.
    Get,
    /// `PUT`.
    Put,
    /// `POST`.
    Post,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::Get => "GET",
            Method::Put => "PUT",
            Method::Post => "POST",
        })
    }
}

/// One request: its method, path with query, and body (JSON when `json`). `Debug` shows the
/// body's size, never the body.
#[derive(Clone, PartialEq, Eq)]
pub struct Call {
    /// The method.
    pub method: Method,
    /// The path and query.
    pub path: String,
    /// The body.
    pub body: Vec<u8>,
    /// Whether the body is JSON (`Content-Type: application/json;charset=UTF8`).
    pub json: bool,
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("body", &format_args!("{} bytes", self.body.len()))
            .field("json", &self.json)
            .finish()
    }
}

impl Call {
    /// `GET path`.
    pub fn get(path: impl Into<String>) -> Call {
        Call {
            method: Method::Get,
            path: path.into(),
            body: Vec::new(),
            json: false,
        }
    }

    /// `PUT path` with `body`.
    pub fn put(path: impl Into<String>, body: Vec<u8>) -> Call {
        Call {
            method: Method::Put,
            body,
            ..Call::get(path)
        }
    }

    /// `POST path` with the JSON `body`.
    pub fn post_json(path: impl Into<String>, body: Vec<u8>) -> Call {
        Call {
            method: Method::Post,
            body,
            json: true,
            ..Call::get(path)
        }
    }
}

/// What a service answered: its status and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
}

/// Sends a call to a service on `127.0.0.1`.
pub trait Loopback {
    /// Sends `call` to `api` with its Basic authentication, and returns what it answered,
    /// whatever the status.
    ///
    /// # Errors
    /// The call did not complete: the connection failed, it timed out, or the answer was
    /// larger than [`MAX_ANSWER`]; the text says why.
    fn send(&self, api: &Api, call: &Call) -> Result<Answer, String>;
}

/// `<method> <url> (<service> API[, <n> bytes])`.
pub fn shown(api: &Api, call: &Call) -> String {
    let size = match call.body.len() {
        0 => String::new(),
        n => format!(", {n} bytes"),
    };
    format!(
        "{} {} ({} API{size})",
        call.method,
        api.url(&call.path),
        api.name
    )
}

/// Prints `call`; with [`Mode::Apply`], makes it and returns the body of its 2xx answer. Any
/// other answer fails with its status and the start of its body; a dry run returns `None`.
///
/// # Errors
/// [`MdmError::Api`]: the call did not complete or was not answered 2xx; the output could not
/// be written.
pub fn call(
    env: &Env<'_>,
    api: &Api,
    call: &Call,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<Option<Vec<u8>>, MdmError> {
    let line = shown(api, call);
    if mode == Mode::DryRun {
        writeln!(out, "DRY-RUN: {line}")?;
        return Ok(None);
    }
    writeln!(out, "+ {line}")?;
    let answer = env.api.send(api, call).map_err(|why| MdmError::Api {
        call: line.clone(),
        why,
    })?;
    if !(200..300).contains(&answer.status) {
        let said = String::from_utf8_lossy(&answer.body);
        let said: String = said.trim().chars().take(QUOTED).collect();
        return Err(MdmError::Api {
            call: line,
            why: format!("answered {}: {said}", answer.status),
        });
    }
    Ok(Some(answer.body))
}

/// [`Loopback`] over ureq: plain HTTP to `127.0.0.1` only, no proxy setting read, no redirect
/// followed, [`TIMEOUT`] for the whole call and at most [`MAX_ANSWER`] bytes read.
pub struct HttpLoopback {
    agent: Agent,
}

impl HttpLoopback {
    /// The client.
    pub fn new() -> HttpLoopback {
        let config = Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .https_only(false)
            .max_redirects(0)
            .timeout_global(Some(TIMEOUT))
            .user_agent("hostkeeper-mdm")
            .build();
        HttpLoopback {
            agent: Agent::new_with_config(config),
        }
    }
}

impl Default for HttpLoopback {
    fn default() -> HttpLoopback {
        HttpLoopback::new()
    }
}

/// The value of the `Authorization` header of Basic authentication as `user` with `key`.
pub fn basic_auth(user: &str, key: &Secret) -> String {
    use base64::Engine;
    let token =
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{}", key.expose()));
    format!("Basic {token}")
}

impl Loopback for HttpLoopback {
    fn send(&self, api: &Api, call: &Call) -> Result<Answer, String> {
        let url = api.url(&call.path);
        let auth = basic_auth(api.user, &api.key);
        let sent = match call.method {
            Method::Get => self.agent.get(&url).header("Authorization", &auth).call(),
            Method::Put | Method::Post => {
                let request = match call.method {
                    Method::Put => self.agent.put(&url),
                    _ => self.agent.post(&url),
                };
                let request = request.header("Authorization", &auth);
                let request = match call.json {
                    true => request.header("Content-Type", "application/json;charset=UTF8"),
                    false => request,
                };
                request.send(call.body.as_slice())
            }
        };
        let mut answer = sent.map_err(|e| e.to_string())?;
        let status = answer.status().as_u16();
        let body = answer
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        Ok(Answer { status, body })
    }
}
