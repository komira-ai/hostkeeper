//! The static files of `--ui-dir` on the private surface: what is served, with which headers,
//! what is never served (a path that leaves the directory, a name or size or kind not
//! served), and that the private surface's routes, its tokens and its lack of cross-origin
//! access are what they were.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hyper::HeaderMap;

use super::*;
use crate::ui::{CONTENT_SECURITY_POLICY, MAX_FILE_BYTES, Open, PREFIX, UiDir, UiDirError};

const INDEX: &str = "<!doctype html><title>operator</title>";
const SUB_INDEX: &str = "<!doctype html><title>sub</title>";
const SENTINEL: &str = "the sentinel outside the directory";
const NOT_FOUND: &str = r#"{"error":"not_found"}"#;
const HTML: &str = "text/html; charset=utf-8";

/// Each file of the directory with an extension of the table, its text and its content type.
const TYPED: [(&str, &str, &str); 7] = [
    ("page.html", "<p>page</p>", HTML),
    ("app.css", "body{margin:0}", "text/css; charset=utf-8"),
    (
        "app.js",
        "export const x = 1;",
        "text/javascript; charset=utf-8",
    ),
    ("data.json", "{\"a\":1}", "application/json"),
    ("logo.svg", "<svg/>", "image/svg+xml"),
    ("logo.png", "png", "image/png"),
    ("favicon.ico", "ico", "image/x-icon"),
];

/// The paths a file opened through the port of a test's directory was opened at, in order.
#[derive(Clone, Default)]
struct Opened(Arc<Mutex<Vec<PathBuf>>>);

impl Opened {
    /// A port that notes the path, then opens it.
    fn port(&self) -> Open {
        let log = self.clone();
        Arc::new(move |path: &Path| {
            log.0.lock().expect("lock").push(path.to_owned());
            std::fs::File::open(path)
        })
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.0.lock().expect("lock").clone()
    }
}

/// A rig whose coordinator serves `<scratch>/ui`: `index.html`, the files of [`TYPED`] and
/// `sub/index.html`, with `<scratch>/secret.html` and `<scratch>/secret` beside the directory.
struct Site {
    rig: Rig,
    ui: PathBuf,
    opened: Opened,
}

fn site(name: &str) -> Site {
    let mut rig = Rig::new(name);
    let ui = rig.dir.path().join("ui");
    fs::create_dir_all(ui.join("sub")).expect("the directory");
    fs::write(ui.join("index.html"), INDEX).expect("written");
    fs::write(ui.join("sub/index.html"), SUB_INDEX).expect("written");
    for (file, text, _) in TYPED {
        fs::write(ui.join(file), text).expect("written");
    }
    fs::write(rig.dir.path().join("secret.html"), SENTINEL).expect("written");
    fs::write(rig.dir.path().join("secret"), SENTINEL).expect("written");
    let opened = Opened::default();
    let dir = UiDir::with_open(&ui, opened.port()).expect("a directory");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_ui(Some(dir));
    Site { rig, ui, opened }
}

/// The answer to `request` on `surface` from `peer`: its status, headers and body.
async fn ask(
    rig: &Rig,
    surface: Surface,
    peer: &Peer,
    request: Request<Full<Bytes>>,
) -> (StatusCode, HeaderMap, Bytes) {
    let response = handle(&rig.coordinator, surface, peer, request, PATIENCE).await;
    let (parts, body) = response.into_parts();
    let body = body.collect().await.expect("a body").to_bytes();
    (parts.status, parts.headers, body)
}

/// `method path` on the private surface, with no token.
async fn on_private(rig: &Rig, method: &str, path: &str) -> (StatusCode, HeaderMap, Bytes) {
    ask(
        rig,
        Surface::Control,
        &None,
        request(method, path, None, ""),
    )
    .await
}

/// A request of `method` for `path` with the headers `headers` and no body.
fn with_headers(method: &str, path: &str, headers: &[(&str, &str)]) -> Request<Full<Bytes>> {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Full::new(Bytes::new())).expect("a request")
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The four headers every answer of the file server carries.
fn assert_guarded(headers: &HeaderMap, what: &str) {
    for (name, want) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", CONTENT_SECURITY_POLICY),
    ] {
        assert_eq!(header(headers, name), Some(want), "{what}: {name}");
    }
}

/// No `Access-Control-*` header.
fn assert_no_cors(headers: &HeaderMap, what: &str) {
    let cors: Vec<&str> = headers
        .keys()
        .map(hyper::header::HeaderName::as_str)
        .filter(|name| name.starts_with("access-control-"))
        .collect();
    assert!(cors.is_empty(), "{what}: {cors:?}");
}

/// The routes of [`private_routes.txt`](private_routes.txt), one `METHOD path` each.
fn committed_routes() -> BTreeSet<String> {
    include_str!("private_routes.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// A path of each committed route: `/ui/*` asked as `/ui/`.
fn committed_paths() -> Vec<String> {
    committed_routes()
        .iter()
        .filter_map(|route| route.split_once(' '))
        .map(|(_, path)| path.replace('*', ""))
        .collect()
}

const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"];

/// Paths a UI-specific route, a session or a second way in would be likely to take, beside
/// the committed ones.
const PROBES: [&str; 24] = [
    "/",
    "/index.html",
    "/ui",
    "/ui/index.html",
    "/ui/sub/",
    "/ui/v1/status",
    "/ui/login",
    "/ui/session",
    "/ui/config.json",
    "/ui/api",
    "/v1",
    "/v1/",
    "/v1/ui",
    "/v1/ui/",
    "/v1/login",
    "/v1/logout",
    "/v1/session",
    "/v1/token",
    "/v1/whoami",
    "/v1/status/",
    "/login",
    "/session",
    "/static/app.js",
    HEARTBEAT_PATH,
];

/// Every `METHOD path` of [`METHODS`] over the committed paths and [`PROBES`] that `rig`
/// answers on `surface` with an operator token as anything but not-found or
/// method-not-allowed; a path under `/ui/` is noted as `/ui/*`.
async fn answered(rig: &Rig, surface: Surface, peer: &Peer) -> BTreeSet<String> {
    let mut paths = committed_paths();
    paths.extend(PROBES.iter().map(|path| (*path).to_owned()));
    let mut found = BTreeSet::new();
    for method in METHODS {
        for path in &paths {
            let asked = request(method, path, Some(OPERATOR), "");
            let (code, _, _) = ask(rig, surface, peer, asked).await;
            if code != StatusCode::NOT_FOUND && code != StatusCode::METHOD_NOT_ALLOWED {
                let route = if path.starts_with(PREFIX) {
                    "/ui/*"
                } else {
                    path.as_str()
                };
                found.insert(format!("{method} {route}"));
            }
        }
    }
    found
}

/// Catches: the file server always on: with no `--ui-dir`, `GET` and `HEAD` of `/ui/` and of a
/// file below it are not found, by the router (no header of the file server), even though a
/// directory of files sits in the coordinator's directory.
#[tokio::test]
async fn ui_dir_unset_serves_nothing() {
    let rig = Rig::new("ui-unset");
    let ui = rig.dir.path().join("ui");
    fs::create_dir(&ui).expect("the directory");
    fs::write(ui.join("index.html"), INDEX).expect("written");
    assert!(rig.coordinator.ui().is_none());
    for method in ["GET", "HEAD"] {
        for path in ["/ui/", "/ui/index.html"] {
            let (code, headers, body) = on_private(&rig, method, path).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(body, NOT_FOUND, "{method} {path}");
            assert_eq!(header(&headers, "x-content-type-options"), None);
        }
    }
}

/// Catches: a security header missing, the wrong content type, or a body on `HEAD`: `GET /ui/`
/// (and with a query) is `index.html` as HTML with the four headers and its length; `HEAD /ui/`
/// has the same headers and length and no body; each extension of the table is served with
/// its type; `/ui/sub/` is that directory's `index.html`; an unknown extension, no extension,
/// an upper-case one and a missing file are not found, with the four headers; another method
/// is not allowed, with the four headers.
#[tokio::test]
async fn ui_files_are_served_with_security_headers() {
    let site = site("ui-served");
    let rig = &site.rig;
    for path in ["/ui/", "/ui/?from=link", "/ui/index.html"] {
        let (code, headers, body) = on_private(rig, "GET", path).await;
        assert_eq!(code, StatusCode::OK, "{path}");
        assert_eq!(body, INDEX, "{path}");
        assert_eq!(header(&headers, "content-type"), Some(HTML), "{path}");
        let length = INDEX.len().to_string();
        assert_eq!(header(&headers, "content-length"), Some(length.as_str()));
        assert_guarded(&headers, path);
    }
    let (code, headers, body) = on_private(rig, "HEAD", "/ui/").await;
    assert_eq!(code, StatusCode::OK);
    assert!(body.is_empty(), "HEAD has no body: {body:?}");
    assert_eq!(header(&headers, "content-type"), Some(HTML));
    let length = INDEX.len().to_string();
    assert_eq!(header(&headers, "content-length"), Some(length.as_str()));
    assert_guarded(&headers, "HEAD /ui/");
    for (file, text, kind) in TYPED {
        let path = format!("/ui/{file}");
        let (code, headers, body) = on_private(rig, "GET", &path).await;
        assert_eq!(code, StatusCode::OK, "{path}");
        assert_eq!(body, text, "{path}");
        assert_eq!(header(&headers, "content-type"), Some(kind), "{path}");
        assert_guarded(&headers, &path);
    }
    for path in ["/ui/sub/", "/ui/sub/index.html"] {
        let (code, _, body) = on_private(rig, "GET", path).await;
        assert_eq!(code, StatusCode::OK, "{path}");
        assert_eq!(body, SUB_INDEX, "{path}");
    }
    for file in [
        "notes.txt",
        "README",
        "archive.tar.gz",
        "PAGE2.HTML",
        "other.JS",
    ] {
        fs::write(site.ui.join(file), "not served").expect("written");
    }
    for path in [
        "/ui/notes.txt",
        "/ui/README",
        "/ui/archive.tar.gz",
        "/ui/PAGE2.HTML",
        "/ui/other.JS",
        "/ui/missing.html",
        "/ui/sub/missing.js",
    ] {
        for method in ["GET", "HEAD"] {
            let (code, headers, body) = on_private(rig, method, path).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(header(&headers, "content-type"), Some("application/json"));
            assert_guarded(&headers, path);
            let want = if method == "GET" { NOT_FOUND } else { "" };
            assert_eq!(body, want, "{method} {path}");
        }
    }
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let (code, headers, body) = on_private(rig, method, "/ui/").await;
        assert_eq!(code, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(body, r#"{"error":"method_not_allowed"}"#);
        assert_guarded(&headers, method);
    }
}

/// Catches: a name the table, the decoding or the size cap should refuse being served: a
/// hidden file, a directory with a served extension, `%` not followed by two hex digits, a
/// name that decodes to bytes that are not UTF-8, to a NUL or to a `/`, and a file one byte
/// over 4 MiB are all not found; a file of exactly 4 MiB and a name spelled with `%61` are
/// served.
#[tokio::test]
async fn ui_names_sizes_and_kinds_that_are_not_served_are_not_found() {
    let site = site("ui-boundaries");
    let rig = &site.rig;
    fs::write(site.ui.join(".hidden.js"), "hidden").expect("written");
    fs::create_dir(site.ui.join("dir.html")).expect("a directory");
    let cap = usize::try_from(MAX_FILE_BYTES).expect("fits");
    fs::write(site.ui.join("exact.png"), vec![7u8; cap]).expect("written");
    fs::write(site.ui.join("over.png"), vec![7u8; cap + 1]).expect("written");
    for path in [
        "/ui/.hidden.js",
        "/ui/%2ehidden.js",
        "/ui/dir.html",
        "/ui/dir.html/",
        "/ui/%zzapp.js",
        "/ui/app%2.js",
        "/ui/app.js%",
        "/ui/%ff.js",
        "/ui/app%00.js",
        "/ui/sub%2findex.html",
        "/ui/over.png",
    ] {
        let (code, headers, body) = on_private(rig, "GET", path).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, NOT_FOUND, "{path}");
        assert_guarded(&headers, path);
    }
    let (code, _, body) = on_private(rig, "GET", "/ui/exact.png").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body.len(), cap);
    let (code, _, body) = on_private(rig, "GET", "/ui/%61pp.js").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body, "export const x = 1;");
}

/// Catches: a path joined to the directory unchecked: `..` plain, percent-encoded in either
/// case and with a backslash, an absolute path after `//`, `..` after a real directory, and a
/// symbolic link (to a file or to a directory) that leads out are all not found, by `GET` and
/// `HEAD`; the file outside is never opened (the directory's port notes every open and has
/// none), and none of its bytes are answered. A symbolic link that stays inside is served, and
/// then the port has noted that one open.
#[tokio::test]
async fn ui_path_traversal_is_refused() {
    let site = site("ui-traversal");
    let rig = &site.rig;
    let outside = rig.dir.path().join("outside");
    fs::create_dir(&outside).expect("a directory");
    fs::write(outside.join("secret.html"), SENTINEL).expect("written");
    symlink(
        rig.dir.path().join("secret.html"),
        site.ui.join("leak.html"),
    )
    .expect("a link");
    symlink("../outside", site.ui.join("out")).expect("a link");
    symlink("app.js", site.ui.join("alias.js")).expect("a link");
    let absolute = format!("/ui/{}", rig.dir.path().join("secret.html").display());
    let paths = [
        "/ui/../secret",
        "/ui/%2e%2e/secret",
        "/ui/..%5csecret",
        "/ui//etc/x",
        "/ui/../secret.html",
        "/ui/%2e%2e/secret.html",
        "/ui/%2E%2E/secret.html",
        "/ui/..%5csecret.html",
        "/ui/..%5c..%5csecret.html",
        "/ui/%2e%2e%2fsecret.html",
        "/ui/./../secret.html",
        "/ui/sub/../../secret.html",
        "/ui/sub/%2e%2e/%2e%2e/secret.html",
        absolute.as_str(),
        "/ui/leak.html",
        "/ui/out/secret.html",
    ];
    for path in paths {
        for method in ["GET", "HEAD"] {
            let (code, headers, body) = on_private(rig, method, path).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{method} {path}");
            assert_guarded(&headers, path);
            let want = if method == "GET" { NOT_FOUND } else { "" };
            assert_eq!(body, want, "{method} {path}");
        }
    }
    assert_eq!(site.opened.paths(), Vec::<PathBuf>::new());
    let (code, _, body) = on_private(rig, "GET", "/ui/alias.js").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body, "export const x = 1;");
    let root = fs::canonicalize(&site.ui).expect("the directory");
    assert_eq!(site.opened.paths(), [root.join("app.js")]);
}

/// Catches: the file server registered on both listeners: on the heartbeat surface `/ui/` and
/// a file below it are not found by the router, with or without an updater's certificate, and
/// nothing is opened; the same requests on the private surface are served.
#[tokio::test]
async fn ui_is_not_served_on_the_heartbeat_listener() {
    let site = site("ui-heartbeat");
    let rig = &site.rig;
    for who in [None, updater("h1", "storage")] {
        for method in ["GET", "HEAD"] {
            for path in ["/ui/", "/ui/app.js"] {
                let asked = request(method, path, None, "");
                let (code, headers, _) = ask(rig, Surface::Heartbeat, &who, asked).await;
                assert_eq!(code, StatusCode::NOT_FOUND, "{method} {path}");
                assert_eq!(header(&headers, "x-content-type-options"), None);
            }
        }
    }
    assert_eq!(site.opened.paths(), Vec::<PathBuf>::new());
    for path in ["/ui/", "/ui/app.js"] {
        let (code, _, _) = on_private(rig, "GET", path).await;
        assert_eq!(code, StatusCode::OK, "{path}");
    }
}

/// Catches: cross-origin access added for convenience: a file, a file server error, a router
/// error and every committed route of the private surface (with no token and with the
/// operator's, with an `Origin` and without), and the heartbeat route, answer with no
/// `Access-Control-*` header; an `OPTIONS` preflight with an `Origin` to any of those paths on
/// either surface is not found or not allowed, again with no such header.
#[tokio::test]
async fn no_route_sets_a_cors_header_and_an_options_request_is_refused() {
    let site = site("ui-cors");
    let rig = &site.rig;
    let origin = ("origin", "https://elsewhere.example");
    let mut asked: Vec<(String, String)> = committed_routes()
        .iter()
        .filter_map(|route| route.split_once(' '))
        .map(|(method, path)| (method.to_owned(), path.replace('*', "")))
        .collect();
    for (method, path) in [
        ("GET", "/ui/app.js"),
        ("GET", "/ui/missing.html"),
        ("POST", "/ui/"),
        ("GET", "/nowhere"),
        ("DELETE", STATUS_PATH),
    ] {
        asked.push((method.to_owned(), path.to_owned()));
    }
    for (method, path) in &asked {
        for bearer in [None, Some(format!("Bearer {OPERATOR}"))] {
            let mut headers = vec![origin];
            headers.extend(bearer.as_deref().map(|value| ("authorization", value)));
            for given in [&headers[..], &headers[1..]] {
                let request = with_headers(method, path, given);
                let (_, answer, _) = ask(rig, Surface::Control, &None, request).await;
                assert_no_cors(&answer, &format!("{method} {path}"));
            }
        }
    }
    let beat = request("POST", HEARTBEAT_PATH, None, BEAT);
    let (code, answer, _) = ask(rig, Surface::Heartbeat, &None, beat).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_no_cors(&answer, "the heartbeat");
    let preflight = [
        origin,
        ("access-control-request-method", "POST"),
        ("access-control-request-headers", "authorization"),
    ];
    let mut paths: Vec<String> = asked.into_iter().map(|(_, path)| path).collect();
    paths.extend(["/".to_owned(), "/ui/index.html".to_owned()]);
    paths.push(HEARTBEAT_PATH.to_owned());
    for surface in [Surface::Control, Surface::Heartbeat] {
        for path in &paths {
            let request = with_headers("OPTIONS", path, &preflight);
            let (code, answer, _) = ask(rig, surface, &None, request).await;
            assert!(
                matches!(code, StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED),
                "OPTIONS {path} on {surface:?}: {code}"
            );
            assert_no_cors(&answer, &format!("OPTIONS {path}"));
        }
    }
}

/// Catches: the file server authenticating, or the API reachable through `/ui/`: a file is the
/// same with no `Authorization`, a wrong token, another scheme and the operator's token; the
/// status route is still refused without a token and with a wrong one; and `/ui/` followed by
/// an API path, plainly or after `..`, is the file server's not-found, never the API's answer.
#[tokio::test]
async fn the_static_handler_needs_no_token_and_reads_none() {
    let site = site("ui-tokens");
    let rig = &site.rig;
    let operator = format!("Bearer {OPERATOR}");
    for given in [
        &[][..],
        &[("authorization", "Bearer not-the-operator-token")][..],
        &[("authorization", "Basic b3BlcmF0b3I6eA==")][..],
        &[("authorization", operator.as_str())][..],
    ] {
        let (code, headers, body) = ask(
            rig,
            Surface::Control,
            &None,
            with_headers("GET", "/ui/app.js", given),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{given:?}");
        assert_eq!(body, "export const x = 1;", "{given:?}");
        assert_guarded(&headers, "a file");
    }
    for bearer in [None, Some("not-the-operator-token")] {
        let (code, body) = call(
            rig,
            Surface::Control,
            &None,
            request("GET", STATUS_PATH, bearer, ""),
        )
        .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{bearer:?}");
        assert_eq!(body, json!({"error": "unauthorized"}));
    }
    for (method, path) in [
        ("GET", "/ui/v1/status"),
        ("GET", "/ui/../v1/status"),
        ("GET", "/ui/%2e%2e/v1/status"),
        ("POST", "/ui/v1/operator"),
        ("POST", "/ui/../v1/operator"),
    ] {
        let asked = request(method, path, Some(OPERATOR), r#"{"action":"exclude"}"#);
        let (code, headers, _) = ask(rig, Surface::Control, &None, asked).await;
        assert!(
            matches!(code, StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED),
            "{method} {path}: {code}"
        );
        assert_guarded(&headers, path);
    }
    assert_eq!(site.opened.paths().len(), 4);
}

/// Catches: a UI-specific route (a login, a session, a config, a second way to the API): every
/// method over the committed paths and the probe paths, asked on the private surface of a
/// coordinator serving a directory, is answered exactly on the routes of
/// `private_routes.txt`; without `--ui-dir` it is those routes less `/ui/*`; and the heartbeat
/// surface answers `POST /v1/heartbeat` alone.
#[tokio::test]
async fn the_private_route_set_is_unchanged() {
    let site = site("ui-routes");
    let committed = committed_routes();
    assert_eq!(
        answered(&site.rig, Surface::Control, &None).await,
        committed
    );
    let plain = Rig::new("ui-routes-plain");
    let without: BTreeSet<String> = committed
        .iter()
        .filter(|route| !route.ends_with("/ui/*"))
        .cloned()
        .collect();
    assert_eq!(answered(&plain, Surface::Control, &None).await, without);
    assert_eq!(
        answered(&site.rig, Surface::Heartbeat, &None).await,
        BTreeSet::from([format!("POST {HEARTBEAT_PATH}")])
    );
}

/// Catches: a `--ui-dir` that is not a directory accepted, or a directory served by another
/// opener than the file system: a missing path and a file are refused with the path in the
/// text, a directory's root is its real path, and a coordinator over it serves `/ui/` from
/// the files on disk.
#[tokio::test]
async fn a_ui_dir_is_a_directory_resolved_once_and_read_from_disk() {
    let rig = Rig::new("ui-dir");
    let missing = rig.dir.path().join("missing");
    let why = UiDir::new(&missing).err().expect("refused");
    assert!(matches!(why, UiDirError::Unreadable { .. }), "{why}");
    assert!(
        why.to_string()
            .starts_with(&format!("{}: ", missing.display())),
        "{why}"
    );
    let file = rig.dir.path().join("index.html");
    fs::write(&file, INDEX).expect("written");
    let why = UiDir::new(&file).err().expect("refused");
    assert!(matches!(why, UiDirError::NotADirectory(_)), "{why}");
    assert_eq!(
        why.to_string(),
        format!("{} is not a directory", file.display())
    );
    let ui = rig.dir.path().join("ui");
    fs::create_dir(&ui).expect("the directory");
    fs::write(ui.join("index.html"), INDEX).expect("written");
    symlink(&ui, rig.dir.path().join("linked")).expect("a link");
    let dir = UiDir::new(&rig.dir.path().join("linked")).expect("a directory");
    assert_eq!(dir.root(), fs::canonicalize(&ui).expect("the directory"));
    let served = Rig::start(&rig.dir, &rig.clock).with_ui(Some(dir));
    let asked = request("GET", "/ui/", None, "");
    let response = handle(&served, Surface::Control, &None, asked, PATIENCE).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    assert_eq!(body, INDEX);
}
