//! What the tests of `PUT /v1/xcode/<sha256>` share: a coordinator with an Xcode store in a
//! directory of its own, the requests they send and the request bodies they send with, and what
//! they read back of the store's directory, the audit and the log.

use std::collections::VecDeque;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{HeaderMap, Request, StatusCode};
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, SanType};
use serde_json::{Value, json};

use crate::audit_actions::XCODE_PUT;
use crate::audit_file::Appender;
use crate::grpc::route;
use crate::http::{BoxError, Peer, Surface};
use crate::ops::AuditEntry;
use crate::service::Coordinator;
use crate::testutil::{Log, OPERATOR, Rig};
use crate::xcode_feed::{Channel, KnownReleases, XcodeId, XcodeRelease, XcodeVersion};
use crate::xcode_store::{Digesting, XcodeStore};

/// How long a body has in the tests that are not about patience.
pub(super) const PATIENCE: Duration = Duration::from_secs(30);

/// The bytes the good pushes send.
pub(super) const ARCHIVE: &[u8] = b"the bytes of an Xcode archive";

/// Other bytes, another archive.
pub(super) const OTHER: &[u8] = b"the bytes of another Xcode archive";

/// Sixteen bytes: the cap of the tests of the cap.
pub(super) const SIXTEEN: &[u8] = b"0123456789abcdef";

/// A store cap no test of the other behaviour comes near.
pub(super) const ROOMY: u64 = 1 << 20;

/// The SHA-256 of `bytes`, as the path of a push names it.
pub(super) fn digest_of(bytes: &[u8]) -> String {
    let mut digest = Digesting::default();
    digest.feed(bytes);
    digest.finish()
}

/// The Xcode `version` of `build`.
pub(super) fn id(version: &str, build: &str) -> XcodeId {
    XcodeId {
        version: XcodeVersion::parse(version).expect("a version in a test"),
        build: build.to_owned(),
    }
}

/// A release of `channel` requiring macOS 26.6.
pub(super) fn release(version: &str, build: &str, channel: Channel) -> XcodeRelease {
    XcodeRelease {
        id: id(version, build),
        channel,
        requires_macos: "26.6".to_owned(),
    }
}

/// The list the coordinator has read: 27.2 (27B52) is the newest release, 27.1 is older, and 28.0
/// is a beta.
pub(super) fn feed() -> Vec<XcodeRelease> {
    vec![
        release("27.2", "27B52", Channel::Release),
        release("27.1", "27B40", Channel::Release),
        release("28.0", "28A5", Channel::Beta),
    ]
}

/// A list the coordinator cannot read.
pub(super) struct Unreadable;

impl KnownReleases for Unreadable {
    fn keep(&self, _: &[XcodeRelease]) -> Result<(), String> {
        Ok(())
    }

    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        Err("the list cannot be read".to_owned())
    }
}

/// An audit that takes no line.
pub(super) fn refusing() -> Appender {
    Box::new(|_| Err(io::Error::from(io::ErrorKind::StorageFull)))
}

/// What a request body gives next.
pub(super) enum Piece {
    /// A data frame of these bytes.
    Data(Vec<u8>),
    /// A trailers frame.
    Trailers,
    /// An error: the client went away.
    Fail,
}

/// A request body of the pieces it is given, then its end or, when `stalls` is set, nothing more
/// for ever. `at_end` runs once, when the pieces are all given and the body would end.
#[derive(Default)]
pub(super) struct Chunks {
    pub(super) pieces: VecDeque<Piece>,
    pub(super) stalls: bool,
    pub(super) at_end: Option<Box<dyn FnOnce() + Send>>,
    /// Set when the body is asked for a frame.
    pub(super) polled: Arc<AtomicBool>,
}

impl Body for Chunks {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        this.polled.store(true, Ordering::SeqCst);
        match this.pieces.pop_front() {
            Some(Piece::Data(bytes)) => Poll::Ready(Some(Ok(Frame::data(Bytes::from(bytes))))),
            Some(Piece::Trailers) => Poll::Ready(Some(Ok(Frame::trailers(HeaderMap::new())))),
            Some(Piece::Fail) => Poll::Ready(Some(Err(io::Error::other("the client went away")))),
            None if this.stalls => Poll::Pending,
            None => {
                if let Some(end) = this.at_end.take() {
                    end();
                }
                Poll::Ready(None)
            }
        }
    }
}

/// A body of the data frames `chunks`.
pub(super) fn data(chunks: &[&[u8]]) -> Chunks {
    Chunks {
        pieces: chunks
            .iter()
            .map(|chunk| Piece::Data(chunk.to_vec()))
            .collect(),
        ..Chunks::default()
    }
}

/// A client certificate with the subjectAltNames `sans` (`dns:` names, else URIs).
fn peer(sans: &[&str]) -> Peer {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = sans
        .iter()
        .map(|san| match san.strip_prefix("dns:") {
            Some(name) => SanType::DnsName(name.try_into().expect("a name")),
            None => SanType::URI((*san).try_into().expect("a uri")),
        })
        .collect();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("key");
    Some(Arc::from(
        params.self_signed(&key).expect("sign").der().to_vec(),
    ))
}

/// The certificate of the farm's updater `d1`.
pub(super) fn updater() -> Peer {
    peer(&["urn:kbf:role:updater", "urn:kbf:host-role:daemon", "dns:d1"])
}

/// The value of the `Authorization` header that presents `token`.
pub(super) fn bearer(token: &str) -> Option<String> {
    Some(format!("Bearer {token}"))
}

/// A request of `method` for `uri`, with the header `authorization` when there is one.
pub(super) fn builder(
    method: &str,
    uri: &str,
    authorization: Option<String>,
) -> hyper::http::request::Builder {
    let builder = Request::builder().method(method).uri(uri);
    match authorization {
        Some(value) => builder.header("authorization", value),
        None => builder,
    }
}

/// A request of `method` for `uri` with `body`.
pub(super) fn request<B>(
    method: &str,
    uri: &str,
    authorization: Option<String>,
    body: B,
) -> Request<B> {
    builder(method, uri, authorization)
        .body(body)
        .expect("a request")
}

/// The path and query of a push of the archive `sha256` that is Xcode `version` of `build`.
pub(super) fn uri(sha256: &str, version: &str, build: &str) -> String {
    format!("/v1/xcode/{sha256}?version={version}&build={build}")
}

/// The path and query of a push of `bytes` as Xcode 27.2 (27B52).
pub(super) fn good_uri(bytes: &[u8]) -> String {
    uri(&digest_of(bytes), "27.2", "27B52")
}

/// The operator's push of `bytes` as Xcode 27.2 (27B52).
pub(super) fn push(bytes: &'static [u8]) -> Request<Full<Bytes>> {
    request(
        "PUT",
        &good_uri(bytes),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(bytes)),
    )
}

/// The names in `dir`, sorted.
pub(super) fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("the store's directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The two files of the archive `sha256`, sorted.
pub(super) fn files_of(sha256: &str) -> Vec<String> {
    vec![format!("{sha256}.json"), format!("{sha256}.xip")]
}

/// The lines of `log`, without their time.
pub(super) fn lines(log: &Log) -> Vec<String> {
    log.text()
        .lines()
        .map(|line| line.split_once(' ').expect("a time").1.to_owned())
        .collect()
}

/// The future `future` polled once.
pub(super) async fn polled<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx))).await
}

/// A coordinator with an Xcode store in a directory of its own, the list [`feed`] read, and what
/// it logs.
pub(super) struct Farm {
    /// Ends the capture of the log: it is dropped before the rig's.
    _guard: tracing::subscriber::DefaultGuard,
    pub(super) log: Log,
    pub(super) coordinator: Arc<Coordinator>,
    pub(super) store: Arc<XcodeStore>,
    /// The store's directory.
    pub(super) dir: PathBuf,
    _rig: Rig,
}

/// A farm named `test` whose store takes archives of at most `max_bytes`, over the coordinator
/// `build` makes of the rig's.
pub(super) fn farm_with(
    test: &str,
    max_bytes: u64,
    build: impl FnOnce(Coordinator, &Scratch) -> Coordinator,
) -> Farm {
    let rig = Rig::new(test);
    let log = Log::default();
    let guard = log.install();
    let dir = rig.dir.path().join("xcode");
    fs::create_dir(&dir).expect("the store's directory");
    let store = Arc::new(XcodeStore::open(&dir, max_bytes).expect("a store"));
    let coordinator =
        build(Rig::start(&rig.dir, &rig.clock), &rig.dir).with_xcode_store(Arc::clone(&store));
    coordinator
        .xcode
        .known
        .keep(&feed())
        .expect("the list is kept");
    Farm {
        _guard: guard,
        log,
        coordinator: Arc::new(coordinator),
        store,
        dir,
        _rig: rig,
    }
}

/// A farm named `test` with a store that takes [`ROOMY`] archives.
pub(super) fn farm(test: &str) -> Farm {
    farm_with(test, ROOMY, |coordinator, _| coordinator)
}

impl Farm {
    /// The answer of the private surface to `request` from `peer`, read as JSON, when a body
    /// has `patience` for each frame.
    pub(super) async fn send_within<B>(
        &self,
        peer: &Peer,
        request: Request<B>,
        patience: Duration,
    ) -> (StatusCode, Value)
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError> + Send + 'static,
    {
        let response = route(&self.coordinator, Surface::Control, peer, request, patience).await;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("a response body").to_bytes();
        (
            parts.status,
            serde_json::from_slice(&bytes).expect("a JSON answer"),
        )
    }

    /// The answer to `request` from no certificate.
    pub(super) async fn send<B>(&self, request: Request<B>) -> (StatusCode, Value)
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError> + Send + 'static,
    {
        self.send_within(&None, request, PATIENCE).await
    }

    /// The `xcode_put` entries of the audit.
    pub(super) fn puts(&self) -> Vec<AuditEntry> {
        self.coordinator
            .ops
            .audit_after(0)
            .expect("the audit")
            .into_iter()
            .filter(|entry| entry.action == XCODE_PUT)
            .collect()
    }

    /// `request` is answered `status` with the error `code`, and nothing of it is left: the
    /// store's directory is empty and the audit holds no upload.
    pub(super) async fn refused<B>(
        &self,
        what: &str,
        request: Request<B>,
        status: StatusCode,
        code: &str,
    ) where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError> + Send + 'static,
    {
        let answered = self.send(request).await;
        assert_eq!(answered, (status, json!({ "error": code })), "{what}");
        self.assert_untouched(what);
    }

    /// The store's directory is empty and the audit holds no upload.
    pub(super) fn assert_untouched(&self, what: &str) {
        assert_eq!(listing(&self.dir), Vec::<String>::new(), "{what}");
        assert_eq!(self.puts(), Vec::new(), "{what}");
    }
}
