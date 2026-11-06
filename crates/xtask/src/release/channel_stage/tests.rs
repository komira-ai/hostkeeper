use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

use clap::Parser;
use hostkeeper_config::daemon::{DaemonConfig, Sha256Hex};
use hostkeeper_package::fetch::{FetchEnv, PackageOrder, fetch_package};
use hostkeeper_package::ports::{
    Attest, Attested, Fetcher, Got, Limit, NetError, Sink, Url, Verifier, VerifyError,
};
use hostkeeper_package::prove::{ProveEnv, prove};
use hostkeeper_steps::{BootId, BootIdError, Clock};

use super::*;
use crate::release::Verb;
use crate::release::fakes::{FakeHistory, Fakes, scratch};
use crate::release::ports::Adapters;
use crate::testdir::TestDir;

const SERVICE: &str = "tierstore";
const VERSION: &str = "1.5.0";
/// The daemon config's `release_channel` in the fetch tests; the in-process origin serves `--out`
/// under it.
const CHANNEL: &str = "https://releases.example/channel";
/// The archive the committed fixture bundle of hostkeeper-package attests, and that bundle: one
/// the daemon's proof reads (a certificate with a source ref, a provenance with one commit, a
/// subject with this archive's SHA-256). It is not signed; the verifier here is a fake.
const ARCHIVE: &[u8] = b"the archive the fixture bundles attest\n";
const ATTESTED_BUNDLE: &[u8] =
    include_bytes!("../../../../hostkeeper-package/tests/fixtures/good.sigstore.json");

fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

fn sha256(bytes: &[u8]) -> String {
    Measure::of(bytes).sha256
}

fn package_name(s: &str, v: &str, platform: Platform) -> String {
    format!("{s}-{v}-{platform}.tar.zst")
}

fn bundle_name(s: &str, v: &str, platform: Platform) -> String {
    format!("{}.sigstore.json", package_name(s, v, platform))
}

/// An unsigned bundle whose statement lists `digests` as its subjects' SHA-256.
fn bundle_listing(digests: &[&str]) -> Vec<u8> {
    let subjects: Vec<Value> = digests
        .iter()
        .map(|d| serde_json::json!({"name": "package", "digest": {"sha256": d}}))
        .collect();
    let statement = serde_json::json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": subjects,
    });
    serde_json::json!({
        "dsseEnvelope": {
            "payload": STANDARD.encode(statement.to_string()),
            "payloadType": "application/vnd.in-toto+json",
        }
    })
    .to_string()
    .into_bytes()
}

/// The canonical index of `packages` of `s` at `v`, written at `dir/index.json`.
fn write_index(dir: &Path, s: &str, v: &str, packages: &[(Platform, &[u8])]) {
    let entries = packages
        .iter()
        .map(|(platform, bytes)| PackageEntry {
            platform: *platform,
            sha256: Sha256Hex::parse(&sha256(bytes)).expect("a SHA-256"),
            bytes: bytes.len() as u64,
        })
        .collect();
    let index = ReleaseIndex::new(service(s), version(v), entries).expect("an index");
    std::fs::write(dir.join("index.json"), index.to_json()).expect("write the index");
}

/// The release of `s` at `v` in `dir` as its assets arrive: each package, a bundle beside it that
/// lists its SHA-256, and the index of them.
fn write_release(dir: &Path, s: &str, v: &str, packages: &[(Platform, &[u8])]) {
    for (platform, bytes) in packages {
        std::fs::write(dir.join(package_name(s, v, *platform)), bytes).expect("write a package");
        std::fs::write(
            dir.join(bundle_name(s, v, *platform)),
            bundle_listing(&[&sha256(bytes)]),
        )
        .expect("write a bundle");
    }
    write_index(dir, s, v, packages);
}

/// Three packages of different bytes, one per platform.
fn three() -> [(Platform, &'static [u8]); 3] {
    [
        (Platform::LinuxArm64, b"the linux-arm64 package"),
        (Platform::LinuxX86_64, b"the linux-x86_64 package"),
        (Platform::MacosArm64, b"the macos-arm64 package"),
    ]
}

/// A scratch directory holding `from/` (made) and `out/` (not made).
struct Dirs {
    _root: TestDir,
    from: PathBuf,
    out: PathBuf,
}

fn dirs(name: &str) -> Dirs {
    let root = scratch(name);
    let from = root.join("from");
    std::fs::create_dir(&from).expect("make from/");
    let out = root.join("out");
    Dirs {
        _root: root,
        from,
        out,
    }
}

fn request_of(dirs: &Dirs, s: &str, v: &str) -> Request {
    Request {
        service: service(s),
        version: version(v),
        from: dirs.from.clone(),
        index: None,
        require: Platform::ALL.to_vec(),
        out: dirs.out.clone(),
    }
}

fn request(dirs: &Dirs) -> Request {
    request_of(dirs, SERVICE, VERSION)
}

/// Every file under `dir`, by its path from `dir`, with its bytes and inode.
fn tree(dir: &Path) -> BTreeMap<String, (Vec<u8>, u64)> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in std::fs::read_dir(&at).expect("read a directory") {
            let path = entry.expect("a directory entry").path();
            let meta = std::fs::symlink_metadata(&path).expect("read an entry");
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            let rel = path.strip_prefix(dir).expect("below the root");
            let bytes = std::fs::read(&path).expect("read a file");
            files.insert(rel.display().to_string(), (bytes, meta.ino()));
        }
    }
    files
}

/// The files of `tree`, without their inodes.
fn bytes_of(tree: &BTreeMap<String, (Vec<u8>, u64)>) -> BTreeMap<String, Vec<u8>> {
    tree.iter()
        .map(|(rel, (bytes, _))| (rel.clone(), bytes.clone()))
        .collect()
}

/// The tree `--out` must hold for the release of `s` at `v` in `from` on `platforms`: each package
/// and bundle, and the index, byte for byte.
fn expected(from: &Path, s: &str, v: &str, platforms: &[Platform]) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let read = |name: &str| std::fs::read(from.join(name)).expect("read an asset");
    for platform in platforms {
        for name in [package_name(s, v, *platform), bundle_name(s, v, *platform)] {
            files.insert(format!("{s}/{v}/{name}"), read(&name));
        }
    }
    files.insert(format!("{s}/{v}/index.json"), read("index.json"));
    files
}

/// The lines of a stage that writes or leaves (`word`) every file of `platforms`.
fn lines(out: &Path, s: &str, v: &str, platforms: &[Platform], word: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for platform in platforms {
        lines.push(format!("{word} {s}/{v}/{}", package_name(s, v, *platform)));
        lines.push(format!("{word} {s}/{v}/{}", bundle_name(s, v, *platform)));
    }
    lines.push(format!("{word} {s}/{v}/index.json"));
    let names: Vec<&str> = platforms.iter().map(|p| p.as_str()).collect();
    let files = 2 * platforms.len() + 1;
    let written = if word == "staged" { files } else { 0 };
    lines.push(format!(
        "{}: the index and the packages of {} with their bundles; {written} of {files} files \
         written",
        out.join(s).join(v).display(),
        names.join(", ")
    ));
    lines
}

/// An origin in process: serves the file at `<root>/<path>` for `<CHANNEL>/<path>`, taking the
/// path as it is (no decoding), as a static file origin maps a URL path to a file. Anything else
/// is a 404. It records each path asked for.
struct Origin {
    root: PathBuf,
    asked: RefCell<Vec<String>>,
}

impl Fetcher for Origin {
    fn get(&self, url: &Url, sink: &mut dyn Sink, _limit: Limit) -> Result<Got, NetError> {
        let prefix = format!("{CHANNEL}/");
        let path = url
            .as_str()
            .strip_prefix(&prefix)
            .ok_or(NetError::Status(404))?;
        self.asked.borrow_mut().push(path.to_owned());
        let bytes = std::fs::read(self.root.join(path)).map_err(|_| NetError::Status(404))?;
        let sent = sink
            .begin(Some(bytes.len() as u64))
            .and_then(|()| bytes.chunks(7).try_for_each(|chunk| sink.chunk(chunk)));
        Ok(sent.map_or(Got::Stopped, |()| Got::Complete))
    }
}

/// A verifier that confirms what the bundle names; the signature is not what these tests prove.
struct Confirms;

impl Verifier for Confirms {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        Ok(Attested {
            commit: request.commit.clone(),
            source_ref: request.source_ref.clone(),
        })
    }
}

/// A clock that never moves.
struct Still;

impl Clock for Still {
    fn mono_secs(&self) -> u64 {
        100
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the still clock", "boot")
    }

    fn sleep_secs(&self, _secs: u64) {}
}

/// A daemon config whose release channel is [`CHANNEL`], read by the daemon's own loader.
fn daemon_config() -> DaemonConfig {
    let text = format!(
        "coordinator = \"https://coordinator.example:8443\"\n\
         ca_sha256 = \"{}\"\n\
         release_channel = \"{CHANNEL}\"\n\
         [trusted_signers]\n\
         organisation = \"example-org\"\n\
         workflow = \".github/workflows/artifacts.yml\"\n\
         refs = [\"refs/heads/main\"]\n\
         hosted_runner = true\n",
        "a".repeat(64)
    );
    DaemonConfig::parse(&text).expect("a daemon config")
}

/// Runs the daemon's fetch and proof of `s` at `v` on a host of `platform`, with `out` served as
/// the channel: the verified archive's bytes and the paths the daemon asked the origin for.
fn fetched(out: &Path, s: &str, v: &str, platform: Platform) -> (Vec<u8>, Vec<String>) {
    let config = daemon_config();
    let origin = Origin {
        root: out.to_path_buf(),
        asked: RefCell::new(Vec::new()),
    };
    let packages = scratch("channel-stage-packages");
    let host = hostkeeper_package::Platform::of_name(platform.as_str())
        .expect("the fetch's platform of the same name");
    let env = FetchEnv {
        fetcher: &origin,
        clock: &Still,
        bounds: &config.bounds,
        release_channel: &config.release_channel,
        packages_root: &packages,
        platform: host,
    };
    let order = PackageOrder {
        service: service(s),
        version: version(v),
    };
    let staged = fetch_package(&env, &order).expect("the daemon's fetch takes the staged tree");
    let proof = ProveEnv {
        fetcher: &origin,
        verifier: &Confirms,
        clock: &Still,
        bounds: &config.bounds,
        signers: &config.trusted_signers,
    };
    let verified = prove(&proof, staged).expect("the daemon's proof takes the staged bundle");
    assert_eq!(verified.sha256().as_str(), sha256(ARCHIVE));
    let bytes = std::fs::read(verified.path()).expect("read the verified archive");
    (bytes, origin.asked.into_inner())
}

/// The release of `s` at `v` in `from` with [`ARCHIVE`] for every platform and the attested
/// fixture bundle beside each.
fn write_attested_release(from: &Path, s: &str, v: &str) {
    let packages = Platform::ALL.map(|platform| (platform, ARCHIVE));
    write_release(from, s, v, &packages);
    for platform in Platform::ALL {
        std::fs::write(from.join(bundle_name(s, v, platform)), ATTESTED_BUNDLE)
            .expect("write the attested bundle");
    }
}

#[test]
fn a_staged_release_is_fetched_and_proved_by_the_daemons_own_code_on_every_platform() {
    // Catches: a tree laid out other than the daemon requests it (another directory, file name,
    // or index name), an index the daemon's strict parser refuses, a bundle not where the proof
    // asks for it, or temporary files left in the served tree.
    let d = dirs("channel-stage-fetch");
    write_attested_release(&d.from, SERVICE, VERSION);
    let staged = stage(&request(&d)).expect("stage");
    assert_eq!(
        staged,
        lines(&d.out, SERVICE, VERSION, &Platform::ALL, "staged")
    );
    assert_eq!(
        bytes_of(&tree(&d.out)),
        expected(&d.from, SERVICE, VERSION, &Platform::ALL)
    );
    for platform in Platform::ALL {
        let (bytes, asked) = fetched(&d.out, SERVICE, VERSION, platform);
        assert_eq!(bytes, ARCHIVE, "{platform}");
        let package = package_name(SERVICE, VERSION, platform);
        assert_eq!(
            asked,
            [
                format!("{SERVICE}/{VERSION}/index.json"),
                format!("{SERVICE}/{VERSION}/{package}"),
                format!("{SERVICE}/{VERSION}/{package}.sigstore.json"),
            ]
        );
    }
}

#[test]
fn a_version_with_a_plus_is_staged_at_the_path_the_fetch_requests() {
    // Catches: a `+` escaped, decoded or dropped on one side only, so the daemon asks for a path
    // the staged tree does not have.
    let d = dirs("channel-stage-plus");
    let v = "1.4.0+g1a2b3c";
    write_attested_release(&d.from, SERVICE, v);
    let staged = stage(&request_of(&d, SERVICE, v)).expect("stage");
    assert_eq!(staged, lines(&d.out, SERVICE, v, &Platform::ALL, "staged"));
    assert!(d.out.join("tierstore/1.4.0+g1a2b3c/index.json").is_file());
    let (bytes, asked) = fetched(&d.out, SERVICE, v, Platform::LinuxX86_64);
    assert_eq!(bytes, ARCHIVE);
    assert_eq!(
        asked,
        [
            "tierstore/1.4.0+g1a2b3c/index.json",
            "tierstore/1.4.0+g1a2b3c/tierstore-1.4.0+g1a2b3c-linux-x86_64.tar.zst",
            "tierstore/1.4.0+g1a2b3c/tierstore-1.4.0+g1a2b3c-linux-x86_64.tar.zst.sigstore.json",
        ]
    );
}

#[test]
fn a_tampered_package_is_refused_before_anything_is_written() {
    // Catches: a package copied without its size and SHA-256 checked against the index, or the
    // check made after the first files were written.
    let d = dirs("channel-stage-tampered");
    write_release(&d.from, SERVICE, VERSION, &three());
    let x86 = package_name(SERVICE, VERSION, Platform::LinuxX86_64);
    let mac = package_name(SERVICE, VERSION, Platform::MacosArm64);
    // The same length with one byte changed, and one byte more.
    std::fs::write(d.from.join(&x86), b"the linux-x86_64 packagE").expect("tamper");
    std::fs::write(d.from.join(&mac), b"the macos-arm64 package!").expect("tamper");
    let refused = stage(&request(&d));
    assert_eq!(
        refused,
        Err(Error::Refused(vec![
            format!(
                "{x86}: package_digest_mismatch: 24 bytes with SHA-256 {}, but the index lists \
                 24 bytes with SHA-256 {}",
                sha256(b"the linux-x86_64 packagE"),
                sha256(b"the linux-x86_64 package")
            ),
            format!(
                "{mac}: package_digest_mismatch: 24 bytes with SHA-256 {}, but the index lists \
                 23 bytes with SHA-256 {}",
                sha256(b"the macos-arm64 package!"),
                sha256(b"the macos-arm64 package")
            ),
        ]))
    );
    assert!(!d.out.exists());
}

#[test]
fn a_package_without_its_bundle_is_refused_before_anything_is_written() {
    // Catches: a package staged without the bundle the daemon's proof downloads beside it.
    let d = dirs("channel-stage-no-bundle");
    write_release(&d.from, SERVICE, VERSION, &three());
    let bundle = bundle_name(SERVICE, VERSION, Platform::LinuxX86_64);
    std::fs::remove_file(d.from.join(&bundle)).expect("remove the bundle");
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Refused(vec![format!(
            "{bundle}: missing: every package is staged with its attestation bundle"
        )]))
    );
    assert!(!d.out.exists());
}

#[test]
fn staging_the_same_release_again_writes_nothing() {
    // Catches: a rerun that rewrites or replaces the files (new inodes), or refuses its own tree.
    let d = dirs("channel-stage-again");
    write_release(&d.from, SERVICE, VERSION, &three());
    stage(&request(&d)).expect("stage");
    let before = tree(&d.out);
    let again = stage(&request(&d)).expect("stage again");
    assert_eq!(
        again,
        lines(&d.out, SERVICE, VERSION, &Platform::ALL, "unchanged")
    );
    assert_eq!(tree(&d.out), before);
}

#[test]
fn a_different_file_at_a_staged_path_is_refused_and_the_tree_is_left_alone() {
    // Catches: a staged release changed in place (a rebuilt package under the same version), or
    // a refusal that writes the files that did not conflict.
    let d = dirs("channel-stage-conflict");
    write_release(&d.from, SERVICE, VERSION, &three());
    stage(&request(&d)).expect("stage");
    let before = tree(&d.out);
    let mut rebuilt = three();
    rebuilt[1].1 = b"a rebuilt linux-x86_64 package";
    write_release(&d.from, SERVICE, VERSION, &rebuilt);
    let at = |name: &str| d.out.join(SERVICE).join(VERSION).join(name);
    let why = "already staged with other bytes; a release in the channel never changes";
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Refused(vec![
            format!(
                "{}: {why}",
                at(&package_name(SERVICE, VERSION, Platform::LinuxX86_64)).display()
            ),
            format!(
                "{}: {why}",
                at(&bundle_name(SERVICE, VERSION, Platform::LinuxX86_64)).display()
            ),
            format!("{}: {why}", at("index.json").display()),
        ]))
    );
    assert_eq!(tree(&d.out), before);
}

#[test]
fn a_partly_staged_release_is_completed() {
    // Catches: a rerun that stops at the first file already there, so a stage cut short is never
    // finished.
    let d = dirs("channel-stage-partial");
    write_release(&d.from, SERVICE, VERSION, &three());
    stage(&request(&d)).expect("stage");
    let bundle = bundle_name(SERVICE, VERSION, Platform::MacosArm64);
    std::fs::remove_file(d.out.join(SERVICE).join(VERSION).join(&bundle)).expect("remove");
    let mut wanted = lines(&d.out, SERVICE, VERSION, &Platform::ALL, "unchanged");
    wanted[5] = format!("staged {SERVICE}/{VERSION}/{bundle}");
    wanted[7] = wanted[7].replace("; 0 of 7", "; 1 of 7");
    assert_eq!(stage(&request(&d)), Ok(wanted));
    assert_eq!(
        bytes_of(&tree(&d.out)),
        expected(&d.from, SERVICE, VERSION, &Platform::ALL)
    );
}

#[test]
fn a_release_without_a_required_platform_is_refused_unless_the_flag_narrows_it() {
    // Catches: a release staged without its macos-arm64 package, which leaves every Mac with
    // `no_package_for_platform`, when the operator did not say it has none.
    let d = dirs("channel-stage-required");
    let linux = [three()[0], three()[1]];
    write_release(&d.from, SERVICE, VERSION, &linux);
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Refused(vec![
            "no_package_for_platform: the release index has no package for macos-arm64, which \
             --require-platforms names"
                .to_owned()
        ]))
    );
    assert!(!d.out.exists());
    let both = [Platform::LinuxArm64, Platform::LinuxX86_64];
    let narrowed = Request {
        require: both.to_vec(),
        ..request(&d)
    };
    assert_eq!(
        stage(&narrowed),
        Ok(lines(&d.out, SERVICE, VERSION, &both, "staged"))
    );
    assert_eq!(
        bytes_of(&tree(&d.out)),
        expected(&d.from, SERVICE, VERSION, &both)
    );
}

#[test]
fn an_index_named_by_its_flag_is_staged_as_index_json() {
    // Catches: `--index` ignored (the default read instead), or the index staged under its asset
    // name (`hello-index.json`) where the daemon asks for `index.json`.
    let d = dirs("channel-stage-named-index");
    write_release(&d.from, "hello", VERSION, &three());
    let named = d.from.join("hello-index.json");
    std::fs::rename(d.from.join("index.json"), &named).expect("rename the index");
    let request = Request {
        index: Some(named.clone()),
        ..request_of(&d, "hello", VERSION)
    };
    assert_eq!(
        stage(&request),
        Ok(lines(&d.out, "hello", VERSION, &Platform::ALL, "staged"))
    );
    assert_eq!(
        std::fs::read(d.out.join("hello").join(VERSION).join("index.json")).expect("read"),
        std::fs::read(&named).expect("read")
    );
}

#[test]
fn an_index_the_daemon_would_refuse_is_refused() {
    // Catches: an index staged that the daemon refuses as `index_invalid` (too long, not UTF-8,
    // another service), or one that is valid but not written canonically.
    let d = dirs("channel-stage-index");
    write_release(&d.from, SERVICE, VERSION, &three());
    let path = d.from.join("index.json");
    let canonical = std::fs::read_to_string(&path).expect("read the index");
    let failed = |text: &[u8]| {
        std::fs::write(&path, text).expect("write the index");
        stage(&request(&d))
    };
    let shown = path.display();

    // Padded to exactly the cap, the index is read and judged on its form.
    let mut padded = canonical.clone().into_bytes();
    padded.resize(MAX_INDEX_BYTES as usize, b' ');
    let not_canonical = format!(
        "{shown}: not the canonical form of the index (keys sorted, packages sorted by platform, \
         no white space, no final line break), which is:\n{canonical}"
    );
    assert_eq!(failed(&padded), Err(Error::Failed(not_canonical.clone())));
    padded.push(b' ');
    assert_eq!(
        failed(&padded),
        Err(Error::Failed(format!(
            "{shown}: index_invalid: longer than 65536 bytes, the most the daemon reads of an \
             index"
        )))
    );
    assert_eq!(
        failed(&[0xff, b'{', b'}']),
        Err(Error::Failed(format!("{shown}: index_invalid: not UTF-8")))
    );
    assert_eq!(
        failed(format!("{canonical}\n").as_bytes()),
        Err(Error::Failed(not_canonical))
    );
    let other = canonical.replace("\"service\":\"tierstore\"", "\"service\":\"hello\"");
    assert_eq!(
        failed(other.as_bytes()),
        Err(Error::Failed(format!(
            "{shown}: index_invalid: the release index is for the service \"hello\", not \
             \"tierstore\""
        )))
    );
    assert!(!d.out.exists());
}

#[test]
fn a_bundle_the_daemon_would_not_take_is_refused() {
    // Catches: a bundle staged that the daemon's proof refuses before asking the verifier: over
    // its 1 MiB cap, not a bundle, or attesting other bytes than the package's.
    let d = dirs("channel-stage-bundles");
    write_release(&d.from, SERVICE, VERSION, &three());
    let [arm, x86, mac] = Platform::ALL.map(|p| bundle_name(SERVICE, VERSION, p));
    let write = |name: &str, bytes: &[u8]| std::fs::write(d.from.join(name), bytes).expect("write");

    // At the cap, a bundle is read and taken.
    let mut at_cap = bundle_listing(&[&sha256(three()[0].1)]);
    at_cap.resize(MAX_BUNDLE_BYTES as usize, b' ');
    write(&arm, &at_cap);
    at_cap.push(b' ');
    write(&x86, &at_cap);
    write(&mac, b"not json");
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Refused(vec![
            format!("{x86}: longer than 1048576 bytes, the most the daemon reads of a bundle"),
            format!("{mac}: not an attestation bundle: not JSON"),
        ]))
    );

    write(
        &arm,
        br#"{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json"}"#,
    );
    write(&x86, br#"{"dsseEnvelope":{"payload":"not base64!"}}"#);
    let other = sha256(b"other bytes");
    write(&mac, &bundle_listing(&[&other]));
    let mac_package = package_name(SERVICE, VERSION, Platform::MacosArm64);
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Refused(vec![
            format!("{arm}: not an attestation bundle: no DSSE payload"),
            format!(
                "{x86}: not an attestation bundle: the DSSE payload is not a JSON statement in \
                 base64"
            ),
            format!(
                "{mac}: does not list the SHA-256 {} of {mac_package}",
                sha256(three()[2].1)
            ),
        ]))
    );
    assert!(!d.out.exists());
}

#[test]
fn a_package_that_is_not_a_regular_file_or_not_in_the_index_is_refused() {
    // Catches: a directory (or anything but a file) taken as a package, a package listed but
    // absent, or a package of the release left out of its index.
    let d = dirs("channel-stage-files");
    write_release(&d.from, SERVICE, VERSION, &three()[..2]);
    let arm = package_name(SERVICE, VERSION, Platform::LinuxArm64);
    std::fs::remove_file(d.from.join(&arm)).expect("remove");
    std::fs::create_dir(d.from.join(&arm)).expect("a directory in its place");
    let x86 = package_name(SERVICE, VERSION, Platform::LinuxX86_64);
    std::fs::remove_file(d.from.join(&x86)).expect("remove");
    let mac = package_name(SERVICE, VERSION, Platform::MacosArm64);
    std::fs::write(d.from.join(&mac), b"an unlisted package").expect("write");
    let request = Request {
        require: vec![Platform::LinuxX86_64],
        ..request(&d)
    };
    assert_eq!(
        stage(&request),
        Err(Error::Refused(vec![
            format!("{arm}: not a regular file"),
            format!("{x86}: missing, but the index lists it"),
            format!(
                "{mac}: in {} but not in the index, which must list every package of the release",
                d.from.display()
            ),
        ]))
    );
    assert!(!d.out.exists());
}

#[test]
fn a_bundle_name_longer_than_a_file_name_may_be_is_refused() {
    // Catches: a version whose bundle name no file system can hold, found only half way through
    // the copy. 219 bytes of version give 256 bytes for the linux-x86_64 bundle and exactly 255
    // for the two others, which are taken.
    let d = dirs("channel-stage-long");
    let v = format!("1{}", ".0".repeat(109));
    assert_eq!(v.len(), 219);
    write_index(&d.from, "t", &v, &three());
    let long = bundle_name("t", &v, Platform::LinuxX86_64);
    assert_eq!(bundle_name("t", &v, Platform::MacosArm64).len(), MAX_NAME);
    assert_eq!(
        stage(&request_of(&d, "t", &v)),
        Err(Error::Refused(vec![format!(
            "the bundle of linux-x86_64, {long}, is a file name of 256 bytes, more than the 255 a \
             file system takes"
        )]))
    );
}

#[test]
fn a_path_under_out_that_is_not_a_file_is_refused() {
    // Catches: a directory where a file goes taken as staged, or a path that cannot be read
    // under `--out` taken as absent.
    let d = dirs("channel-stage-targets");
    write_release(&d.from, SERVICE, VERSION, &three());
    let dir = d.out.join(SERVICE).join(VERSION);
    let x86 = dir.join(package_name(SERVICE, VERSION, Platform::LinuxX86_64));
    std::fs::create_dir_all(&x86).expect("a directory where the package goes");
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Refused(vec![format!(
            "{}: not a regular file",
            x86.display()
        )]))
    );
    assert_eq!(tree(&d.out), BTreeMap::new());

    std::fs::remove_dir_all(&d.out).expect("clear out/");
    std::fs::create_dir(&d.out).expect("make out/");
    std::fs::write(
        d.out.join(SERVICE),
        b"a file where the service directory goes",
    )
    .expect("w");
    let arm = dir.join(package_name(SERVICE, VERSION, Platform::LinuxArm64));
    assert_eq!(
        stage(&request(&d)),
        Err(Error::Io {
            what: format!("read {}", arm.display()),
            why: "Not a directory (os error 20)".to_owned(),
        })
    );
}

#[test]
fn place_refuses_bytes_that_changed_since_they_were_checked() {
    // Catches: a file linked into the channel with other bytes than the ones checked (the source
    // changed between the check and the copy), or its temporary file left behind.
    let d = scratch("channel-stage-place-changed");
    let source = d.join("source");
    std::fs::write(&source, b"abc").expect("write");
    let target = d.join("target");
    let placed = place(
        &d,
        &target,
        &Source::File(source.clone()),
        &Measure::of(b"abd"),
    );
    assert_eq!(
        placed,
        Err(Error::Failed(format!(
            "{} changed while it was staged: 3 bytes with SHA-256 {}, not 3 bytes with SHA-256 \
             {}; nothing was linked",
            source.display(),
            sha256(b"abc"),
            sha256(b"abd")
        )))
    );
    assert_eq!(
        bytes_of(&tree(&d)),
        BTreeMap::from([("source".to_owned(), b"abc".to_vec())])
    );
}

#[test]
fn place_never_replaces_a_file_and_leaves_no_temporary_file() {
    // Catches: a staged file replaced by a later write (a rename over it), or a temporary file
    // left in the served tree when the link or the read fails.
    let d = scratch("channel-stage-place-exists");
    let target = d.join("target");
    std::fs::write(&target, b"first").expect("write");
    let text = Source::Text("second".to_owned());
    let placed = place(&d, &target, &text, &Measure::of(b"second"));
    let Err(Error::Io { what, why }) = placed else {
        panic!("not an I/O refusal: {placed:?}");
    };
    assert!(what.starts_with("link "), "{what}");
    assert!(
        what.ends_with(&format!(" to {}", target.display())),
        "{what}"
    );
    assert_eq!(why, "File exists (os error 17)");
    assert_eq!(
        bytes_of(&tree(&d)),
        BTreeMap::from([("target".to_owned(), b"first".to_vec())])
    );

    let absent = d.join("absent");
    let placed = place(
        &d,
        &d.join("new"),
        &Source::File(absent.clone()),
        &Measure::of(b""),
    );
    assert_eq!(
        placed,
        Err(Error::Io {
            what: format!("read {}", absent.display()),
            why: "No such file or directory (os error 2)".to_owned(),
        })
    );
    assert_eq!(
        bytes_of(&tree(&d)),
        BTreeMap::from([("target".to_owned(), b"first".to_vec())])
    );
}

#[test]
fn copy_measured_measures_what_it_copies_across_buffers() {
    // Catches: a measure of the first buffer only, or bytes counted but not copied.
    let bytes: Vec<u8> = (0..(1 << 16) + 3)
        .map(|n: u32| n.to_le_bytes()[0])
        .collect();
    let mut copy = Vec::new();
    let measure = copy_measured(bytes.as_slice(), &mut copy).expect("copy");
    assert_eq!(measure, Measure::of(&bytes));
    assert_eq!(copy, bytes);
}

#[test]
fn the_index_cap_is_the_daemons() {
    // Catches: the cap here drifting from the one the daemon's fetch applies.
    assert_eq!(MAX_INDEX_BYTES, hostkeeper_package::index::MAX_INDEX_BYTES);
}

/// The argv of the `release` command, as clap reads it.
#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    verb: Verb,
}

fn parse(args: &[&str]) -> Result<Verb, clap::Error> {
    let argv = ["release", "channel-stage", "--from", "f", "--out", "o"];
    Args::try_parse_from(argv.iter().chain(args)).map(|a| a.verb)
}

#[test]
fn the_flags_take_the_daemons_names_and_platforms() {
    // Catches: a service or version outside the daemon's grammar taken (a path separator, a `+`
    // in a service name, which no unit name may hold), a `+` in a version refused, or the
    // required platforms not defaulting to all three.
    let Ok(Verb::ChannelStage {
        service,
        version,
        index,
        require_platforms,
        ..
    }) = parse(&["--service", "tierstore", "--version", "1.4.0+g1a2b3c"])
    else {
        panic!("not parsed");
    };
    assert_eq!(
        (service.as_str(), version.as_str(), index),
        ("tierstore", "1.4.0+g1a2b3c", None)
    );
    assert_eq!(require_platforms, Platform::ALL.to_vec());

    let narrowed = parse(&[
        "--service",
        "hostkeeper",
        "--version",
        "0.1.9",
        "--require-platforms",
        "linux-x86_64,linux-arm64",
        "--index",
        "i.json",
    ]);
    let Ok(Verb::ChannelStage {
        require_platforms,
        index,
        ..
    }) = narrowed
    else {
        panic!("not parsed");
    };
    assert_eq!(
        require_platforms,
        [Platform::LinuxX86_64, Platform::LinuxArm64]
    );
    assert_eq!(index, Some(PathBuf::from("i.json")));

    for bad in [
        ["--service", "tier+store", "--version", "1.5.0"],
        ["--service=-tierstore", "--version", "1.5.0", "--index=i"],
        ["--service", "tierstore", "--version", "../1.5.0"],
        ["--service", "tierstore", "--version", ".1.5.0"],
    ] {
        let error = parse(&bad).err().expect("refused");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::ValueValidation,
            "{bad:?}"
        );
    }
    let platform = parse(&[
        "--service",
        "tierstore",
        "--version",
        "1.5.0",
        "--require-platforms",
        "darwin-arm64",
    ]);
    assert_eq!(
        platform.err().map(|e| e.kind()),
        Some(clap::error::ErrorKind::ValueValidation)
    );
}

/// The `channel-stage` verb over `d`, whose `from/` holds no index.
fn without_index(d: &Dirs) -> Verb {
    let from = d.from.display().to_string();
    let out = d.out.display().to_string();
    Args::try_parse_from([
        "release",
        "channel-stage",
        "--service",
        SERVICE,
        "--version",
        VERSION,
        "--from",
        from.as_str(),
        "--out",
        out.as_str(),
    ])
    .map(|a| a.verb)
    .expect("parse the argv")
}

#[test]
fn the_verb_runs_without_asking_for_adapters_and_with_ports_lent_back() {
    // Catches: the verb building the real adapters (and so needing `gh` and `git`) though it
    // copies files only, `run_with` failing it instead of handing it back to `run`, or `--from`
    // not reaching it.
    let d = dirs("channel-stage-verb");
    let missing = Err(Error::Io {
        what: format!("read {}", d.from.join("index.json").display()),
        why: "No such file or directory (os error 2)".to_owned(),
    });
    let connected = std::cell::Cell::new(false);
    let out = crate::release::run(without_index(&d), || {
        connected.set(true);
        Fakes::over(FakeHistory::default())
    });
    assert_eq!(out, missing);
    assert!(!connected.get());

    let fakes = Fakes::over(FakeHistory::default());
    let out = crate::release::run_with(without_index(&d), &fakes.ports());
    assert_eq!(out, missing);
}
