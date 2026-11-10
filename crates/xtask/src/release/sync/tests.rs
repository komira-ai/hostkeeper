use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::Parser;
use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::index::PackageEntry;

use super::*;
use crate::release::contract::{Package, SIGNER_WORKFLOW};
use crate::release::fakes::{
    FakeHistory, FakeSource, FakeVerifier, Fakes, good_attestation, name_of, nth_commit, scratch,
    tree_of, write_assets,
};
use crate::release::ports::Adapters;
use crate::release::ports::{Attestation, PortError};
use crate::release::{Verb, run_with};
use crate::testdir::TestDir;

const TIERSTORE: &str = "komira-ai/tierstore";
const OTHER: &str = "komira-ai/other";
const TIERSTORE_SIGNER: &str = "komira-ai/tierstore/.github/workflows/artifacts.yml";

fn repo(name: &str) -> Repo {
    name.parse().expect("a repository")
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))
}

/// An unsigned bundle whose statement lists `digest` as its subject's SHA-256, as channel-stage
/// reads one.
fn bundle_listing(digest: &str) -> Vec<u8> {
    let statement = serde_json::json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": "package", "digest": {"sha256": digest}}],
    });
    serde_json::json!({"dsseEnvelope": {"payload": STANDARD.encode(statement.to_string())}})
        .to_string()
        .into_bytes()
}

/// The assets of a release, by name.
type Assets = BTreeMap<String, Vec<u8>>;

/// The release of the service `s` at the index version `v`, as a repository other than
/// hostkeeper publishes it: a package per platform holding `marker`, its bundle, the index, one
/// asset that is not staged (a binary tarball), and `SHA256SUMS` of every asset but the bundles.
fn release_of(s: &str, v: &str, marker: &str) -> Assets {
    let mut assets = Assets::new();
    let mut entries = Vec::new();
    for platform in Platform::ALL {
        let name = format!("{s}-{v}-{platform}.tar.zst");
        let bytes = format!("the {platform} package of {s} {v} {marker}").into_bytes();
        entries.push(PackageEntry {
            platform,
            sha256: Sha256Hex::parse(&sha256(&bytes)).expect("a SHA-256"),
            bytes: bytes.len() as u64,
        });
        assets.insert(
            format!("{name}.sigstore.json"),
            bundle_listing(&sha256(&bytes)),
        );
        assets.insert(name, bytes);
    }
    let service = ServiceName::new(s).expect("a service");
    let version = ChannelVersion::new(v).expect("a version");
    let index = ReleaseIndex::new(service, version, entries).expect("an index");
    assets.insert("index.json".to_owned(), index.to_json().into_bytes());
    let tarball = format!("{s}-{v}-linux-x86_64.tar.gz");
    assets.insert(format!("{tarball}.sigstore.json"), b"{}".to_vec());
    assets.insert(tarball, format!("the binary {marker}").into_bytes());
    resum(&mut assets);
    assets
}

/// Writes `SHA256SUMS` of every asset of `assets` but itself and the bundles.
fn resum(assets: &mut Assets) {
    let sums: String = assets
        .iter()
        .filter(|(name, _)| (name.as_str() != SUMS) & !is_bundle(name))
        .map(|(name, bytes)| format!("{}  {name}\n", sha256(bytes)))
        .collect();
    assets.insert(SUMS.to_owned(), sums.into_bytes());
}

/// The hostkeeper release of `version`: every asset `release check` wants, as
/// [`write_assets`] writes them, with each package's bundle listing its SHA-256.
fn hostkeeper_release(version: &Version) -> Assets {
    let dir = scratch("sync-hostkeeper-assets");
    write_assets(&dir, version, "built");
    for package in Package::ALL {
        for &platform in package.platforms() {
            let name = package.package_name(version, platform);
            let digest = file_sha256(&dir.join(&name)).expect("hash a package");
            std::fs::write(dir.join(bundle_of(&name)), bundle_listing(&digest))
                .expect("write a bundle");
        }
    }
    tree_of(&dir)
        .into_iter()
        .map(|name| {
            let bytes = std::fs::read(dir.join(&name)).expect("read an asset");
            (name, bytes)
        })
        .collect()
}

fn publish(source: &FakeSource, repo: &str, tag: &str, commit: u64, assets: &Assets) {
    let list = assets.iter().map(|(n, b)| (n.clone(), b.clone())).collect();
    source.publish(repo, tag, nth_commit(commit).as_str(), list);
}

/// A scratch directory with `channel/` made in it.
struct Channel {
    _root: TestDir,
    dir: PathBuf,
}

fn channel(name: &str) -> Channel {
    let root = scratch(name);
    let dir = root.join("channel");
    std::fs::create_dir(&dir).expect("make the channel");
    Channel { _root: root, dir }
}

/// The trust list entry of `service` from `repository`, signed by `workflow` on `refs` on a
/// hosted runner.
fn entry(service: &str, repository: &str, workflow: &str, refs: &[&str]) -> String {
    format!(
        "[[service]]\nname = {service:?}\nrepository = {repository:?}\nworkflow = {workflow:?}\n\
         refs = {refs:?}\nhosted_runner = true\n"
    )
}

/// The trust list of `entries`.
fn trust(entries: &[String]) -> ChannelTrust {
    ChannelTrust::parse(&entries.concat()).expect("a trust list")
}

/// The trust list of `repos`: each repository's service named for it, and hostkeeper's also
/// hello and pxe, each signed by the repository's `artifacts.yml` on `refs/heads/main`.
fn trust_of(repos: &[&str]) -> ChannelTrust {
    let mut entries = Vec::new();
    for repo in repos {
        let name = repo.rsplit('/').next().expect("a name");
        let artifacts = ".github/workflows/artifacts.yml";
        entries.push(entry(name, repo, artifacts, &["refs/heads/main"]));
        if *repo == REPO {
            entries.push(entry("hello", repo, artifacts, &["refs/heads/main"]));
            entries.push(entry("pxe", repo, artifacts, &["refs/heads/main"]));
        }
    }
    trust(&entries)
}

fn request(channel: &Channel, repos: &[&str]) -> Request {
    Request {
        trust: trust_of(repos),
        channel: channel.dir.clone(),
        since: None,
    }
}

/// Every path under `dir` (a directory's with `/` after it) and the bytes of each file.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    tree_of(dir)
        .into_iter()
        .map(|rel| {
            let bytes = if rel.ends_with('/') {
                Vec::new()
            } else {
                std::fs::read(dir.join(&rel)).expect("read a channel file")
            };
            (rel, bytes)
        })
        .collect()
}

/// What staging `assets` of `s` at `v` puts in the channel: its directories, index, packages and
/// their bundles (those of the platforms the index lists), with the release's bytes.
fn staged(s: &str, v: &str, assets: &Assets, index_name: &str) -> BTreeMap<String, Vec<u8>> {
    let mut tree = BTreeMap::from([
        (format!("{s}/"), Vec::new()),
        (format!("{s}/{v}/"), Vec::new()),
        (format!("{s}/{v}/index.json"), assets[index_name].clone()),
    ]);
    let listed = String::from_utf8(assets[index_name].clone()).expect("a UTF-8 index");
    for platform in Platform::ALL {
        if !listed.contains(&format!("{s}-{v}-{platform}.tar.zst\"")) {
            continue;
        }
        for name in [
            format!("{s}-{v}-{platform}.tar.zst"),
            format!("{s}-{v}-{platform}.tar.zst.sigstore.json"),
        ] {
            tree.insert(format!("{s}/{v}/{name}"), assets[&name].clone());
        }
    }
    tree
}

fn refused_lines(result: Result<Vec<String>, Error>) -> Vec<String> {
    match result {
        Err(Error::Refused(lines)) => lines,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

const TS_V: &str = "0.3.53+0771c30f87f3";

mod refuse;
mod run;
mod stage;
mod trust;
