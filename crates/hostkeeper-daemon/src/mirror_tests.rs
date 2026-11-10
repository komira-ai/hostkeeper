//! The tests of `mirror`: the files of a private repository read from a fake coordinator into a
//! scratch directory, and the removal of the mirrors a pushed set does not keep.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// What the fake coordinator answers to one path.
#[derive(Clone)]
enum Reply {
    Body(Vec<u8>),
    Missing,
    Fails(GetError),
}

/// A coordinator whose answers are a script per path: the n-th request of a path gets the n-th
/// reply, the last one again after that. A body arrives in chunks of seven bytes.
#[derive(Default)]
struct Origin {
    script: Mutex<BTreeMap<String, Vec<Reply>>>,
    requests: Mutex<Vec<(String, Limit)>>,
}

impl Origin {
    fn say(&self, path: &str, replies: Vec<Reply>) {
        self.script
            .lock()
            .expect("script")
            .insert(path.to_owned(), replies);
    }

    fn body(&self, path: &str, bytes: &[u8]) {
        self.say(path, vec![Reply::Body(bytes.to_vec())]);
    }

    fn requests(&self) -> Vec<(String, Limit)> {
        self.requests.lock().expect("requests").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.requests().into_iter().map(|(path, _)| path).collect()
    }
}

impl Fetch for Origin {
    fn get(
        &self,
        path: &str,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, GetError> {
        let asked = {
            let mut requests = self.requests.lock().expect("requests");
            requests.push((path.to_owned(), limit));
            requests.iter().filter(|(seen, _)| seen == path).count()
        };
        let replies = self
            .script
            .lock()
            .expect("script")
            .get(path)
            .cloned()
            .unwrap_or_else(|| vec![Reply::Missing]);
        match &replies[(asked - 1).min(replies.len() - 1)] {
            Reply::Missing => Ok(Got::NotFound),
            Reply::Fails(error) => Err(error.clone()),
            Reply::Body(bytes) => {
                if bytes.len() as u64 > limit.max_bytes {
                    return Err(GetError::TooLong {
                        length: bytes.len() as u64,
                        max: limit.max_bytes,
                    });
                }
                for chunk in bytes.chunks(7) {
                    sink(chunk).map_err(|e| GetError::Sink(e.kind()))?;
                }
                Ok(Got::Body(bytes.len() as u64))
            }
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// An `InRelease` as a clear-signed message lists `files`.
fn release(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut text = "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA512\n\nOrigin: acme\nSuite: web\n\
                    Components: main\nSHA256:\n"
        .to_owned();
    for (path, bytes) in files {
        text.push_str(&format!(" {} {} {path}\n", sha256(bytes), bytes.len()));
    }
    text.push_str("-----BEGIN PGP SIGNATURE-----\n\nabc\n-----END PGP SIGNATURE-----\n");
    text.into_bytes()
}

const RELEASE: &str = "/repositories/acme/dists/web/InRelease";

/// The files the origin lists, of which the host reads `PACKAGES`, `ALL` and `TRANSLATION`.
const PACKAGES: (&str, &[u8]) = ("main/binary-amd64/Packages", b"Package: hello\n");
const ALL: (&str, &[u8]) = ("main/binary-all/Packages.xz", b"all packages");
const TRANSLATION: (&str, &[u8]) = ("main/i18n/Translation-en", b"a greeting");
const OTHER: [(&str, &[u8]); 4] = [
    ("main/binary-arm64/Packages", b"other architecture"),
    ("contrib/binary-amd64/Packages", b"other component"),
    ("main/Contents-amd64", b"contents"),
    ("main/source/Sources", b"sources"),
];

fn listed() -> Vec<(&'static str, &'static [u8])> {
    let mut files = vec![PACKAGES, ALL, TRANSLATION];
    files.extend(OTHER);
    files
}

/// A mirror under a scratch root, reading from the fake coordinator.
struct Bench {
    _scratch: Scratch,
    root: PathBuf,
    origin: Arc<Origin>,
    mirror: CoordinatorMirror,
}

fn bench(name: &str) -> Bench {
    let scratch = scratch(name);
    let root = scratch.path().join("mirror");
    let origin = Arc::new(Origin::default());
    let mirror = CoordinatorMirror::new(root.clone(), Arc::clone(&origin) as Arc<dyn Fetch>);
    Bench {
        _scratch: scratch,
        root,
        origin,
        mirror,
    }
}

impl Bench {
    /// The origin publishes `files` under the suite `web`, and an `InRelease` listing `files`.
    fn publish(&self, files: &[(&str, &[u8])]) {
        self.origin.body(RELEASE, &release(files));
        for (path, bytes) in files {
            self.origin
                .body(&format!("/repositories/acme/dists/web/{path}"), bytes);
        }
    }

    fn index(&self) -> Result<(), MirrorError> {
        self.mirror.index(&acme(&self.root), "amd64")
    }

    fn mirrored(&self) -> PathBuf {
        self.root.join("acme")
    }

    /// Every path below the repository's mirror, relative, a `/` after a directory.
    fn tree(&self) -> Vec<String> {
        fn walk(base: &Path, rel: &Path, out: &mut Vec<String>) {
            let mut items: Vec<_> = fs::read_dir(base.join(rel))
                .expect("a directory")
                .map(|item| item.expect("an item"))
                .collect();
            items.sort_by_key(fs::DirEntry::file_name);
            for item in items {
                let path = rel.join(item.file_name());
                let dir = item.file_type().expect("a type").is_dir();
                out.push(format!("{}{}", path.display(), if dir { "/" } else { "" }));
                if dir {
                    walk(base, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.mirrored(), Path::new(""), &mut out);
        out
    }
}

/// The entry the host was sent for the mirror under `root`: the channel `acme-web` of the
/// repository `acme`, its one suite the host's group `web`.
fn acme(root: &Path) -> Repository {
    Repository {
        name: "acme-web".to_owned(),
        uri: format!("file:{}/acme", root.display()),
        suite: "web".to_owned(),
        components: vec!["main".to_owned()],
        keyring: PathBuf::from("/etc/hostkeeper/repositories/acme.gpg"),
        text: String::new(),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("a file").permissions().mode() & 0o777
}

/// Proves the index of a repository is read file by file into the mirror: `InRelease`, then the
/// listed files under `<component>/binary-<architecture>/`, `binary-all/` and `i18n/` of the
/// entry's component and no others (not another architecture or component, not contents or
/// sources); each is requested with its listed size as the bound and arrives byte for byte in
/// chunks; directories are 0755 and files 0644; and no staging directory is left.
#[test]
fn the_index_files_of_the_hosts_architecture_are_mirrored_byte_for_byte() {
    let bench = bench("mirror-index");
    bench.publish(&listed());
    bench.index().expect("the index is mirrored");

    assert_eq!(
        bench.tree(),
        [
            "dists/",
            "dists/web/",
            "dists/web/InRelease",
            "dists/web/main/",
            "dists/web/main/binary-all/",
            "dists/web/main/binary-all/Packages.xz",
            "dists/web/main/binary-amd64/",
            "dists/web/main/binary-amd64/Packages",
            "dists/web/main/i18n/",
            "dists/web/main/i18n/Translation-en",
        ]
    );
    let dists = bench.mirrored().join("dists/web");
    for (path, bytes) in [PACKAGES, ALL, TRANSLATION] {
        assert_eq!(fs::read(dists.join(path)).expect("a file"), bytes, "{path}");
        assert_eq!(mode(&dists.join(path)), 0o644, "{path}");
    }
    assert_eq!(
        fs::read(dists.join("InRelease")).expect("the release"),
        release(&listed())
    );
    for dir in ["", "dists", "dists/web", "dists/web/main/i18n"] {
        assert_eq!(mode(&bench.mirrored().join(dir)), 0o755, "{dir:?}");
    }
    let requests = bench.origin.requests();
    assert_eq!(
        bench.origin.paths(),
        [
            RELEASE,
            "/repositories/acme/dists/web/main/binary-all/Packages.xz",
            "/repositories/acme/dists/web/main/binary-amd64/Packages",
            "/repositories/acme/dists/web/main/i18n/Translation-en",
        ]
    );
    assert_eq!(requests[0].1.max_bytes, INRELEASE_MAX);
    assert_eq!(requests[1].1.max_bytes, ALL.1.len() as u64);
    assert!(!bench.root.join(".acme.next").exists());
}

/// Proves the new directory takes the old mirror's place whole: a file of the old one (a stale
/// index, a package file) is gone, and a host of another architecture lists only its own files.
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
#[test]
fn a_new_index_replaces_the_old_mirror_whole() {
    let bench = bench("mirror-replace");
    let old = bench.mirrored().join("pool/web");
    fs::create_dir_all(&old).expect("an old pool");
    fs::write(old.join("old.deb"), b"old").expect("an old file");
    bench.publish(&listed());
    bench.index().expect("the index is mirrored");
    assert!(!old.exists());
    assert!(bench.mirrored().join("dists/web/InRelease").is_file());

    bench
        .mirror
        .index(&acme(&bench.root), "arm64")
        .expect("arm64");
    let tree = bench.tree();
    assert!(tree.contains(&"dists/web/main/binary-arm64/Packages".to_owned()));
    assert!(!tree.contains(&"dists/web/main/binary-amd64/Packages".to_owned()));
    assert!(!bench.root.join(".acme.next").exists());
}

/// Proves an `InRelease` the coordinator does not have is `NotPublished` (the group's suite is
/// not published yet), that nothing else is requested, and that the old mirror stays.
#[test]
fn an_unpublished_suite_is_not_published_and_the_old_mirror_stays() {
    let bench = bench("mirror-unpublished");
    fs::create_dir_all(bench.mirrored()).expect("a mirror");
    fs::write(bench.mirrored().join("kept"), b"x").expect("a file");
    let error = bench.index().expect_err("not published");
    assert!(matches!(error, MirrorError::NotPublished(_)), "{error}");
    assert_eq!(bench.origin.paths(), [RELEASE]);
    assert!(bench.mirrored().join("kept").is_file());
    assert!(!bench.root.join(".acme.next").exists());
}

/// Proves a file that is not what `InRelease` lists (other bytes, a shorter body, a longer one,
/// a file that is gone) means the origin published between two reads: everything is read again,
/// `InRelease` first, and the second read is taken.
#[test]
fn a_file_that_is_not_what_the_release_lists_is_read_again_once() {
    let path = "/repositories/acme/dists/web/main/binary-amd64/Packages";
    let wrong = [
        Reply::Body(b"Package: other\n".to_vec()),
        Reply::Body(b"Package".to_vec()),
        Reply::Body(b"Package: hello\nand more bytes".to_vec()),
        Reply::Missing,
    ];
    for (n, first) in wrong.into_iter().enumerate() {
        let bench = bench(&format!("mirror-again-{n}"));
        bench.publish(&listed());
        bench
            .origin
            .say(path, vec![first, Reply::Body(PACKAGES.1.to_vec())]);
        bench.index().expect("the second read is taken");
        let requests = bench.origin.paths();
        assert_eq!(
            requests.iter().filter(|p| *p == RELEASE).count(),
            2,
            "{n}: {requests:?}"
        );
        assert_eq!(
            fs::read(
                bench
                    .mirrored()
                    .join("dists/web/main/binary-amd64/Packages")
            )
            .expect("the file"),
            PACKAGES.1
        );
    }
}

/// Proves a file that is wrong twice is `Unreachable` (`fetch_failed`), says what differed, and
/// leaves the old mirror and no staging directory.
#[test]
fn a_file_that_is_wrong_twice_fails_and_leaves_the_old_mirror() {
    let bench = bench("mirror-twice");
    fs::create_dir_all(bench.mirrored()).expect("a mirror");
    fs::write(bench.mirrored().join("kept"), b"x").expect("a file");
    bench.publish(&listed());
    bench.origin.say(
        "/repositories/acme/dists/web/main/binary-amd64/Packages",
        vec![Reply::Body(b"Package: other\n".to_vec())],
    );
    let error = bench.index().expect_err("changed twice");
    let MirrorError::Unreachable(why) = &error else {
        panic!("{error}");
    };
    assert!(why.contains("twice"), "{why}");
    assert!(why.contains("main/binary-amd64/Packages"), "{why}");
    assert!(bench.mirrored().join("kept").is_file());
    assert!(!bench.root.join(".acme.next").exists());
}

/// Proves a coordinator or origin that cannot be read is `Unreachable` without a second try, for
/// `InRelease` and for a listed file alike (a 502, a connection cut), and a sink that cannot
/// write is `Io`.
#[test]
fn a_failing_coordinator_is_unreachable_and_a_failing_disk_is_io() {
    let file = "/repositories/acme/dists/web/main/i18n/Translation-en";
    for (path, error) in [
        (RELEASE, GetError::Status(502)),
        (file, GetError::Status(502)),
        (file, GetError::Truncated),
        (RELEASE, GetError::NotEnrolled),
    ] {
        let bench = bench("mirror-unreachable");
        bench.publish(&listed());
        bench.origin.say(path, vec![Reply::Fails(error.clone())]);
        let failed = bench.index().expect_err("unreachable");
        assert!(matches!(failed, MirrorError::Unreachable(_)), "{failed}");
        assert_eq!(
            bench
                .origin
                .paths()
                .iter()
                .filter(|p| *p == RELEASE)
                .count(),
            1,
            "{error}: not read again"
        );
        assert!(!bench.root.join(".acme.next").exists());
    }
    for path in [RELEASE, file] {
        let bench = bench("mirror-disk");
        bench.publish(&listed());
        bench.origin.say(
            path,
            vec![Reply::Fails(GetError::Sink(io::ErrorKind::StorageFull))],
        );
        let failed = bench.index().expect_err("io");
        assert!(matches!(failed, MirrorError::Io(_)), "{failed}");
    }
}

/// Catches an `InRelease` the daemon would trust blindly: one over its bound, one with no
/// `SHA256:` field, a line that is not three fields, a size or digest that is not one, a path
/// with `..` or a `%`, and a path listed twice are each `Unreachable`, and no file is requested.
#[test]
fn a_release_that_is_no_list_is_refused() {
    let digest = "ab".repeat(32);
    let too_long = Reply::Fails(GetError::TooLong {
        length: INRELEASE_MAX + 1,
        max: INRELEASE_MAX,
    });
    let texts: Vec<Reply> = [
        "Origin: acme\n".to_owned(),
        format!("SHA256:\n {digest} 12\n"),
        format!("SHA256:\n {digest} twelve main/binary-amd64/Packages\n"),
        "SHA256:\n abc 12 main/binary-amd64/Packages\n".to_owned(),
        format!("SHA256:\n {digest} 12 main/binary-amd64/../Packages\n"),
        format!("SHA256:\n {digest} 12 main/binary-amd64/P%41ckages\n"),
        format!("SHA256:\n {digest} 12 main/binary-amd64//Packages\n"),
        format!(
            "SHA256:\n {digest} 12 main/binary-amd64/Packages\n {digest} 12 main/binary-amd64/Packages\n"
        ),
    ]
    .into_iter()
    .map(|text| Reply::Body(text.into_bytes()))
    .chain([too_long])
    .collect();
    for (n, reply) in texts.into_iter().enumerate() {
        let bench = bench(&format!("mirror-no-list-{n}"));
        bench.origin.say(RELEASE, vec![reply]);
        let error = bench.index().expect_err("refused");
        assert!(matches!(error, MirrorError::Unreachable(_)), "{n}: {error}");
        assert_eq!(bench.origin.paths(), [RELEASE], "{n}");
        assert!(!bench.root.join(".acme.next").exists(), "{n}");
    }
}

/// Proves an `InRelease` that lists nothing for the host (another architecture only) is
/// mirrored as it is: apt, which reads it, says what is missing.
#[test]
fn a_release_with_nothing_for_the_host_is_mirrored_alone() {
    let bench = bench("mirror-nothing");
    bench.publish(&OTHER);
    bench.index().expect("mirrored");
    assert_eq!(
        bench.tree(),
        ["dists/", "dists/web/", "dists/web/InRelease"]
    );
}

/// Proves a mirror root that cannot be made (a file is in its place) is `Io`.
#[test]
fn a_mirror_root_that_is_a_file_is_io() {
    let bench = bench("mirror-root-file");
    fs::write(&bench.root, b"x").expect("a file");
    bench.publish(&listed());
    let error = bench.index().expect_err("io");
    assert!(matches!(error, MirrorError::Io(_)), "{error}");
    let error = bench
        .mirror
        .pool(&acme(&bench.root), "pool/web/a.deb", &sha256(b"x"), 1)
        .expect_err("io");
    assert!(matches!(error, MirrorError::Io(_)), "{error}");
}

/// Proves a mirror root that cannot be made because its parent is a file is an I/O failure of
/// the stage, for the index and for the pool file, and nothing is asked of the coordinator.
#[test]
fn a_mirror_root_under_a_file_is_io() {
    let mut bench = bench("mirror-root-under-file");
    fs::write(&bench.root, b"x").expect("a file where the root's parent is");
    bench.root = bench.root.join("mirror");
    bench.mirror = CoordinatorMirror::new(
        bench.root.clone(),
        Arc::clone(&bench.origin) as Arc<dyn Fetch>,
    );
    bench.publish(&listed());
    let error = bench.index().expect_err("io");
    assert!(matches!(error, MirrorError::Io(_)), "{error}");
    let error = bench
        .mirror
        .pool(&acme(&bench.root), "pool/web/a.deb", &sha256(b"x"), 1)
        .expect_err("io");
    assert!(matches!(error, MirrorError::Io(_)), "{error}");
    assert!(bench.origin.requests().is_empty());
}

const POOL: &str = "/repositories/acme/pool/web/a/hello_1_amd64.deb";
const FILENAME: &str = "pool/web/a/hello_1_amd64.deb";

/// Proves a package file is read into the mirror at the index's `Filename` (directories 0755,
/// the file 0644), requested with the order's size as the bound, with no temporary file left;
/// and a later read of the same file replaces it.
#[test]
fn the_pool_file_is_mirrored_at_its_filename() {
    let bench = bench("mirror-pool");
    let deb = b"!<arch>\nhello".to_vec();
    bench.origin.body(POOL, &deb);
    bench
        .mirror
        .pool(
            &acme(&bench.root),
            FILENAME,
            &sha256(&deb),
            deb.len() as u64,
        )
        .expect("mirrored");
    let dest = bench.mirrored().join(FILENAME);
    assert_eq!(fs::read(&dest).expect("the file"), deb);
    assert_eq!(mode(&dest), 0o644);
    for dir in ["", "pool", "pool/web", "pool/web/a"] {
        assert_eq!(mode(&bench.mirrored().join(dir)), 0o755, "{dir:?}");
    }
    assert_eq!(
        bench.tree(),
        [
            "pool/",
            "pool/web/",
            "pool/web/a/",
            "pool/web/a/hello_1_amd64.deb"
        ]
    );
    assert_eq!(bench.origin.requests()[0].1.max_bytes, deb.len() as u64);

    let newer = b"!<arch>\nhallo".to_vec();
    bench.origin.body(POOL, &newer);
    bench
        .mirror
        .pool(
            &acme(&bench.root),
            FILENAME,
            &sha256(&newer),
            newer.len() as u64,
        )
        .expect("mirrored again");
    assert_eq!(fs::read(&dest).expect("the file"), newer);
}

/// Proves a package file that is not the pinned one never stays in the mirror: other bytes, a
/// shorter body and a longer one are `Digest`, a file the coordinator lacks is `NotPublished`, a
/// coordinator that fails is `Unreachable`, and a disk that fails is `Io`; the file already
/// mirrored is not touched.
#[test]
fn a_pool_file_that_is_not_the_pin_is_refused_and_leaves_nothing() {
    let deb = b"!<arch>\nhello".to_vec();
    let pin = sha256(&deb);
    let cases: Vec<(Reply, &str)> = vec![
        (Reply::Body(b"!<arch>\nother".to_vec()), "digest"),
        (Reply::Body(b"!<arch>".to_vec()), "digest"),
        (Reply::Body(b"!<arch>\nhello and more".to_vec()), "digest"),
        (Reply::Missing, "published"),
        (Reply::Fails(GetError::Status(502)), "unreachable"),
        (
            Reply::Fails(GetError::Sink(io::ErrorKind::StorageFull)),
            "io",
        ),
    ];
    for (n, (reply, kind)) in cases.into_iter().enumerate() {
        let bench = bench(&format!("mirror-pool-refused-{n}"));
        let dest = bench.mirrored().join(FILENAME);
        fs::create_dir_all(dest.parent().expect("a parent")).expect("a pool");
        fs::write(&dest, b"older bytes").expect("an older file");
        bench.origin.say(POOL, vec![reply]);
        let error = bench
            .mirror
            .pool(&acme(&bench.root), FILENAME, &pin, deb.len() as u64)
            .expect_err("refused");
        let got = match error {
            MirrorError::Digest(_) => "digest",
            MirrorError::NotPublished(_) => "published",
            MirrorError::Unreachable(_) => "unreachable",
            MirrorError::Io(_) => "io",
        };
        assert_eq!(got, kind, "{n}");
        assert_eq!(fs::read(&dest).expect("the file"), b"older bytes", "{n}");
        assert_eq!(
            bench.tree(),
            [
                "pool/",
                "pool/web/",
                "pool/web/a/",
                "pool/web/a/hello_1_amd64.deb"
            ],
            "{n}: no temporary file"
        );
    }
}

/// Catches a `Filename` that leaves the suite's pool: another suite's, a relative path with `..`,
/// an empty segment, a `%`, a path not under `pool/` is `Unreachable`, and nothing is requested.
#[test]
fn a_filename_outside_the_suites_pool_is_refused() {
    let bench = bench("mirror-filename");
    for filename in [
        "pool/dev/a.deb",
        "pool/web/../dev/a.deb",
        "pool/web//a.deb",
        "pool/web/a%41.deb",
        "dists/web/InRelease",
        "/pool/web/a.deb",
        "pool/web/",
        &format!("pool/web/{}.deb", "a".repeat(1012)),
        "pool/web/a b.deb",
    ] {
        let error = bench
            .mirror
            .pool(&acme(&bench.root), filename, &sha256(b"x"), 1)
            .expect_err("refused");
        assert!(
            matches!(error, MirrorError::Unreachable(_)),
            "{filename}: {error}"
        );
    }
    assert!(bench.origin.requests().is_empty());
}

fn private(name: &str, suites: &[&str]) -> Entry {
    Entry {
        name: name.to_owned(),
        kind: "apt".to_owned(),
        uri: format!("https://{name}.example.com/debian"),
        suites: suites.iter().map(|s| (*s).to_owned()).collect(),
        access: "hosts".to_owned(),
        credential_file: "/etc/hostkeeper/credential".to_owned(),
        ..Default::default()
    }
}

/// Proves a pushed set keeps the mirrors it names and removes the rest: a mirror whose name is
/// not a private entry of the set (gone, or now public), one whose `dists/` holds another suite
/// than the entry's (the host moved group), the staging directory of an entry the set does not
/// keep and a stray file go; a mirror of the entry's own suite, one with no `dists/` yet, and the
/// staging directory of a kept entry (a fill in progress) stay with their files; no mirror root
/// is no error.
#[test]
fn a_set_keeps_the_mirrors_it_names_and_removes_the_rest() {
    let scratch = scratch("mirror-prune");
    let root = scratch.path().join("mirror");
    let make = |path: &str| {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("a parent")).expect("directories");
        fs::write(full, b"x").expect("a file");
    };
    for path in [
        "acme/dists/web/InRelease",
        "acme/pool/web/a.deb",
        "gone/dists/web/InRelease",
        "public/dists/web/InRelease",
        "moved/dists/dev/InRelease",
        "both/dists/web/InRelease",
        "both/dists/dev/InRelease",
        "fresh/pool/web/a.deb",
        ".acme.next/dists/web/InRelease",
        ".gone.next/dists/web/InRelease",
        ".acme.nex/dists/web/InRelease",
        "stray",
    ] {
        make(path);
    }
    let mut public = private("public", &["web"]);
    public.access = String::new();
    let entries = [
        private("acme", &["web"]),
        public,
        private("moved", &["web"]),
        private("both", &["web"]),
        private("fresh", &["web"]),
    ];
    prune(&root, &entries).expect("pruned");
    let mut left: Vec<String> = fs::read_dir(&root)
        .expect("the root")
        .map(|item| {
            item.expect("an item")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    left.sort();
    assert_eq!(left, [".acme.next", "acme", "fresh"]);
    assert!(root.join("acme/pool/web/a.deb").is_file());
    assert!(root.join("fresh/pool/web/a.deb").is_file());

    prune(&scratch.path().join("absent"), &entries).expect("no root is no error");

    // A mirror that cannot be removed (its directory holds one no one may write) is the error,
    // and the rest of the root is still pruned.
    make("held/dists/web/sub/InRelease");
    make("other/dists/web/InRelease");
    let locked = root.join("held/dists/web");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).expect("the mode");
    let failed = prune(&root, &entries);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("the mode back");
    failed.expect_err("a mirror that cannot be removed");
    assert!(!root.join("other").exists(), "the rest is pruned");
    assert!(root.join("held").exists());
    fs::write(scratch.path().join("file"), b"x").expect("a file");
    prune(&scratch.path().join("file"), &entries).expect_err("a root that is a file");
}

/// Proves the coordinator wiring is made for a config whose coordinator can be dialed and not
/// for one with a path, and that the silent log says nothing.
#[test]
fn the_coordinator_wiring_needs_an_address_that_can_be_dialed() {
    let config = |url: &str| {
        DaemonConfig::parse(&format!(
            "coordinator = \"{url}\"\nca_sha256 = \"{}\"\nrelease_channel = \"https://releases.test\"\n\n\
             [trusted_signers]\norganisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
             refs = [\"refs/tags/*\"]\nhosted_runner = true\n",
            "ab".repeat(32)
        ))
        .expect("a config")
    };
    let clock: Arc<dyn Clock> = Arc::new(hostkeeper_link::testing::FakeClock::new(0, 0));
    let pki = Path::new("/nonexistent");
    assert!(
        over_coordinator(
            &config("https://coordinator.test:8443"),
            pki,
            Arc::clone(&clock)
        )
        .is_some()
    );
    assert!(over_coordinator(&config("https://coordinator.test:8443/x"), pki, clock).is_none());
    Quiet.line("nothing");
}

/// Proves a mirror prints as its root alone.
#[test]
fn a_mirror_debugs_as_its_root() {
    let bench = bench("mirror-debug");
    assert_eq!(
        format!("{:?}", bench.mirror),
        format!("CoordinatorMirror {{ root: {} }}", bench.root.display())
    );
}

/// Catches an entry the mirror would write outside its root: one whose URI is not a directory of
/// the mirror (another place, or a path below a directory of it) is `Io`, and nothing is requested.
#[test]
fn an_entry_that_is_not_read_from_the_mirror_is_refused() {
    let bench = bench("mirror-foreign");
    for uri in [
        "https://origin.test/debian".to_owned(),
        "file:/elsewhere/acme".to_owned(),
        format!("file:{}/", bench.root.display()),
        format!("file:{}/acme/deeper", bench.root.display()),
    ] {
        let entry = Repository {
            uri: uri.clone(),
            ..acme(&bench.root)
        };
        let error = bench.mirror.index(&entry, "amd64").expect_err("refused");
        assert!(matches!(error, MirrorError::Io(_)), "{uri}: {error}");
        let error = bench
            .mirror
            .pool(&entry, FILENAME, &sha256(b"x"), 1)
            .expect_err("refused");
        assert!(matches!(error, MirrorError::Io(_)), "{uri}: {error}");
    }
    assert!(bench.origin.requests().is_empty());
}
