//! The HTTP server: `GET /readyz` and `GET /version` on loopback, one connection at a time.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// The most bytes of a request head the server reads; a longer one is answered with 400.
pub const MAX_HEAD: usize = 8 * 1024;

/// How long the server waits on one client before it answers with what it has.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// What the server answers with.
pub struct Answer<'a> {
    /// The body of `GET /version`.
    pub version: &'a str,
    /// Answer `GET /readyz` with 503: the server that is up but not ready.
    pub fail_ready: bool,
}

/// The full HTTP response bytes for one request head (pure; no I/O).
pub fn answer(head: &[u8], answer: &Answer<'_>) -> Vec<u8> {
    let end = find_end(head).unwrap_or(head.len());
    let request = if end > MAX_HEAD {
        None
    } else {
        request_line(&head[..end])
    };
    let Some((method, target)) = request else {
        return response(400, "Bad Request", "bad request\n");
    };
    let path = target.split('?').next().unwrap_or(target);
    match (method, path) {
        ("GET", "/readyz") if answer.fail_ready => {
            response(503, "Service Unavailable", "not ready\n")
        }
        ("GET", "/readyz") => response(200, "OK", "ready\n"),
        ("GET", "/version") => response(200, "OK", &format!("{}\n", answer.version)),
        ("GET", _) => response(404, "Not Found", "not found\n"),
        _ => response(405, "Method Not Allowed", "method not allowed\n"),
    }
}

/// The offset of the blank line that ends the head, if the head has one.
fn find_end(head: &[u8]) -> Option<usize> {
    head.windows(4).position(|window| window == b"\r\n\r\n")
}

/// The method and the target of the request line, when it is `<METHOD> /<target> HTTP/1.x`.
fn request_line(head: &[u8]) -> Option<(&str, &str)> {
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    let line = std::str::from_utf8(line.strip_suffix(b"\r").unwrap_or(line)).ok()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    let well_formed = parts.next().is_none()
        && !method.is_empty()
        && method.bytes().all(|b| b.is_ascii_uppercase())
        && target.starts_with('/')
        && matches!(version, "HTTP/1.0" | "HTTP/1.1");
    well_formed.then_some((method, target))
}

fn response(status: u16, reason: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Accepts and answers connections on `listener` until accept fails.
pub fn serve(listener: TcpListener, answer: &Answer<'_>) -> io::Result<()> {
    Err(serve_forever(listener, answer))
}

/// [`serve`] without the `Ok` that it never returns: the error that stopped it.
pub fn serve_forever(listener: TcpListener, answer: &Answer<'_>) -> io::Error {
    loop {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, answer),
            Err(why) => return why,
        }
    }
}

/// Reads one request head and writes its answer. A client that fails mid-way is dropped.
fn handle(mut stream: TcpStream, answer: &Answer<'_>) {
    // A stream accepted from a non-blocking listener may be non-blocking itself (on the BSDs).
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let head = read_head(&mut stream);
    let _ = stream.write_all(&self::answer(&head, answer));
}

/// Reads until the blank line that ends the head, or past [`MAX_HEAD`], or the client stops.
fn read_head(stream: &mut impl Read) -> Vec<u8> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while head.len() <= MAX_HEAD && find_end(&head).is_none() {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
        }
    }
    head
}

#[cfg(test)]
mod tests;
