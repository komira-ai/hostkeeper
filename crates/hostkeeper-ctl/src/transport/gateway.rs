//! The client of the coordinator's HTTP/JSON gateway: `POST /<package>.<Service>/<Method>` with
//! the request as proto3 canonical JSON and the token as `Authorization: Bearer <token>`, over
//! HTTP/1.1 on TLS. A 200 carries the response as canonical JSON; any other status carries
//! `{"code": "<gRPC code name>", "reason": "<token>"}`, which becomes that status.
//!
//! The request and response are turned between protobuf and JSON with the descriptors, so the
//! gateway is called through the same table and the same [`CtlTransport`] as gRPC. One
//! connection per call (`Connection: close`), under one deadline, `--timeout-secs`, that covers
//! the name lookup, every address tried, TLS and each read and write ([`deadline`]).
//!
//! A response is read up to [`MAX_RESPONSE_BYTES`] of HTTP, its head and its body as received.
//! That limit is the gateway's own and is not tonic's limit on a decoded gRPC message: JSON is
//! larger than the protobuf it stands for (bytes become base64, strings gain escapes), so a
//! reply that the gRPC transport accepts near its limit can be refused here. A reply is read as
//! one JSON document; trailing content, and an enum name that this build does not know, are
//! refused ([`enums`]), while fields it does not know are skipped.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage, MethodDescriptor};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use tonic::metadata::MetadataMap;
use tonic::{Code, Status};

use self::deadline::{Clock, Deadline, DeadlineIo, Monotonic, connect_within, failed};
use super::tls::Material;
use super::{ConnectError, CtlTransport, Target};
use crate::{codes, json};

mod deadline;
mod enums;

/// The most bytes of a response read: its head and its body.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// The HTTP/JSON gateway at `--endpoint`.
#[derive(Debug, Clone)]
pub struct Gateway {
    host: String,
    port: u16,
    authority: String,
    server_name: ServerName<'static>,
    tls: Arc<ClientConfig>,
    pool: DescriptorPool,
    timeout: Duration,
}

impl Gateway {
    /// The gateway of `target`, its messages read with the descriptor set `descriptors`.
    pub(crate) fn new(
        target: &Target,
        material: &Material,
        descriptors: &[u8],
    ) -> Result<Gateway, ConnectError> {
        let server_name = ServerName::try_from(target.endpoint.host().to_owned()).map_err(|e| {
            ConnectError::ServerName {
                endpoint: target.endpoint.uri().to_string(),
                why: e.to_string(),
            }
        })?;
        let pool = DescriptorPool::decode(descriptors)
            .map_err(|e| ConnectError::Descriptors(e.to_string()))?;
        Ok(Gateway {
            host: target.endpoint.host().to_owned(),
            port: target.endpoint.port(),
            authority: target
                .endpoint
                .uri()
                .authority()
                .map_or_else(String::new, |a| a.as_str().to_owned()),
            server_name,
            tls: material.client_config(b"http/1.1")?,
            pool,
            timeout: target.timeout,
        })
    }

    /// A TLS connection to the gateway, every read and write of it held to `clock`'s budget of
    /// the call. The TLS handshake happens on the first write.
    fn open<'c>(
        &self,
        clock: &'c dyn Clock,
    ) -> Result<StreamOwned<ClientConnection, DeadlineIo<'c, TcpStream>>, Status> {
        let deadline = Deadline::new(clock, self.timeout);
        let what = format!("cannot connect to {}:{}", self.host, self.port);
        let (host, port) = (self.host.clone(), self.port);
        let tcp = connect_within(
            &what,
            &deadline,
            move || {
                (host.as_str(), port)
                    .to_socket_addrs()
                    .map(|found| found.collect())
            },
            TcpStream::connect_timeout,
        )?;
        let connection = ClientConnection::new(self.tls.clone(), self.server_name.clone())
            .map_err(|e| Status::internal(format!("cannot start TLS: {e}")))?;
        Ok(StreamOwned::new(connection, DeadlineIo::new(tcp, deadline)))
    }
}

impl CtlTransport for Gateway {
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status> {
        let method = method(&self.pool, path)?;
        let clock = Monotonic::start();
        let mut stream = self.open(&clock)?;
        exchange(
            &mut stream,
            &self.authority,
            &method,
            path,
            request,
            metadata,
        )
    }
}

/// The method at a gRPC path, from the descriptors.
pub(crate) fn method(pool: &DescriptorPool, path: &str) -> Result<MethodDescriptor, Status> {
    path.strip_prefix('/')
        .and_then(|p| p.split_once('/'))
        .and_then(|(service, method)| {
            pool.get_service_by_name(service)?
                .methods()
                .find(|m| m.name() == method)
        })
        .ok_or_else(|| Status::internal(format!("the descriptors have no method at {path:?}")))
}

/// A connection a request is written to and its response read from. One trait object, so that
/// [`exchange`] and the readers are compiled once whatever the connection is.
pub(crate) trait Stream: Read + Write {}

impl<S: Read + Write> Stream for S {}

/// One request and its response on `stream`.
pub(crate) fn exchange(
    stream: &mut dyn Stream,
    authority: &str,
    method: &MethodDescriptor,
    path: &str,
    request: Bytes,
    metadata: &MetadataMap,
) -> Result<Bytes, Status> {
    let message = DynamicMessage::decode(method.input(), request)
        .map_err(|e| Status::internal(format!("the request does not decode: {e}")))?;
    let body = json::compact(&message);
    let mut bytes = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )
    .into_bytes();
    if let Some(token) = metadata.get("authorization") {
        bytes.extend_from_slice(b"Authorization: ");
        bytes.extend_from_slice(token.as_encoded_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(body.as_bytes());
    let lost = |e: std::io::Error| failed(&e, "the gateway connection failed");
    stream.write_all(&bytes).map_err(lost)?;
    stream.flush().map_err(lost)?;

    let (status, body) = read_response(&mut BufReader::new(stream)).map_err(|e| match e {
        ReadError::Io(e) => lost(e),
        ReadError::Malformed(why) => Status::internal(format!("malformed reply: {why}")),
    })?;
    if status == 200 {
        return decode_response(method, &body);
    }
    Err(error_status(status, &body))
}

/// The encoded response of a 200's JSON `body`: one document and nothing after it (but white
/// space), in which no enum name of a known field is unknown to this build.
fn decode_response(method: &MethodDescriptor, body: &[u8]) -> Result<Bytes, Status> {
    let refuse = |why: String| Status::internal(format!("malformed reply: {why}"));
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| refuse(e.to_string()))?;
    enums::check(&value, &method.output(), "").map_err(Status::internal)?;
    let options = DeserializeOptions::new().deny_unknown_fields(false);
    let response = DynamicMessage::deserialize_with_options(method.output(), value, &options)
        .map_err(|e| refuse(e.to_string()))?;
    Ok(Bytes::from(response.encode_to_vec()))
}

/// The status an error response carries: `{"code": "<name>", "reason": "<token>"}`.
pub(crate) fn error_status(http_status: u16, body: &[u8]) -> Status {
    let parsed: Option<(Code, String)> = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let code = codes::from_name(v.get("code")?.as_str()?)?;
            let reason = v.get("reason")?.as_str()?.to_owned();
            Some((code, reason))
        });
    match parsed {
        Some((code, reason)) if code != Code::Ok => Status::new(code, reason),
        _ => Status::unknown(format!(
            "malformed reply: HTTP {http_status} without a gateway error body"
        )),
    }
}

/// Why a response could not be read.
#[derive(Debug)]
pub(crate) enum ReadError {
    Io(std::io::Error),
    Malformed(String),
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> ReadError {
        ReadError::Io(e)
    }
}

fn malformed(why: impl Into<String>) -> ReadError {
    ReadError::Malformed(why.into())
}

/// One line of the head, without its CRLF, counted against `budget`.
fn line(reader: &mut dyn BufRead, budget: &mut usize) -> Result<String, ReadError> {
    let mut raw = Vec::new();
    Read::take(&mut *reader, *budget as u64).read_until(b'\n', &mut raw)?;
    *budget = budget.saturating_sub(raw.len());
    if raw.last() != Some(&b'\n') {
        return Err(malformed(
            "the response ended or grew too long inside a line",
        ));
    }
    raw.pop();
    if raw.last() == Some(&b'\r') {
        raw.pop();
    }
    String::from_utf8(raw).map_err(|_| malformed("a line of the head is not UTF-8"))
}

/// The status code of a status line: `HTTP/1.1 ` or `HTTP/1.0 `, three digits, then a space or
/// the end of the line.
fn status_code(line: &str) -> Option<u16> {
    let rest = line
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| line.strip_prefix("HTTP/1.0 "))?;
    let digits = rest.get(..3)?;
    if !digits.bytes().all(|b| b.is_ascii_digit())
        || !matches!(rest.as_bytes().get(3), None | Some(b' '))
    {
        return None;
    }
    digits
        .parse()
        .ok()
        .filter(|code| (100..=599).contains(code))
}

/// How a response says its body is framed.
struct Framing {
    length: Option<usize>,
    chunked: bool,
}

/// The headers after a status line, up to the empty line, counted against `budget`. A
/// `Content-Length` given twice must agree, and may not come with `Transfer-Encoding`.
fn headers(reader: &mut dyn BufRead, budget: &mut usize) -> Result<Framing, ReadError> {
    let mut framing = Framing {
        length: None,
        chunked: false,
    };
    loop {
        let header = line(reader, budget)?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| malformed(format!("not a header: {header:?}")))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let length = Some(value)
                .filter(|v| v.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|v| v.parse::<usize>().ok())
                .ok_or_else(|| malformed(format!("Content-Length {value:?}")))?;
            match framing.length {
                Some(before) if before != length => {
                    return Err(malformed(format!(
                        "Content-Length given twice, as {before} and {length}"
                    )));
                }
                _ => framing.length = Some(length),
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            framing.chunked = value.eq_ignore_ascii_case("chunked");
            if !framing.chunked {
                return Err(malformed(format!("Transfer-Encoding {value:?}")));
            }
        }
    }
    if framing.chunked && framing.length.is_some() {
        return Err(malformed("both Transfer-Encoding and Content-Length"));
    }
    Ok(framing)
}

/// The status and body of an HTTP/1.1 response: by `Content-Length`, by chunks, or to the end of
/// the connection; at most [`MAX_RESPONSE_BYTES`] in all, counting every byte read, head and
/// body. Informational responses (1xx) before the final one are skipped, within the same budget;
/// a 101 is refused, as no upgrade was asked for.
pub(crate) fn read_response(reader: &mut dyn BufRead) -> Result<(u16, Vec<u8>), ReadError> {
    let mut budget = MAX_RESPONSE_BYTES;
    let (status, framing) = loop {
        let status_line = line(reader, &mut budget)?;
        let status = status_code(&status_line)
            .ok_or_else(|| malformed(format!("not an HTTP/1.1 status line: {status_line:?}")))?;
        let framing = headers(reader, &mut budget)?;
        match status {
            101 => {
                return Err(malformed(
                    "101 Switching Protocols, which was not asked for",
                ));
            }
            100..=199 => {}
            _ => break (status, framing),
        }
    };
    let body = match (framing.chunked, framing.length) {
        (true, _) => chunks(reader, &mut budget)?,
        (false, Some(n)) => {
            if n > budget {
                return Err(malformed("the body is over the limit"));
            }
            let mut body = vec![0; n];
            reader.read_exact(&mut body)?;
            body
        }
        (false, None) => {
            // To the end of the connection; one byte past the limit tells a body over it.
            let mut body = Vec::new();
            Read::take(&mut *reader, budget as u64 + 1).read_to_end(&mut body)?;
            if body.len() > budget {
                return Err(malformed("the body is over the limit"));
            }
            body
        }
    };
    Ok((status, body))
}

/// A chunked body.
fn chunks(reader: &mut dyn BufRead, budget: &mut usize) -> Result<Vec<u8>, ReadError> {
    let mut body = Vec::new();
    loop {
        let size_line = line(reader, budget)?;
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| malformed(format!("a chunk size {size_text:?}")))?;
        if size == 0 {
            // Trailers, up to the empty line.
            while !line(reader, budget)?.is_empty() {}
            return Ok(body);
        }
        if size > *budget {
            return Err(malformed("the body is over the limit"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        *budget -= size;
        if !line(reader, budget)?.is_empty() {
            return Err(malformed("a chunk is longer than its size"));
        }
    }
}

#[cfg(test)]
mod tests;
