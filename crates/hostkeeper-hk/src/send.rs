//! One HTTPS request, made with `ureq`: HTTPS only, TLS 1.2 or later, the roots it is given and
//! no other, no redirect followed, no proxy, and a status that is never an error.

use std::time::Duration;

use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider, parse_pem};

/// The most bytes of an answer that are read.
const MAX_ANSWER_BYTES: u64 = 256 * 1024 * 1024;

/// What a request is: a GET with no body, or a POST of JSON. The bearer token is visible ASCII.
pub(crate) enum Method<'a> {
    /// A GET, with a bearer token or none.
    Get(Option<&'a str>),
    /// A POST of this body as `application/json`, with a bearer token.
    Post { body: &'a [u8], bearer: &'a str },
}

/// An answer: its status and its body.
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

/// The longest a request may wait to connect and take in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) connect: Duration,
    pub(crate) total: Duration,
}

/// The certificates of `pem` as DER, each one `begin certificate` section of it.
pub(crate) fn certificates(pem: &[u8]) -> Vec<Certificate<'static>> {
    parse_pem(pem)
        .filter_map(Result::ok)
        .filter_map(|item| match item {
            ureq::tls::PemItem::Certificate(cert) => Some(cert),
            _ => None,
        })
        .collect()
}

/// `text` as one line: its words joined by one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sends the request to `url`, with the `Authorization: Bearer` header when `bearer` (visible
/// ASCII) is given,
/// trusting `roots` alone. The error is a line saying why there is no answer.
pub(crate) fn send(
    url: &str,
    method: &Method<'_>,
    roots: &[Certificate<'static>],
    limits: Limits,
) -> Result<Answer, String> {
    if roots.is_empty() {
        return Err("no certificate to trust".to_owned());
    }
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::new_with_certs(roots))
        .build();
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .proxy(None)
        .tls_config(tls)
        .timeout_connect(Some(limits.connect))
        .timeout_global(Some(limits.total))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = match method {
        Method::Get(None) => agent.get(url).call(),
        Method::Get(Some(token)) => agent
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .call(),
        Method::Post { body, bearer } => agent
            .post(url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {bearer}"))
            .send(*body),
    };
    let mut response = response.map_err(|error| one_line(&error.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_vec()
        .map_err(|error| one_line(&error.to_string()))?;
    Ok(Answer { status, body })
}
