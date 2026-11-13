//! `cargo xtask channels descriptor`: the descriptor of the repository at `packages.komira.ai`,
//! `hostkeeper-repository.json` ([`hostkeeper_release::descriptor`], docs/deploy/package-repositories.md,
//! "Structure of a repository"), which lets an operator add the whole repository to a coordinator
//! with its URL alone.
//!
//! The host is shared by the organisation's products, so the descriptor is **merged**, never
//! overwritten: `write` reads the object that is there with its ETag, adds this product's
//! channels, architectures, installers and packages to it ([`merge`]) and writes the result back
//! with `If-Match` on that ETag (`If-None-Match: *` when none is there), reading and deciding again
//! after a lost race (`--attempts`). It removes nothing another product put there, and it refuses,
//! naming both, to change a key the descriptor already holds: the organisation's key is not this
//! product's to replace. The fingerprints it writes are computed from the keyring in the bucket
//! ([`hostkeeper_release::registry::keys`]), and the result is judged by
//! [`hostkeeper_release::descriptor::check`], the function the coordinator judges it with, before
//! it is written. The object is served with `Cache-Control: public, max-age=60`, and the path is
//! invalidated in the CDN when `--distribution-id` is given.
//!
//! `check` reads the descriptor and the keyring it names from the public host over HTTPS (`curl`,
//! behind [`Host`], so a test answers in place of it) and holds them to the coordinator's own
//! rules, and the fingerprints the descriptor claims to the keyring's.
//!
//! - `channels descriptor write --bucket <b> --keyring-key <key> --work <dir>
//!   [--distribution-id <id>] [--attempts <n>]` ([`write`]). Job `descriptor`: the cloud role, no
//!   signing key.
//! - `channels descriptor check --url <url> --work <dir>` ([`check`]). The same job, after `write`.
//!
//! New in this repository.

use std::path::PathBuf;

use clap::Subcommand;
use hostkeeper_release::descriptor as rules;
use serde_json::{Map, Value, json};

use super::apt::{Failure, classify, strings};
use super::aws::{Answer, Aws, ExecAws, is_bucket, is_distribution_id};
use super::check::url_flag;
use super::host::{ExecHost, Host};
use super::pkg::{POINTER_CACHE, refusal};
use super::spec::{self, Product};
use super::{Error, read};
use crate::channels::apt::read::etag_of;
use crate::release::ports::Exec;

/// How often the descriptor is read and decided again after a concurrent write, by default.
pub(crate) const ATTEMPTS: u32 = 3;

/// The repository's name for itself, in a descriptor that has none.
const NAME: &str = "komira";

/// The description of a descriptor that has none.
const DESCRIPTION: &str = "komira-ai packages";

/// The channels this product publishes: the apt suites and the pointer names.
const CHANNELS: [&str; 2] = ["testing", "stable"];

/// The directory of the apt repository below the base.
const APT_PATH: &str = "apt";

/// The directory of the rpm tree below the base.
const RPM_PATH: &str = "rpm";

/// The components of every suite.
const COMPONENTS: [&str; 1] = ["main"];

/// The platform of the macOS installers.
const MAC_PLATFORM: &str = "macos-arm64";

/// The `channels descriptor` verbs.
#[derive(Subcommand)]
pub(crate) enum Verb {
    /// This product's facts merged into the descriptor in the bucket, with `If-Match`.
    Write {
        /// The repository's bucket.
        #[arg(long)]
        bucket: String,
        /// The CDN distribution to invalidate the descriptor in; empty for none.
        #[arg(long, default_value = "")]
        distribution_id: String,
        /// The key of the binary keyring in the bucket (`apt/komira-archive-keyring.gpg`): the
        /// descriptor's `key.keyring`, whose fingerprints are read from it.
        #[arg(long)]
        keyring_key: String,
        /// A directory for the files the verb makes.
        #[arg(long)]
        work: PathBuf,
        /// How often the descriptor is read and decided again after a concurrent write.
        #[arg(long, default_value_t = ATTEMPTS)]
        attempts: u32,
        /// The `aws` program.
        #[arg(long, default_value = "aws")]
        aws: PathBuf,
    },
    /// The descriptor and the keyring as the public host serves them, held to the coordinator's
    /// rules.
    Check {
        /// The repository host, `https://packages.komira.ai`.
        #[arg(long)]
        url: String,
        /// A directory for the downloads.
        #[arg(long)]
        work: PathBuf,
        /// The `curl` program.
        #[arg(long, default_value = "curl")]
        curl: PathBuf,
    },
}

/// Runs a verb: the lines to print.
///
/// # Errors
/// Whatever the verb refuses or fails on.
pub(crate) fn run(verb: Verb, exec: &dyn Exec) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Write {
            bucket,
            distribution_id,
            keyring_key,
            work,
            attempts,
            aws,
        } => write(
            &ExecAws::new(exec, aws),
            &Request {
                bucket: &bucket,
                distribution_id: &distribution_id,
                keyring_key: &keyring_key,
                work: &work,
                attempts,
            },
            &spec::HOSTKEEPER,
        ),
        Verb::Check { url, work, curl } => check(&ExecHost(exec), &url, &work, &curl),
    }
}

/// What `write` is asked.
pub(crate) struct Request<'a> {
    pub(crate) bucket: &'a str,
    /// The CDN distribution to invalidate the descriptor in; empty for none.
    pub(crate) distribution_id: &'a str,
    pub(crate) keyring_key: &'a str,
    pub(crate) work: &'a std::path::Path,
    pub(crate) attempts: u32,
}

/// The key of the descriptor in the bucket.
fn key() -> &'static str {
    rules::FILE
}

/// This product's contribution to the descriptor: what it publishes, as the descriptor's JSON,
/// the key's fingerprints being `fingerprints` and its keyring `keyring`.
pub(crate) fn contribution(product: &Product, keyring: &str, fingerprints: &[String]) -> Value {
    let deb: Vec<&str> = {
        let mut names: Vec<&str> = product
            .packages
            .iter()
            .flat_map(|p| p.arches.iter().map(|a| a.deb))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    let rpm: Vec<&str> = {
        let mut names: Vec<&str> = product
            .packages
            .iter()
            .filter(|p| p.rpm)
            .flat_map(|p| p.arches.iter().map(|a| a.rpm))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    let dir = product
        .pkg_dir
        .strip_prefix(spec::PKG_ROOT)
        .unwrap_or(product.pkg_dir);
    let packages: Map<String, Value> = product
        .installers
        .iter()
        .map(|name| ((*name).to_owned(), json!({ "dir": dir })))
        .collect();
    json!({
        "version": rules::VERSION,
        "name": NAME,
        "description": DESCRIPTION,
        "channels": CHANNELS,
        "key": { "keyring": keyring, "fingerprints": fingerprints },
        "formats": {
            "apt": { "path": APT_PATH, "components": COMPONENTS, "architectures": deb },
            "rpm": { "path": RPM_PATH, "architectures": rpm },
            "macos-pkg": {
                "path": spec::PKG_ROOT.trim_end_matches('/'),
                "platforms": [MAC_PLATFORM],
                "packages": packages,
            },
        },
    })
}

/// `object[key]` as a mutable object, made when it is not there; an error when it is there and
/// is no object.
fn object_at<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a mut Map<String, Value>, Error> {
    object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| Error::Failed(format!("the descriptor's {path}{key} is not an object")))
}

/// Adds the texts of `ours` that `theirs` lacks to the array `theirs`, after the ones it has.
fn union(theirs: &mut Value, ours: &Value, what: &str) -> Result<(), Error> {
    let held = theirs
        .as_array_mut()
        .ok_or_else(|| Error::Failed(format!("the descriptor's {what} is not a list")))?;
    for item in ours.as_array().into_iter().flatten() {
        if !held.contains(item) {
            held.push(item.clone());
        }
    }
    Ok(())
}

/// A value `ours` that must be the one `theirs` already holds: set when `theirs` has none.
fn same(theirs: &mut Map<String, Value>, key: &str, ours: &Value, what: &str) -> Result<(), Error> {
    match theirs.get(key) {
        None => {
            theirs.insert(key.to_owned(), ours.clone());
            Ok(())
        }
        Some(held) if held == ours => Ok(()),
        Some(held) => Err(Error::Failed(format!(
            "the descriptor's {what} is {held}, and this release would write {ours}: another product's \
             value is not this one's to change"
        ))),
    }
}

/// `existing` (the descriptor in the bucket, none when there is none) with `ours` merged in:
/// version, name and description set when absent; the channels, components and architectures
/// united; the paths, the keyring and the fingerprints required to be the ones already there; this
/// product's packages added with the directory of their installers, another product's kept and a
/// package of the same name in another directory refused. Every key of `existing` that `ours` has
/// no word about is kept as it is.
///
/// # Errors
/// What differs from what is there (a path, the keyring, the fingerprints, a package's directory),
/// a version newer than this build reads, or a part of `existing` that is not of its form.
pub(crate) fn merge(existing: Option<Value>, ours: &Value) -> Result<Value, Error> {
    let mut merged = existing.unwrap_or_else(|| json!({}));
    let root = merged.as_object_mut().ok_or_else(|| {
        Error::Failed("the descriptor in the bucket is not a JSON object".to_owned())
    })?;
    if let Some(version) = root.get("version").and_then(Value::as_u64)
        && version > rules::VERSION
    {
        return Err(Error::Failed(format!(
            "the descriptor in the bucket is version {version}, newer than this build writes ({})",
            rules::VERSION
        )));
    }
    root.insert("version".to_owned(), json!(rules::VERSION));
    for key in ["name", "description"] {
        root.entry(key.to_owned())
            .or_insert_with(|| ours[key].clone());
    }
    let channels = root.entry("channels").or_insert_with(|| json!([]));
    union(channels, &ours["channels"], "channels")?;
    let key = object_at(root, "key", "")?;
    same(key, "keyring", &ours["key"]["keyring"], "key.keyring")?;
    same(
        key,
        "fingerprints",
        &ours["key"]["fingerprints"],
        "key.fingerprints",
    )?;
    let formats = object_at(root, "formats", "")?;
    for format in ["apt", "rpm", "macos-pkg"] {
        let theirs = object_at(formats, format, "formats.")?;
        let mine = &ours["formats"][format];
        let at = format!("formats.{format}");
        same(theirs, "path", &mine["path"], &format!("{at}.path"))?;
        for list in ["components", "architectures", "platforms"] {
            if mine.get(list).is_some() {
                let held = theirs.entry(list).or_insert_with(|| json!([]));
                union(held, &mine[list], &format!("{at}.{list}"))?;
            }
        }
        if let Some(packages) = mine.get("packages").and_then(Value::as_object) {
            let held = object_at(theirs, "packages", &format!("{at}."))?;
            for (name, package) in packages {
                match held.get(name) {
                    None => {
                        held.insert(name.clone(), package.clone());
                    }
                    Some(there) if there["dir"] == package["dir"] => {}
                    Some(there) => {
                        return Err(Error::Failed(format!(
                            "the descriptor maps the package {name} to {}, and this release would map it to {}",
                            there["dir"], package["dir"]
                        )));
                    }
                }
            }
        }
    }
    Ok(merged)
}

/// The fingerprints of the keyring `bytes`: each primary key's and its subkeys', in order.
fn fingerprints_of(bytes: &[u8], what: &str) -> Result<Vec<String>, Error> {
    let keys = hostkeeper_release::registry::keys(bytes)
        .map_err(|e| Error::Failed(format!("{what} is not a keyring of public keys: {e}")))?;
    Ok(keys
        .into_iter()
        .flat_map(|key| std::iter::once(key.fingerprint).chain(key.subkeys))
        .collect())
}

/// Reads `key` of the bucket into `dest`: its ETag, or none when it is not there (`NoSuchKey`, or
/// `AccessDenied`: the publish role may not list the bucket).
fn fetch(
    aws: &dyn Aws,
    r: &Request,
    key: &str,
    dest: &std::path::Path,
) -> Result<Option<String>, Error> {
    let got = aws.run(&strings(&[
        "s3api",
        "get-object",
        "--bucket",
        r.bucket,
        "--key",
        key,
        &dest.display().to_string(),
    ]))?;
    if !got.ok {
        return match classify(&got.stderr) {
            Failure::NotFound | Failure::Denied => Ok(None),
            _ => Err(refusal(&format!("read {key}"), &got)),
        };
    }
    etag_of(&got.stdout)
        .map(Some)
        .ok_or_else(|| Error::Failed(format!("read {key}: aws printed no ETag")))
}

/// Invalidates `/<key>`, when a distribution is given.
fn invalidate(aws: &dyn Aws, r: &Request) -> Result<Option<String>, Error> {
    if r.distribution_id.is_empty() {
        return Ok(None);
    }
    let path = format!("/{}", key());
    let made = aws.run(&strings(&[
        "cloudfront",
        "create-invalidation",
        "--distribution-id",
        r.distribution_id,
        "--paths",
        &path,
    ]))?;
    if !made.ok {
        return Err(refusal(&format!("invalidate {path}"), &made));
    }
    Ok(Some(format!("{path}: invalidated")))
}

/// The flags, held to their forms.
fn check_request(r: &Request) -> Result<(), Error> {
    if !is_bucket(r.bucket) {
        return Err(Error::Usage(format!(
            "--bucket {:?} is not a bucket name",
            r.bucket
        )));
    }
    if !r.distribution_id.is_empty() && !is_distribution_id(r.distribution_id) {
        return Err(Error::Usage(format!(
            "--distribution-id {:?} is not a distribution id",
            r.distribution_id
        )));
    }
    if r.keyring_key.is_empty() || r.keyring_key.starts_with('/') {
        return Err(Error::Usage(format!(
            "--keyring-key {:?} is not a key of the bucket",
            r.keyring_key
        )));
    }
    if r.attempts == 0 {
        return Err(Error::Usage(
            "--attempts is 0; it must be 1 or more".to_owned(),
        ));
    }
    Ok(())
}

/// The `write` verb: this product's facts merged into the descriptor of the bucket.
///
/// # Errors
/// A flag out of its form, a keyring that is not there or is no keyring, a descriptor in the
/// bucket that cannot be merged with or that the merge makes invalid, a refusal by AWS, or the
/// object changed under every attempt.
pub(crate) fn write(aws: &dyn Aws, r: &Request, product: &Product) -> Result<Vec<String>, Error> {
    check_request(r)?;
    std::fs::create_dir_all(r.work)
        .map_err(Error::spawn(format!("create {}", r.work.display())))?;
    let ring = r.work.join("keyring.gpg");
    if fetch(aws, r, r.keyring_key, &ring)?.is_none() {
        return Err(Error::Failed(format!(
            "the keyring {} is not in the bucket",
            r.keyring_key
        )));
    }
    let fingerprints = fingerprints_of(&read(&ring)?, r.keyring_key)?;
    let ours = contribution(product, r.keyring_key, &fingerprints);
    let current = r.work.join("descriptor-current.json");
    let next = r.work.join("descriptor-next.json");
    for _ in 0..r.attempts {
        let etag = fetch(aws, r, key(), &current)?;
        let existing =
            match &etag {
                None => None,
                Some(_) => Some(serde_json::from_slice::<Value>(&read(&current)?).map_err(
                    |e| Error::Failed(format!("{} in the bucket is not JSON: {e}", key())),
                )?),
            };
        let merged = merge(existing.clone(), &ours)?;
        if existing.as_ref() == Some(&merged) {
            let mut said = vec![format!("{}: already holds this release's facts", key())];
            said.extend(invalidate(aws, r)?);
            return Ok(said);
        }
        let text = format!(
            "{}\n",
            serde_json::to_string_pretty(&merged).unwrap_or_default()
        );
        rules::check(text.as_bytes())
            .map_err(|e| Error::Failed(format!("the merged descriptor is not valid: {e}")))?;
        std::fs::write(&next, &text).map_err(Error::spawn(format!("write {}", next.display())))?;
        let mut args = strings(&[
            "s3api",
            "put-object",
            "--bucket",
            r.bucket,
            "--key",
            key(),
            "--body",
            &next.display().to_string(),
            "--content-type",
            "application/json",
            "--cache-control",
            POINTER_CACHE,
        ]);
        args.extend(match &etag {
            Some(etag) => vec!["--if-match".to_owned(), etag.clone()],
            None => strings(&["--if-none-match", "*"]),
        });
        let put: Answer = aws.run(&args)?;
        if put.ok {
            let mut said = vec![format!("{}: written", key())];
            said.extend(invalidate(aws, r)?);
            return Ok(said);
        }
        if classify(&put.stderr) != Failure::Precondition {
            return Err(refusal(&format!("put {}", key()), &put));
        }
    }
    Err(Error::Failed(format!(
        "{} was written by another release {} times in a row; run the job again",
        key(),
        r.attempts
    )))
}

/// Downloads `url` to `dest` over HTTPS.
fn download(
    host: &dyn Host,
    curl: &std::path::Path,
    url: &str,
    dest: &std::path::Path,
) -> Result<(), Error> {
    let got = host.run(
        curl,
        &strings(&[
            "--proto",
            "=https",
            "--tlsv1.2",
            "--fail",
            "--silent",
            "--show-error",
            "--output",
            &dest.display().to_string(),
            url,
        ]),
    )?;
    if got.ok {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{url} could not be downloaded: {}",
            got.stderr.trim()
        )))
    }
}

/// The `check` verb: the descriptor and the keyring as the public host serves them.
///
/// # Errors
/// A flag out of its form, a download that fails, a descriptor the coordinator would refuse, a
/// keyring that is no keyring, or a fingerprint the descriptor claims that the keyring lacks.
pub(crate) fn check(
    host: &dyn Host,
    url: &str,
    work: &std::path::Path,
    curl: &std::path::Path,
) -> Result<Vec<String>, Error> {
    url_flag("--url", url)?;
    std::fs::create_dir_all(work).map_err(Error::spawn(format!("create {}", work.display())))?;
    let served = work.join("descriptor.json");
    download(host, curl, &format!("{url}/{}", rules::FILE), &served)?;
    let parsed = rules::check(&read(&served)?)
        .map_err(|e| Error::Failed(format!("{url}/{}: {e}", rules::FILE)))?;
    let ring = work.join("served-keyring.gpg");
    download(host, curl, &format!("{url}/{}", parsed.keyring), &ring)?;
    let held = fingerprints_of(&read(&ring)?, &parsed.keyring)?;
    let problems: Vec<String> = parsed
        .fingerprints
        .iter()
        .filter(|claimed| !held.contains(claimed))
        .map(|claimed| {
            format!(
                "the descriptor claims the fingerprint {claimed}, which no key of {} has",
                parsed.keyring
            )
        })
        .collect();
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    Ok(vec![format!(
        "{url}/{}: valid, {} channels, platforms {}, keys {}",
        rules::FILE,
        parsed.channels.len(),
        parsed.platforms().join(" "),
        held.join(" ")
    )])
}

#[cfg(test)]
mod tests;
