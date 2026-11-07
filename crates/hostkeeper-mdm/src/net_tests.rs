//! The download client over an in-memory connection: no socket, no name lookup.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

use super::{Download, DownloadError, MAX_REDIRECTS as LIMIT, Net};

/// The redirect limit as a count.
const MAX_REDIRECTS: usize = LIMIT as usize;

/// The production client over the in-memory server: HTTPS only.
fn strict() -> Net {
    Net::with_parts(true, &[], Server, NoLookup)
}

const HTTPS: &str = "https://downloads.example.com";

/// Answers every name with one documentation address; nothing is looked up.
#[derive(Debug)]
struct NoLookup;

impl Resolver for NoLookup {
    fn resolve(
        &self,
        _uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = self.empty();
        addrs.push(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 80)));
        Ok(addrs)
    }
}

/// Opens an [`InMemory`] connection for every request.
#[derive(Debug)]
struct Server;

impl Connector for Server {
    type Out = InMemory;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<InMemory>, ureq::Error> {
        let c = details.config;
        Ok(Some(InMemory {
            buffers: LazyBuffers::new(c.input_buffer_size(), c.output_buffer_size()),
            request: Vec::new(),
            reply: Vec::new(),
            sent: 0,
        }))
    }
}

/// One connection: collects the request and, once its head is complete, serves the answer
/// for its path.
struct InMemory {
    buffers: LazyBuffers,
    request: Vec<u8>,
    reply: Vec<u8>,
    sent: usize,
}

impl fmt::Debug for InMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InMemory")
    }
}

/// A raw reply: the status line and headers (each ending `\r\n`) then the body bytes, as the
/// server writes them, whatever its framing.
fn reply(head: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {head}Connection: close\r\n\r\n").into_bytes();
    out.extend_from_slice(body);
    out
}

/// A reply with a correct `Content-Length`.
fn sized(status: &str, extra: &str, body: &[u8]) -> Vec<u8> {
    reply(
        &format!("{status}\r\n{extra}Content-Length: {}\r\n", body.len()),
        body,
    )
}

/// A reply in chunks of `size` bytes, ended by the zero-length chunk when `finished`.
fn chunked(body: &[u8], size: usize, finished: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for part in body.chunks(size) {
        out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.extend_from_slice(b"\r\n");
    }
    if finished {
        out.extend_from_slice(b"0\r\n\r\n");
    }
    reply("200 OK\r\nTransfer-Encoding: chunked\r\n", &out)
}

/// A redirect to `location`.
fn redirect(location: &str) -> Vec<u8> {
    sized("302 Found", &format!("Location: {location}\r\n"), b"")
}

/// The answer to `request` (a whole request head), by path: `/ok` is 200 with `hello`;
/// `/redirect` is a 302 to `/ok`; `/hop/N` is a 302 to `/hop/N-1`, and `/hop/0` is a 302 to
/// `/ok`; `/a` and `/b` redirect to each other; `/downgrade` is a 302 to the plain-http
/// `/ok`; `/big` is 200 with 1000 bytes; `/size/N` is 200 with N bytes under a
/// `Content-Length`, and `/chunked/N` the same in chunks of 7; `/cut` promises 1000 bytes
/// and sends 500; `/chunked-cut` ends without its last chunk; anything else is 404.
fn answer(request: &[u8]) -> Vec<u8> {
    let head = String::from_utf8_lossy(request);
    let path = head.split(' ').nth(1).unwrap_or_default();
    let number = |prefix: &str| {
        path.strip_prefix(prefix)
            .and_then(|n| n.parse::<usize>().ok())
    };
    match (path, number("/hop/"), number("/size/"), number("/chunked/")) {
        ("/ok", ..) => sized("200 OK", "", b"hello"),
        ("/redirect", ..) => redirect("/ok"),
        (_, Some(0), ..) => redirect("/ok"),
        (_, Some(n), ..) => redirect(&format!("/hop/{}", n - 1)),
        ("/a", ..) => redirect("/b"),
        ("/b", ..) => redirect("/a"),
        ("/downgrade", ..) => redirect("http://downloads.example.com/ok"),
        ("/big", ..) => sized("200 OK", "", &[b'x'; 1000]),
        (_, _, Some(n), _) => sized("200 OK", "", &vec![b'x'; n]),
        (_, _, _, Some(n)) => chunked(&vec![b'x'; n], 7, true),
        ("/cut", ..) => reply("200 OK\r\nContent-Length: 1000\r\n", &[b'x'; 500]),
        ("/chunked-cut", ..) => chunked(&[b'x'; 30], 10, false),
        _ => sized("404 Not Found", "", b"nope"),
    }
}

impl Transport for InMemory {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, _timeout: NextTimeout) -> Result<(), ureq::Error> {
        let sent = self.buffers.output()[..amount].to_vec();
        self.request.extend_from_slice(&sent);
        if self.request.ends_with(b"\r\n\r\n") {
            self.reply = answer(&self.request);
        }
        Ok(())
    }

    fn await_input(&mut self, _timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let room = self.buffers.input_append_buf();
        let n = room.len().min(self.reply.len() - self.sent);
        room[..n].copy_from_slice(&self.reply[self.sent..self.sent + n]);
        self.sent += n;
        self.buffers.input_appended(n);
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        true
    }

    fn is_tls(&self) -> bool {
        true
    }
}

/// Catches a client that treats an error status as a body, ignores the size limit, does not
/// follow a redirect (release assets are served from one), or dials plain http when it must
/// not; each failure comes back as its own kind of [`DownloadError`].
#[test]
fn downloads_follow_redirects_and_refuse_errors_big_bodies_and_plain_http() {
    let net = Net::with_parts(false, &[], Server, NoLookup);
    let base = "http://downloads.example.com";
    assert_eq!(net.get(&format!("{base}/ok"), 100).unwrap(), b"hello");
    assert_eq!(net.get(&format!("{base}/redirect"), 100).unwrap(), b"hello");
    let missing = net.get(&format!("{base}/missing"), 100).unwrap_err();
    assert!(matches!(missing, DownloadError::Status(404)), "{missing:?}");
    assert_eq!(missing.to_string(), "answered 404");
    let big = net.get(&format!("{base}/big"), 10).unwrap_err();
    assert!(
        matches!(
            big,
            DownloadError::Transport(ureq::Error::BodyExceedsLimit(10))
        ),
        "{big:?}"
    );
    assert_eq!(net.get(&format!("{base}/big"), 2000).unwrap().len(), 1000);

    let strict = Net::with_parts(true, &[], Server, NoLookup);
    let plain = strict.get(&format!("{base}/ok"), 100).unwrap_err();
    assert!(
        matches!(
            plain,
            DownloadError::Transport(ureq::Error::RequireHttpsOnly(_))
        ),
        "{plain:?}"
    );
}

/// Catches `Net::new` building something other than a working client over the network's own
/// connector: refused before any connection, a plain-http URL fails the same way.
#[test]
fn the_network_client_refuses_plain_http() {
    let err = Net::new(true)
        .get("http://downloads.example.com/ok", 100)
        .unwrap_err();
    assert!(
        matches!(
            err,
            DownloadError::Transport(ureq::Error::RequireHttpsOnly(_))
        ),
        "{err:?}"
    );
}

/// Catches a client that follows an HTTPS answer's redirect to plain http: the hop is
/// refused as an HTTPS-only violation and `/ok` over http is never asked for.
#[test]
fn a_redirect_from_https_to_http_is_refused() {
    let err = strict()
        .get(&format!("{HTTPS}/downgrade"), 100)
        .unwrap_err();
    assert!(
        matches!(err, DownloadError::Transport(ureq::Error::RequireHttpsOnly(ref u))
            if u == "http://downloads.example.com/ok"),
        "{err:?}"
    );
}

/// Catches a client that only serves plain http in tests: an `https://` URL is served in
/// memory under the production HTTPS-only setting, and its redirect to another https path
/// is followed.
#[test]
fn https_urls_are_served_and_their_redirects_followed_when_https_only() {
    let net = strict();
    assert_eq!(net.get(&format!("{HTTPS}/ok"), 100).unwrap(), b"hello");
    assert_eq!(
        net.get(&format!("{HTTPS}/redirect"), 100).unwrap(),
        b"hello"
    );
}

/// Catches a redirect loop that never ends or that returns a 3xx as the download:
/// `/a -> /b -> /a` ends in `TooManyRedirects`.
#[test]
fn a_redirect_cycle_is_an_error() {
    let err = strict().get(&format!("{HTTPS}/a"), 100).unwrap_err();
    assert!(
        matches!(err, DownloadError::Transport(ureq::Error::TooManyRedirects)),
        "{err:?}"
    );
}

/// Catches a redirect limit that is off by one or that hands back the last 3xx: a chain of
/// exactly `MAX_REDIRECTS` redirects is followed to its body, one more is `TooManyRedirects`.
#[test]
fn a_chain_of_exactly_the_redirect_limit_is_followed_and_one_more_is_refused() {
    let net = strict();
    // `/hop/N` makes N + 1 redirects: N hops, then the one to `/ok`.
    let last = MAX_REDIRECTS - 1;
    assert_eq!(
        net.get(&format!("{HTTPS}/hop/{last}"), 100).unwrap(),
        b"hello"
    );
    let err = net
        .get(&format!("{HTTPS}/hop/{}", last + 1), 100)
        .unwrap_err();
    assert!(
        matches!(err, DownloadError::Transport(ureq::Error::TooManyRedirects)),
        "{err:?}"
    );
}

/// Catches a size check that is off by one, for both framings: a body of exactly the limit
/// is returned whole and one byte more is `BodyExceedsLimit`.
#[test]
fn a_body_of_exactly_the_limit_is_accepted_and_one_byte_more_is_refused() {
    let net = strict();
    for kind in ["size", "chunked"] {
        let got = net.get(&format!("{HTTPS}/{kind}/100"), 100).unwrap();
        assert_eq!(got, vec![b'x'; 100], "{kind}");
        let err = net.get(&format!("{HTTPS}/{kind}/101"), 100).unwrap_err();
        assert!(
            matches!(
                err,
                DownloadError::Transport(ureq::Error::BodyExceedsLimit(100))
            ),
            "{kind}: {err:?}"
        );
    }
}

/// Catches a client that returns the part of a body that arrived before the connection
/// closed: 500 of 1000 promised bytes, and a chunked body without its last chunk, are errors.
#[test]
fn an_interrupted_body_is_an_error_not_a_short_download() {
    let net = strict();
    for path in ["cut", "chunked-cut"] {
        let err = net.get(&format!("{HTTPS}/{path}"), 2000).unwrap_err();
        assert!(
            matches!(err, DownloadError::Transport(ureq::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof),
            "{path}: {err:?}"
        );
    }
}
