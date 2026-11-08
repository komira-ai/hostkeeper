//! `release channel-stage`: lays one version of a service out in a directory exactly as the
//! release channel serves it to the daemon (docs/design/hostkeeper-design.md, section 5.5):
//!
//! ```text
//! <out>/S/V/index.json
//! <out>/S/V/S-V-{platform}.tar.zst                 one per platform the index lists
//! <out>/S/V/S-V-{platform}.tar.zst.sigstore.json   that package's attestation bundle
//! ```
//!
//! The paths are `hostkeeper_config::index`'s ([`index_path`], [`package_path`],
//! [`attestation_path`]) and the service and version keep its name grammars, so the tree is the
//! one the daemon's fetch requests. The input is a directory of a release's assets (`--from`),
//! where the packages and their bundles carry these same file names, and the release index of the
//! version (`--index`, `<from>/index.json` unless named: a hostkeeper release also carries
//! `hello-index.json`, `hello-fail-ready-index.json` and `mdm-index.json`).
//!
//! The output is a plain directory, for any static file origin or object-store sync to serve
//! under the daemon config's `release_channel`. This verb copies files only: it uploads nothing,
//! holds no credentials, and has no opinion on the transport, which is the web server's (the
//! daemon requires `https`).
//!
//! Everything is checked before anything is written, and a refusal lists every problem found:
//!
//! 1. the index is at most [`MAX_INDEX_BYTES`] (the most the daemon reads), UTF-8, reads as the
//!    index of S at V by the daemon config's strict parser ([`ReleaseIndex::parse`]), and is byte
//!    for byte its canonical form ([`ReleaseIndex::to_json`]);
//! 2. it lists a package for every platform of `--require-platforms` (all three unless the flag
//!    narrows it): a missing one would leave every host of that platform with
//!    `no_package_for_platform`;
//! 3. the longest file name of the version, a bundle's, is at most [`MAX_NAME`] bytes;
//! 4. each package the index lists is a regular file in `--from` with the index's size and
//!    SHA-256, and its bundle beside it is a regular file of at most [`MAX_BUNDLE_BYTES`] whose
//!    statement lists that SHA-256 as a subject (the bundle's signature is not checked here:
//!    `release check` does that for a hostkeeper release, and the daemon for every package);
//! 5. `--from` holds no package of S at V for a platform the index does not list;
//! 6. each path the version takes under `--out` is absent or a regular file holding the same
//!    bytes. A release is never changed once in the channel, so a different file is refused.
//!
//! Then each absent file is written (the packages and bundles first and the index last, so a
//! reader that finds the index finds its packages): copied to a temporary name in the version's
//! directory while its size and SHA-256 are measured again, and hard-linked to its own name, which
//! never replaces a file. A file is whole or absent. Staging the same release again writes
//! nothing.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::index::{
    PackageEntry, ReleaseIndex, attestation_path, index_path, package_path,
};
use hostkeeper_config::names::{MAX_NAME, ServiceName, Version};
use hostkeeper_config::platform::Platform;
use serde_json::Value;
use sha2::Digest as _;

use super::Error;

/// The most bytes of an index: the daemon refuses a longer one (`index_invalid`).
pub(crate) const MAX_INDEX_BYTES: u64 = 64 * 1024;
/// The most bytes of an attestation bundle: the daemon refuses a longer one (`not_attested`).
pub(crate) const MAX_BUNDLE_BYTES: u64 = 1 << 20;

/// What `release channel-stage` stages, and where.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) service: ServiceName,
    pub(crate) version: Version,
    /// The directory of the release's assets.
    pub(crate) from: PathBuf,
    /// The release index; `<from>/index.json` when none.
    pub(crate) index: Option<PathBuf>,
    /// The platforms the index must list a package for.
    pub(crate) require: Vec<Platform>,
    /// The root of the channel's tree.
    pub(crate) out: PathBuf,
}

/// The size and SHA-256 (64 lowercase hex digits) of some bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Measure {
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

impl Measure {
    fn of(bytes: &[u8]) -> Measure {
        Measure {
            bytes: bytes.len() as u64,
            sha256: hex::encode(sha2::Sha256::digest(bytes)),
        }
    }
}

/// Where the bytes of a staged file come from.
#[derive(Clone, Debug)]
pub(crate) enum Source {
    File(PathBuf),
    Text(String),
}

/// One file of the version: its path under `--out` (and in the channel), its bytes, and what
/// they measure.
struct Item {
    rel: String,
    source: Source,
    measure: Measure,
}

/// What is at a path.
enum Found {
    Missing,
    File,
    Other,
}

/// What is at `path`, not following a link.
fn found(path: &Path) -> Result<Found, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Found::File),
        Ok(_) => Ok(Found::Other),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Found::Missing),
        Err(e) => Err(Error::io(format!("read {}", path.display()))(e)),
    }
}

/// The last component of a channel path.
fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Copies `input` to `output`, measuring the bytes as they pass.
pub(crate) fn copy_measured(
    mut input: impl Read,
    mut output: impl Write,
) -> std::io::Result<Measure> {
    let mut hasher = sha2::Sha256::new();
    let mut bytes = 0_u64;
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
        bytes += n as u64;
    }
    Ok(Measure {
        bytes,
        sha256: hex::encode(hasher.finalize()),
    })
}

/// The measure of the file at `path`, read in one streaming pass.
fn measure_file(path: &Path) -> Result<Measure, Error> {
    let what = format!("read {}", path.display());
    let file = File::open(path).map_err(Error::io(&what))?;
    copy_measured(file, std::io::sink()).map_err(Error::io(&what))
}

/// At most `cap + 1` bytes of the file at `path`: more than `cap` means it is over the cap.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, Error> {
    let what = format!("read {}", path.display());
    let file = File::open(path).map_err(Error::io(&what))?;
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io(&what))?;
    Ok(bytes)
}

/// The text of the index at `path` and the index it reads as, which must be the canonical index
/// of `service` at `version`.
fn load_index(
    path: &Path,
    service: &ServiceName,
    version: &Version,
) -> Result<(String, ReleaseIndex), Error> {
    let shown = path.display();
    let bytes = read_capped(path, MAX_INDEX_BYTES)?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(Error::Failed(format!(
            "{shown}: index_invalid: longer than {MAX_INDEX_BYTES} bytes, the most the daemon \
             reads of an index"
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::Failed(format!("{shown}: index_invalid: not UTF-8")))?;
    let index = ReleaseIndex::parse(&text, service, version)
        .map_err(|e| Error::Failed(format!("{shown}: {}: {e}", e.reason())))?;
    if text != index.to_json() {
        return Err(Error::Failed(format!(
            "{shown}: not the canonical form of the index (keys sorted, packages sorted by \
             platform, no white space, no final line break), which is:\n{}",
            index.to_json()
        )));
    }
    Ok((text, index))
}

/// The SHA-256 digests of the subjects of the in-toto statement in an attestation bundle, or what
/// the bundle lacks. Nothing here is believed: the signature is the daemon's to check.
fn subjects(bundle: &[u8]) -> Result<Vec<String>, &'static str> {
    let value: Value = serde_json::from_slice(bundle).map_err(|_| "not JSON")?;
    let payload = value
        .pointer("/dsseEnvelope/payload")
        .and_then(Value::as_str)
        .ok_or("no DSSE payload")?;
    let statement: Value = STANDARD
        .decode(payload)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .ok_or("the DSSE payload is not a JSON statement in base64")?;
    Ok(statement
        .get("subject")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|subject| subject.pointer("/digest/sha256")?.as_str())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default())
}

/// What is wrong with `bundle`, the bundle of the package `name` whose SHA-256 is `sha256`, if
/// anything: over the daemon's cap, not a bundle, or not listing that SHA-256.
fn bundle_problem(bundle: &[u8], name: &str, sha256: &str) -> Option<String> {
    if bundle.len() as u64 > MAX_BUNDLE_BYTES {
        return Some(format!(
            "longer than {MAX_BUNDLE_BYTES} bytes, the most the daemon reads of a bundle"
        ));
    }
    match subjects(bundle) {
        Ok(list) if list.iter().any(|s| s == sha256) => None,
        Ok(_) => Some(format!("does not list the SHA-256 {sha256} of {name}")),
        Err(why) => Some(format!("not an attestation bundle: {why}")),
    }
}

/// Whether `name` in `from` is a regular file; when not, why, in `problems` (`missing` when
/// nothing is there).
fn regular_file(
    from: &Path,
    name: &str,
    missing: &str,
    problems: &mut Vec<String>,
) -> Result<bool, Error> {
    let why = match found(&from.join(name))? {
        Found::File => return Ok(true),
        Found::Missing => missing,
        Found::Other => "not a regular file",
    };
    problems.push(format!("{name}: {why}"));
    Ok(false)
}

/// The package of `entry` and its bundle, checked in `from`; every problem goes to `problems`.
fn package_items(
    from: &Path,
    service: &ServiceName,
    version: &Version,
    entry: &PackageEntry,
    problems: &mut Vec<String>,
) -> Result<[Item; 2], Error> {
    let rel = package_path(service, version, entry.platform);
    let name = file_name(&rel);
    let path = from.join(name);
    let listed = Measure {
        bytes: entry.bytes,
        sha256: entry.sha256.as_str().to_owned(),
    };
    if regular_file(from, name, "missing, but the index lists it", problems)? {
        let got = measure_file(&path)?;
        if got != listed {
            problems.push(format!(
                "{name}: package_digest_mismatch: {} bytes with SHA-256 {}, but the index lists \
                 {} bytes with SHA-256 {}",
                got.bytes, got.sha256, listed.bytes, listed.sha256
            ));
        }
    }
    let bundle_rel = attestation_path(service, version, entry.platform);
    let bundle_name = file_name(&bundle_rel);
    let bundle_path = from.join(bundle_name);
    let missing = "missing: every package is staged with its attestation bundle";
    let bundle_measure = if regular_file(from, bundle_name, missing, problems)? {
        let bytes = read_capped(&bundle_path, MAX_BUNDLE_BYTES)?;
        if let Some(why) = bundle_problem(&bytes, name, &listed.sha256) {
            problems.push(format!("{bundle_name}: {why}"));
        }
        Measure::of(&bytes)
    } else {
        // Never compared: the missing bundle refuses the release before any target is read.
        Measure::of(b"")
    };
    Ok([
        Item {
            rel,
            source: Source::File(path),
            measure: listed,
        },
        Item {
            rel: bundle_rel,
            source: Source::File(bundle_path),
            measure: bundle_measure,
        },
    ])
}

/// What is at an item's path under `--out`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Absent,
    Same,
    Refused,
}

/// What is at `item`'s path under `out`; a refusal goes to `problems`.
fn target(out: &Path, item: &Item, problems: &mut Vec<String>) -> Result<Target, Error> {
    let path = out.join(&item.rel);
    let why = match found(&path)? {
        Found::Missing => return Ok(Target::Absent),
        Found::File => {
            if measure_file(&path)? == item.measure {
                return Ok(Target::Same);
            }
            "already staged with other bytes; a release in the channel never changes"
        }
        Found::Other => "not a regular file",
    };
    problems.push(format!("{}: {why}", path.display()));
    Ok(Target::Refused)
}

/// The number the next temporary name of this process takes.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Writes `source` to a new temporary file in `dir`, refusing it unless it measures `expected`.
fn write_temp(temp: &Path, source: &Source, expected: &Measure) -> Result<(), Error> {
    let what = format!("write {}", temp.display());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(Error::io(&what))?;
    let (got, from) = match source {
        Source::File(path) => {
            let input = File::open(path).map_err(Error::io(format!("read {}", path.display())))?;
            (copy_measured(input, &mut file), path.display().to_string())
        }
        Source::Text(text) => (
            copy_measured(text.as_bytes(), &mut file),
            "the index".to_owned(),
        ),
    };
    let got = got.map_err(Error::io(&what))?;
    file.sync_all().map_err(Error::io(&what))?;
    if got != *expected {
        return Err(Error::Failed(format!(
            "{from} changed while it was staged: {} bytes with SHA-256 {}, not {} bytes with \
             SHA-256 {}; nothing was linked",
            got.bytes, got.sha256, expected.bytes, expected.sha256
        )));
    }
    Ok(())
}

/// Writes `source` at `target`, a path in `dir` where nothing is: through a temporary file in
/// `dir` that must measure `expected`, hard-linked to `target` (which never replaces a file), and
/// then removed whether or not the link was made.
pub(crate) fn place(
    dir: &Path,
    target: &Path,
    source: &Source,
    expected: &Measure,
) -> Result<(), Error> {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".channel-stage-{}-{n}", std::process::id()));
    let linked = write_temp(&temp, source, expected).and_then(|()| {
        std::fs::hard_link(&temp, target).map_err(Error::io(format!(
            "link {} to {}",
            temp.display(),
            target.display()
        )))
    });
    let removed =
        std::fs::remove_file(&temp).map_err(Error::io(format!("remove {}", temp.display())));
    linked.and(removed)
}

/// The `release channel-stage` verb: checks the release of `request.service` at
/// `request.version` in `request.from` and stages it under `request.out` (see the module). Prints
/// one line per file, `staged` or `unchanged`, and a summary.
pub(crate) fn stage(request: &Request) -> Result<Vec<String>, Error> {
    let Request {
        service,
        version,
        from,
        index,
        require,
        out,
    } = request;
    let index_at = index.clone().unwrap_or_else(|| from.join("index.json"));
    let (text, parsed) = load_index(&index_at, service, version)?;

    let mut problems: Vec<String> = require
        .iter()
        .filter_map(|platform| parsed.entry_for(*platform).err())
        .map(|e| format!("{}: {e}, which --require-platforms names", e.reason()))
        .collect();
    for entry in parsed.packages() {
        let bundle = attestation_path(service, version, entry.platform);
        let length = file_name(&bundle).len();
        if length > MAX_NAME {
            problems.push(format!(
                "the bundle of {}, {}, is a file name of {length} bytes, more than the \
                 {MAX_NAME} a file system takes",
                entry.platform,
                file_name(&bundle)
            ));
        }
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }

    let mut items = Vec::new();
    for entry in parsed.packages() {
        items.extend(package_items(from, service, version, entry, &mut problems)?);
    }
    let unlisted = Platform::ALL
        .into_iter()
        .filter(|platform| parsed.entry_for(*platform).is_err())
        .map(|platform| package_path(service, version, platform))
        .filter(|rel| std::fs::symlink_metadata(from.join(file_name(rel))).is_ok());
    for rel in unlisted {
        problems.push(format!(
            "{}: in {} but not in the index, which must list every package of the release",
            file_name(&rel),
            from.display()
        ));
    }
    items.push(Item {
        rel: index_path(service, version),
        measure: Measure::of(text.as_bytes()),
        source: Source::Text(text),
    });
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }

    let targets = items
        .iter()
        .map(|item| target(out, item, &mut problems))
        .collect::<Result<Vec<Target>, Error>>()?;
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }

    let dir = out.join(service.as_str()).join(version.as_str());
    std::fs::create_dir_all(&dir).map_err(Error::io(format!("create {}", dir.display())))?;
    let mut lines = Vec::new();
    let mut written = 0;
    for (item, state) in items.iter().zip(targets) {
        let word = match state {
            Target::Absent => {
                place(&dir, &out.join(&item.rel), &item.source, &item.measure)?;
                written += 1;
                "staged"
            }
            _ => "unchanged",
        };
        lines.push(format!("{word} {}", item.rel));
    }
    let platforms: Vec<&str> = parsed
        .packages()
        .iter()
        .map(|e| e.platform.as_str())
        .collect();
    lines.push(format!(
        "{}: the index and the packages of {} with their bundles; {written} of {} files written",
        dir.display(),
        platforms.join(", "),
        items.len()
    ));
    Ok(lines)
}

#[cfg(test)]
mod tests;
