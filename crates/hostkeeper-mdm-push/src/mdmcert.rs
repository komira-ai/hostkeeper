//! The request to mdmcert.download, the community service that signs a push certificate CSR
//! (`docs/design/coordinator-mdm.md`, section 4.4). The formats are read from the service's own
//! client, micromdm's `cmd/mdmctl/mdmcert.download.go`, and the service's reply to the request is
//! only whether it took it: the signed request arrives **by email** to the address in the
//! request, and the operator uploads that attachment ([`crate::p7m`]).
//!
//! `POST https://mdmcert.download/api/v1/signrequest`, `Content-Type: application/json`, the body
//! `{"csr": base64(PEM CSR), "email": ..., "key": API_KEY, "encrypt": base64(PEM encryption
//! certificate)}`; the answer is `200` and `{"result": "success"}`, or another result with a
//! `reason`.
//!
//! **Not verified** (one real request with the owner's email would; none was sent): that `API_KEY`
//! is still accepted, the name and encoding of the emailed attachment beyond what the client
//! source says (hex text of the PKCS#7), and what `reason` says for each refusal.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::header::{CONTENT_TYPE, HOST, HeaderValue, USER_AGENT};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use rustls::{ClientConfig, RootCertStore};
use serde::{Deserialize, Serialize};

use crate::BoxFuture;
use crate::tls::{self, Endpoint, TlsError};

/// The host of the service.
pub const HOST_NAME: &str = "mdmcert.download";

/// The path of the sign request.
pub const SIGN_REQUEST_PATH: &str = "/api/v1/signrequest";

/// The service's API key: a constant every client of the community service sends, from the source
/// of micromdm's client (which took it from commandment's `mdmcert.py`). It is public and not a
/// secret of this organization, so it is a constant here and not a stored value.
pub const API_KEY: &str = "f847aea2ba06b41264d587b229e2712c89b1490a1208b7ff1aafab5bb40d47bc";

/// How much of the service's answer is read.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// What a request to the service came back as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpReply {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
}

/// Why the service could not be asked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// The port the request goes through: the real HTTPS client, or a fake.
pub trait MdmcertTransport: Send + Sync {
    /// Posts `body` (JSON) to the service's sign-request URL.
    fn post_sign_request(&self, body: Vec<u8>) -> BoxFuture<'_, Result<HttpReply, TransportError>>;
}

/// Why the service did not take a request.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MdmcertError {
    /// The service could not be reached.
    #[error("mdmcert.download could not be reached: {0}")]
    Transport(String),
    /// The service answered another status than `200`.
    #[error("mdmcert.download answered status {0}")]
    Status(u16),
    /// The answer is not the JSON the client of micromdm reads.
    #[error("mdmcert.download's answer is not understood")]
    Malformed,
    /// The service answered that it did not take the request.
    #[error("mdmcert.download refused the request: {result}: {reason}")]
    Refused {
        /// The `result`.
        result: String,
        /// The `reason`, or empty.
        reason: String,
    },
}

/// The body of a sign request.
#[derive(Serialize)]
struct SignRequest<'a> {
    csr: String,
    email: &'a str,
    key: &'a str,
    encrypt: String,
}

#[derive(Deserialize)]
struct SignAnswer {
    #[serde(default)]
    result: String,
    #[serde(default)]
    reason: String,
}

/// The JSON body of a sign request for `email`, the CSR and the encryption certificate (PEM).
#[must_use]
pub fn sign_request_body(email: &str, csr_pem: &str, encryption_cert_pem: &str) -> Vec<u8> {
    let request = SignRequest {
        csr: STANDARD.encode(csr_pem),
        email,
        key: API_KEY,
        encrypt: STANDARD.encode(encryption_cert_pem),
    };
    // Strings and a fixed shape: serializing cannot fail.
    serde_json::to_vec(&request).unwrap_or_default()
}

/// Posts the request through `transport` and reads the answer.
///
/// # Errors
/// [`MdmcertError`].
pub async fn submit(
    transport: &dyn MdmcertTransport,
    email: &str,
    csr_pem: &str,
    encryption_cert_pem: &str,
) -> Result<(), MdmcertError> {
    let reply = transport
        .post_sign_request(sign_request_body(email, csr_pem, encryption_cert_pem))
        .await
        .map_err(|why| MdmcertError::Transport(why.0))?;
    if reply.status != 200 {
        return Err(MdmcertError::Status(reply.status));
    }
    let answer: SignAnswer =
        serde_json::from_slice(&reply.body).map_err(|_| MdmcertError::Malformed)?;
    if answer.result == "success" {
        Ok(())
    } else {
        Err(MdmcertError::Refused {
            result: answer.result,
            reason: answer.reason,
        })
    }
}

/// The HTTPS client of the service.
pub struct HttpsTransport {
    endpoint: Endpoint,
    config: Arc<ClientConfig>,
    timeout: Duration,
}

impl HttpsTransport {
    /// The transport to `endpoint` trusting `roots`, giving up after `timeout`.
    ///
    /// # Errors
    /// [`TlsError`].
    pub fn new(
        endpoint: Endpoint,
        roots: RootCertStore,
        timeout: Duration,
    ) -> Result<Self, TlsError> {
        Ok(Self {
            endpoint,
            config: tls::client_config(roots, None, &[b"http/1.1"])?,
            timeout,
        })
    }

    /// The transport to the real service, trusting the public web's roots.
    ///
    /// # Errors
    /// [`TlsError`].
    pub fn production(timeout: Duration) -> Result<Self, TlsError> {
        Self::new(Endpoint::new(HOST_NAME, 443), tls::public_roots(), timeout)
    }

    async fn post(&self, body: Vec<u8>) -> Result<HttpReply, TransportError> {
        let failed = |why: &dyn std::fmt::Display| TransportError(why.to_string());
        let stream = tls::connect(&self.endpoint, self.config.clone(), self.timeout)
            .await
            .map_err(|why| failed(&why))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|why| failed(&why))?;
        tokio::spawn(async move {
            drop(connection.await);
        });
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(SIGN_REQUEST_PATH)
            .body(Full::new(Bytes::from(body)))
            .map_err(|why| failed(&why))?;
        let headers = request.headers_mut();
        let host = HeaderValue::from_str(&self.endpoint.server_name).map_err(|why| failed(&why))?;
        headers.insert(HOST, host);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("hostkeeper-coordinator"),
        );
        let answer = sender
            .send_request(request)
            .await
            .map_err(|why| failed(&why))?;
        let status = answer.status().as_u16();
        let body = Limited::new(answer.into_body(), MAX_ANSWER_BYTES)
            .collect()
            .await
            .map_err(|why| failed(&why))?
            .to_bytes()
            .to_vec();
        Ok(HttpReply { status, body })
    }
}

impl MdmcertTransport for HttpsTransport {
    fn post_sign_request(&self, body: Vec<u8>) -> BoxFuture<'_, Result<HttpReply, TransportError>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.post(body))
                .await
                .unwrap_or_else(|_| Err(TransportError("the request timed out".to_owned())))
        })
    }
}

/// Why a decrypted reply is not the signed request for the pending CSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignedRequestError {
    /// It is not a property list with a dictionary at its root.
    #[error("the decrypted reply is not a property list")]
    NotPlist,
    /// A key of the signed request is missing or is not a string.
    #[error("the signed request has no {0}")]
    Missing(&'static str),
    /// The CSR in it is not the one of the pending request.
    #[error("the signed request is for another CSR than the pending one")]
    CsrMismatch,
}

/// The keys of the property list Apple's push portal takes, as micromdm's `mdmcertutil` writes it.
const CSR_KEY: &str = "PushCertRequestCSR";
const CHAIN_KEY: &str = "PushCertCertificateChain";
const SIGNATURE_KEY: &str = "PushCertSignature";

/// Checks that `plain`, the decrypted reply, is a signed push certificate request (a property
/// list with `PushCertRequestCSR`, `PushCertCertificateChain` and `PushCertSignature`) whose CSR
/// is `pending_csr_pem`, the one this coordinator made.
///
/// The CSR may be written as the PEM or as base64 of the DER or of the PEM; the keys and these
/// forms are as micromdm's client writes them and are **not verified** against a real reply.
///
/// # Errors
/// [`SignedRequestError`].
pub fn check_signed_request(plain: &[u8], pending_csr_pem: &str) -> Result<(), SignedRequestError> {
    use rustls::pki_types::CertificateSigningRequestDer;
    use rustls::pki_types::pem::PemObject as _;

    let value = plist::Value::from_reader(std::io::Cursor::new(plain))
        .map_err(|_| SignedRequestError::NotPlist)?;
    let dict = value.as_dictionary().ok_or(SignedRequestError::NotPlist)?;
    let text = |key: &'static str| {
        dict.get(key)
            .and_then(plist::Value::as_string)
            .filter(|text| !text.is_empty())
            .ok_or(SignedRequestError::Missing(key))
    };
    let csr = text(CSR_KEY)?;
    text(CHAIN_KEY)?;
    text(SIGNATURE_KEY)?;
    let pending = CertificateSigningRequestDer::from_pem_slice(pending_csr_pem.as_bytes())
        .map_err(|_| SignedRequestError::CsrMismatch)?;
    let der_of = |bytes: &[u8]| {
        CertificateSigningRequestDer::from_pem_slice(bytes)
            .map(|der| der.as_ref().to_vec())
            .unwrap_or_else(|_| bytes.to_vec())
    };
    let mut candidate = csr.as_bytes().to_vec();
    // As written, then as base64 of it, then as base64 of that (the PEM of a CSR in base64).
    for _ in 0..3 {
        if der_of(&candidate) == pending.as_ref() {
            return Ok(());
        }
        let Ok(decoded) = STANDARD.decode(&candidate) else {
            break;
        };
        candidate = decoded;
    }
    Err(SignedRequestError::CsrMismatch)
}

#[cfg(test)]
#[path = "mdmcert_tests.rs"]
mod tests;
