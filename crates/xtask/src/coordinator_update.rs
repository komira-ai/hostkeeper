//! `coordinator-update`: the machine that runs the coordinator's container rolls it forward to the
//! newest published release by itself (`docs/deploy/coordinator-update.md`). A systemd timer runs
//! it (`docs/deploy/coordinator/hostkeeper-coordinator-update.{timer,service}`); CI never
//! reaches the machine, and a release only reads the coordinator's `GET /v1/version` and waits.
//!
//! The guarantees a hand deploy gave are kept: the image is the release's attested image, what
//! docker loaded is checked, the new container replaces the old with the old kept until the new
//! one is ready, volumes are kept, and the new container has the old one's command line and
//! mounts. In this order, one report line each:
//!
//! 1. The target version: `--version`, else the newest published release of hostkeeper's
//!    repository tagged `vX.Y.Z`.
//! 2. The container `--container` must exist and run (a stopped one is never started), and no
//!    container `<name>-next` may exist (an earlier run was cut short).
//! 3. The running version: the coordinator's `/v1/version`, or for an older coordinator that has
//!    no such route the version in its image's `hostkeeper-coordinator:<version>` tag. A running
//!    version at least the target ends the run: up to date, nothing changed. A tag that names no
//!    version is an unknown version, and the update goes on; a `/v1/version` that gives no answer
//!    it can read (a wrong CA, an outage) refuses the run, since the same probe judges the new
//!    container.
//! 4. A marker `failed-v<target>` in `--download-dir` (a run that rolled back) refuses the
//!    version until the operator removes it.
//! 5. The image tarball, its bundle and `SHA256SUMS` are downloaded to `v<target>/`; the tarball
//!    must be the sum `SHA256SUMS` lists and its attestation by the release signer, of the
//!    release's commit, must verify online and from the bundle. Nothing is loaded otherwise.
//! 6. The manifest and config digests the tarball names are read from it.
//! 7. The tarball is loaded; the id of `hostkeeper-coordinator:<target>` must be one of them.
//! 8. The new container's create body is [`rebase::create_body`].
//! 9. `<name>-next` is created, the old container stopped, the new one started and polled until
//!    it is ready (running, not restarting, restart count 0, and the coordinator answers
//!    `?at_least=<target>`); then the old container is deleted (never with its volumes) and the
//!    new one renamed. A new container that is not ready is stopped and deleted, the old one is
//!    started again, and the version is marked failed. A docker call that fails while the new
//!    container is checked is one check that is not ready. Only a successful update removes old
//!    downloads, keeping the newest two `v*` directories and the one just installed.

mod engine;
mod rebase;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_engine;
#[cfg(test)]
mod tests_fake;
#[cfg(test)]
mod tests_rebase;

use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use serde_json::Value;

use crate::release::check::{judge, sums_line};
use crate::release::compare_images::tar_files;
use crate::release::contract::{
    COORDINATOR, Commit, Platform, Repo, SUMS, Signer, Version, bundle_of,
};
use crate::release::image_index::image_name;
use crate::release::ports::{Exec, PortError, Release, ReleaseSource, Verifier};
use crate::release::{self, file_sha256};
use engine::{CurlEngine, CurlProbe, Engine, Probe, Probed};

/// The platform of this build: the Linux platform of `std::env::consts::ARCH`.
const DEFAULT_PLATFORM: &str = match std::env::consts::ARCH.as_bytes() {
    b"aarch64" => "linux-arm64",
    _ => "linux-x86_64",
};

/// How many lines of a new container's log a failed update reports.
const LOG_LINES: u32 = 50;

/// The start of every report line.
const PREFIX: &str = "coordinator-update";

/// The scratch directory of a download in `v<version>/`: a download lands here and is renamed
/// into place whole, so a run that was killed never leaves a half file that later runs trust.
const PART: &str = ".part";

/// The flags of `coordinator-update`.
#[derive(Debug, clap::Args)]
pub(crate) struct Flags {
    /// The coordinator's container.
    #[arg(long, default_value = "hostkeeper-coordinator", value_parser = container_name)]
    pub(crate) container: String,
    /// Where release files are kept, in `v<version>/`; it must exist. A version that did not
    /// become ready leaves `failed-v<version>` here, whose text says why.
    #[arg(long)]
    pub(crate) download_dir: PathBuf,
    /// Docker's unix socket.
    #[arg(long, default_value = "/var/run/docker.sock")]
    pub(crate) docker_socket: PathBuf,
    /// The coordinator's private listener, `https://<host>[:<port>]`, whose `/v1/version`
    /// says which version runs.
    #[arg(long, value_parser = https_url)]
    pub(crate) coordinator_url: String,
    /// The CA its certificate chains to.
    #[arg(long)]
    pub(crate) ca_file: PathBuf,
    /// The image to load: `linux-x86_64` or `linux-arm64` (default: this build's).
    #[arg(long, default_value = DEFAULT_PLATFORM, value_parser = linux_platform)]
    pub(crate) platform: Platform,
    /// The version to run, `X.Y.Z` (default: the newest published release).
    #[arg(long, value_parser = parse_version)]
    pub(crate) version: Option<Version>,
    /// How many times to check that the new container is ready, at most.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) ready_checks: u32,
    /// The seconds between two readiness checks.
    #[arg(long, default_value_t = 5)]
    pub(crate) check_every_secs: u64,
    /// The seconds the old container gets to stop before it is killed.
    #[arg(long, default_value_t = 30)]
    pub(crate) stop_timeout_secs: u64,
    /// The `curl` program.
    #[arg(long, default_value = "curl")]
    pub(crate) curl: String,
}

/// A container name as docker takes it: letters, digits, `_`, `.` and `-`, not starting with
/// one of the last three.
fn container_name(text: &str) -> Result<String, String> {
    let name_char = |b: u8| b.is_ascii_alphanumeric() | matches!(b, b'_' | b'.' | b'-');
    let first = text
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric());
    if first & text.bytes().all(name_char) & (text.len() <= 64) {
        Ok(text.to_owned())
    } else {
        Err(format!(
            "not a container name: {text:?} (letters, digits, _ . -, at most 64, starting with a letter or digit)"
        ))
    }
}

/// `https://<host>[:<port>]` and nothing more.
fn https_url(text: &str) -> Result<String, String> {
    let bad = |why: &str| format!("not an https://<host>[:<port>] URL: {text:?} ({why})");
    let rest = text
        .strip_prefix("https://")
        .ok_or_else(|| bad("it must start with https://"))?;
    let host_char =
        |b: u8| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'-' | b':' | b'[' | b']');
    if rest.is_empty() | !rest.bytes().all(host_char) {
        return Err(bad("only a host and a port, no path, user or query"));
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') && !port.contains(']') => (host, Some(port)),
        _ => (rest, None),
    };
    let port_ok = port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p > 0));
    if host.is_empty() | !port_ok {
        return Err(bad("the host is empty or the port is not 1 to 65535"));
    }
    Ok(text.to_owned())
}

/// A Linux platform by name.
fn linux_platform(text: &str) -> Result<Platform, String> {
    Platform::of_name(text)
        .filter(|p| p.linux_arches().is_some())
        .ok_or_else(|| {
            format!(
                "not a platform of the coordinator's image: {text:?} (linux-x86_64 or linux-arm64)"
            )
        })
}

fn parse_version(text: &str) -> Result<Version, String> {
    text.parse::<Version>().map_err(|e| e.to_string())
}

/// Why the update did not happen, or did not finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// The run refuses before it changes anything.
    #[error("{0}")]
    Refused(String),
    /// A change was begun and did not finish; the text says what stands.
    #[error("{0}")]
    Failed(String),
    /// Docker could not answer.
    #[error(transparent)]
    Engine(#[from] PortError),
    /// A file could not be read or written.
    #[error(transparent)]
    Release(#[from] release::Error),
}

/// What waits between two readiness checks.
pub(crate) trait Pause {
    fn pause(&self, secs: u64);
}

/// The real pause: `sleep` for the seconds asked (the run passes [`std::thread::sleep`]).
pub(crate) struct Sleep<F: Fn(std::time::Duration)>(pub(crate) F);

impl<F: Fn(std::time::Duration)> Pause for Sleep<F> {
    fn pause(&self, secs: u64) {
        (self.0)(std::time::Duration::from_secs(secs));
    }
}

/// What the update runs against.
pub(crate) struct Ports<'a> {
    pub(crate) source: &'a dyn ReleaseSource,
    pub(crate) verifier: &'a dyn Verifier,
    pub(crate) engine: &'a dyn Engine,
    pub(crate) probe: &'a dyn Probe,
    pub(crate) pause: &'a dyn Pause,
}

/// The verb: `flags` over the real `curl`, through `exec`, and the release adapters.
pub(crate) fn run(
    flags: &Flags,
    source: &dyn ReleaseSource,
    verifier: &dyn Verifier,
    exec: &dyn Exec,
) -> Result<Vec<String>, Error> {
    let dir = &flags.download_dir;
    if !std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Err(Error::Refused(format!(
            "--download-dir {}: not a directory",
            dir.display()
        )));
    }
    let engine = CurlEngine::new(exec, &flags.curl, &flags.docker_socket, dir);
    let probe = CurlProbe::new(exec, &flags.curl, &flags.coordinator_url, &flags.ca_file);
    let ports = Ports {
        source,
        verifier,
        engine: &engine,
        probe: &probe,
        pause: &Sleep(std::thread::sleep),
    };
    update(&ports, flags)
}

fn said(text: impl std::fmt::Display) -> String {
    format!("{PREFIX}: {text}")
}

/// The string `key` of the JSON `value`, or the refusal that it is not there.
fn field<'v>(value: &'v Value, path: &[&str], what: &str) -> Result<&'v str, Error> {
    let found = path.iter().fold(value, |v, key| &v[*key]);
    found
        .as_str()
        .ok_or_else(|| Error::Refused(format!("{what} has no {}", path.join("."))))
}

/// One update (see the module).
pub(crate) fn update(ports: &Ports<'_>, flags: &Flags) -> Result<Vec<String>, Error> {
    let mut lines = Vec::new();
    let name = flags.container.as_str();
    let (target, release) = choose(ports.source, flags.version)?;
    lines.push(said(format!("target v{target}")));

    let old = inspect_old(ports.engine, name)?;
    let old_id = field(&old, &["Id"], "the container")?.to_owned();
    let old_image = ports
        .engine
        .image(field(&old, &["Image"], "the container")?)?;
    let running = running_version(ports.probe, old_image.as_ref())?;
    let was = running.map_or_else(|| "an unknown version".to_owned(), |v| format!("v{v}"));
    lines.push(said(format!("{name} runs {was}")));
    if running.is_some_and(|r| r >= target) {
        lines.push(said(format!(
            "up to date: {name} runs {was}, which is at least v{target}; nothing changed"
        )));
        return Ok(lines);
    }

    let Some(old_image) = old_image else {
        return Err(Error::Refused(format!(
            "the image of {name} is not in docker, so what it supplies cannot be told from what \
             the container sets"
        )));
    };
    let dir = flags.download_dir.join(format!("v{target}"));
    let marker = flags.download_dir.join(format!("failed-v{target}"));
    if let Ok(why) = std::fs::read_to_string(&marker) {
        return Err(Error::Refused(format!(
            "{} says v{target} did not become ready on an earlier run: {}; remove it to try v{target} again",
            marker.display(),
            why.trim()
        )));
    }
    let tar = fetch(ports, flags, &release, target, &dir)?;
    lines.push(said(format!(
        "{} is the release's image: it matches {SUMS} and its attestation verified online and from its bundle",
        tar.display()
    )));
    let (manifest, config) = image_ids(&tar)?;
    lines.push(said(format!(
        "the tarball's manifest is {manifest}, its config {config}"
    )));
    let new_id = load(ports.engine, &tar, target, (&manifest, &config))?;
    lines.push(said(format!("docker loaded {new_id}")));

    let body = rebase::create_body(&old, &old_image, &new_id).map_err(Error::Refused)?;
    replace(ports, flags, (&old_id, &body), (target, &was))?;
    lines.push(said(format!(
        "{name} now runs v{target} (was {was}); its command line, mounts and volumes are the old container's"
    )));
    lines.extend(prune(&flags.download_dir, target)?);
    Ok(lines)
}

/// The version to install and its release: `wanted`, else the newest published one.
fn choose(
    source: &dyn ReleaseSource,
    wanted: Option<Version>,
) -> Result<(Version, Release), Error> {
    let repo = Repo::hostkeeper();
    let releases = source.list(&repo)?;
    let published = releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| Some((Version::of_tag(&r.tag)?, r)));
    let chosen = match wanted {
        Some(v) => published.into_iter().find(|(tag, _)| *tag == v),
        None => published.max_by_key(|(tag, _)| *tag),
    };
    chosen.ok_or_else(|| {
        Error::Refused(match wanted {
            Some(v) => format!("{repo} has no published release v{v}"),
            None => format!("{repo} has no published release"),
        })
    })
}

/// The inspect of the running container `name`, refusing when it is absent or stopped, or when
/// `<name>-next` exists.
fn inspect_old(engine: &dyn Engine, name: &str) -> Result<Value, Error> {
    let Some(old) = engine.container(name)? else {
        return Err(Error::Refused(format!(
            "no container {name} on this machine: deploy the coordinator first"
        )));
    };
    if old["State"]["Running"] != true {
        return Err(Error::Refused(format!(
            "container {name} is not running: this verb rolls a running coordinator forward and never starts a stopped one"
        )));
    }
    let next = format!("{name}-next");
    if engine.container(&next)?.is_some() {
        return Err(Error::Refused(format!(
            "container {next} exists: an earlier run was cut short; read its logs (docker logs {next}) and remove it (docker rm {next})"
        )));
    }
    Ok(old)
}

/// The version the coordinator runs: its `/v1/version`; for one that has no such route, the
/// version in its image's tag; none when neither says.
///
/// # Errors
/// A refusal when `/v1/version` cannot be read (no answer, another status): the same probe
/// decides whether a new container is ready, so a run that went on would stop a coordinator
/// that may well be up to date and then roll it back. Nothing is changed.
fn running_version(probe: &dyn Probe, image: Option<&Value>) -> Result<Option<Version>, Error> {
    match probe.version(None) {
        Probed::Running(v) => Ok(Some(v)),
        Probed::NotFound => Ok(image.and_then(|image| {
            image["RepoTags"]
                .as_array()?
                .iter()
                .filter_map(|t| {
                    t.as_str()?
                        .strip_prefix(&format!("{COORDINATOR}:"))?
                        .parse()
                        .ok()
                })
                .max()
        })),
        Probed::Older => Ok(None),
        Probed::Failed(why) => Err(Error::Refused(format!(
            "the coordinator's version cannot be read ({why}): check --coordinator-url and --ca-file; nothing changed"
        ))),
    }
}

/// Downloads the image tarball, its bundle and `SHA256SUMS` into `dir` (a file already there is
/// kept) and checks them; the tarball's path.
fn fetch(
    ports: &Ports<'_>,
    flags: &Flags,
    release: &Release,
    target: Version,
    dir: &Path,
) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(dir).map_err(release::Error::io(format!("make {}", dir.display())))?;
    let name = image_name(&target, flags.platform);
    let repo = Repo::hostkeeper();
    for asset in [name.clone(), bundle_of(&name), SUMS.to_owned()] {
        let to = dir.join(&asset);
        if to.exists() {
            continue;
        }
        let part = dir.join(PART);
        let _ = std::fs::remove_dir_all(&part);
        std::fs::create_dir(&part)
            .map_err(release::Error::io(format!("make {}", part.display())))?;
        ports
            .source
            .download(&repo, &release.tag, &asset, &part)
            .map_err(|e| Error::Refused(format!("download {asset} of {}: {e}", release.tag)))?;
        std::fs::rename(part.join(&asset), &to).map_err(release::Error::io(format!(
            "move {asset} into {}",
            dir.display()
        )))?;
        let _ = std::fs::remove_dir_all(&part);
    }
    let tar = dir.join(&name);
    let digest = file_sha256(&tar)?;
    let sums_path = dir.join(SUMS);
    let sums = std::fs::read_to_string(&sums_path)
        .map_err(release::Error::io(format!("read {}", sums_path.display())))?;
    let listed = sums.lines().filter_map(sums_line).find(|(_, n)| *n == name);
    match listed {
        None => return Err(Error::Refused(format!("{SUMS} does not list {name}"))),
        Some((want, _)) if want != digest => {
            return Err(Error::Refused(format!(
                "{name}: its SHA-256 is {digest}, {SUMS} says {want}"
            )));
        }
        Some(_) => {}
    }
    let commit = Commit::from_str(&release.commit).map_err(|e| Error::Refused(e.to_string()))?;
    let signer = Signer::hostkeeper();
    let bundle = dir.join(bundle_of(&name));
    let mut problems = Vec::new();
    for (form, bundle) in [("online", None), ("offline", Some(bundle.as_path()))] {
        judge(
            &format!("{name} ({form})"),
            ports.verifier.verify(&signer, &tar, bundle, &commit),
            &digest,
            (&signer, &commit),
            &mut problems,
        );
    }
    if problems.is_empty() {
        Ok(tar)
    } else {
        Err(Error::Refused(problems.join("\n")))
    }
}

/// The manifest digest the tarball's `index.json` names, and the config digest that manifest
/// names.
fn image_ids(tar: &Path) -> Result<(String, String), Error> {
    let name = tar.display();
    let bytes = std::fs::read(tar).map_err(release::Error::io(format!("read {name}")))?;
    let refuse = |why: &str| Error::Refused(format!("{name}: {why}"));
    let files = tar_files(&bytes).map_err(|why| refuse(&why))?;
    let json = |path: &str| -> Result<Value, Error> {
        let bytes = files
            .get(path)
            .ok_or_else(|| refuse(&format!("{path} is missing")))?;
        serde_json::from_slice(bytes).map_err(|e| refuse(&format!("{path} is not JSON: {e}")))
    };
    let digest_of = |value: &Value, what: &str| -> Result<String, Error> {
        let digest = value
            .as_str()
            .ok_or_else(|| refuse(&format!("{what} names no digest")))?;
        Ok(digest.to_owned())
    };
    let index = json("index.json")?;
    let manifest = digest_of(&index["manifests"][0]["digest"], "index.json")?;
    let blob = |digest: &str| format!("blobs/sha256/{}", digest.trim_start_matches("sha256:"));
    let config = json(&blob(&manifest))?;
    let config = digest_of(&config["config"]["digest"], "the manifest")?;
    Ok((manifest, config))
}

/// Loads the tarball and returns the id of `hostkeeper-coordinator:<target>`, which must be the
/// manifest digest or the config digest (the containerd image store reports one, the classic
/// store the other).
fn load(
    engine: &dyn Engine,
    tar: &Path,
    target: Version,
    (manifest, config): (&str, &str),
) -> Result<String, Error> {
    engine.load(tar)?;
    let reference = format!("{COORDINATOR}:{target}");
    let loaded = engine.image(&reference)?;
    let id = loaded.as_ref().and_then(|i| i["Id"].as_str());
    match id {
        Some(id) if (id == manifest) | (id == config) => Ok(id.to_owned()),
        _ => Err(Error::Refused(format!(
            "the image docker loaded is not the verified one: {reference} is {}, the tarball's manifest is {manifest} and its config {config}",
            id.unwrap_or("not there")
        ))),
    }
}

/// Creates the new container, swaps it for the old and waits for it; on a failure puts the old
/// back.
fn replace(
    ports: &Ports<'_>,
    flags: &Flags,
    (old_id, body): (&str, &Value),
    (target, was): (Version, &str),
) -> Result<(), Error> {
    let (engine, name) = (ports.engine, flags.container.as_str());
    let next = format!("{name}-next");
    let new_id = engine.create(&next, body)?;
    if let Err(e) = engine.stop(old_id, flags.stop_timeout_secs) {
        let undone = match engine.remove(&new_id) {
            Ok(()) => format!("{next} was deleted and {name} was left as it was"),
            Err(e) => format!("{next} could not be deleted either ({e}): delete it by hand"),
        };
        return Err(Error::Failed(format!(
            "{name} could not be stopped: {e}; {undone}"
        )));
    }
    let why = match engine.start(&new_id) {
        Err(e) => format!("v{target}: starting {next} failed: {e}"),
        Ok(()) if ready(ports, flags, &new_id, target) => {
            return finish(engine, (old_id, &new_id), (name, target, was));
        }
        Ok(()) => format!(
            "v{target} did not become ready within {} checks",
            flags.ready_checks
        ),
    };
    Err(roll_back(ports, flags, (old_id, &new_id), target, &why))
}

/// Polls the new container, at most `ready_checks` times with a pause between.
fn ready(ports: &Ports<'_>, flags: &Flags, id: &str, target: Version) -> bool {
    for check in 0..flags.ready_checks {
        if check > 0 {
            ports.pause.pause(flags.check_every_secs);
        }
        // A docker call that fails is one check that is not ready, never an end of the run: the
        // old container is stopped by now, and only the roll back starts it again.
        let state = ports
            .engine
            .container(id)
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        let up = (state["State"]["Running"] == true)
            & (state["State"]["Restarting"] != true)
            & (state["RestartCount"] == 0);
        if up && matches!(ports.probe.version(Some(&target)), Probed::Running(_)) {
            return true;
        }
    }
    false
}

/// The new container is ready: the old one goes (its volumes stay) and the new one takes its name.
fn finish(
    engine: &dyn Engine,
    (old_id, new_id): (&str, &str),
    (name, target, was): (&str, Version, &str),
) -> Result<(), Error> {
    let next = format!("{name}-next");
    engine.remove(old_id).map_err(|e| {
        Error::Failed(format!(
            "{next} runs v{target} and is ready, but the old container {old_id} could not be deleted: {e}; delete it (docker rm {old_id}) and rename {next} to {name}"
        ))
    })?;
    engine.rename(new_id, name).map_err(|e| {
        Error::Failed(format!(
            "{next} runs v{target} (was {was}) and the old container is deleted, but {next} could not be renamed to {name}: {e}; rename it (docker rename {next} {name})"
        ))
    })
}

/// The new container did not come up: its last log lines are kept for the error, it is stopped
/// and deleted, the old container is started again and checked, and the version is marked failed.
fn roll_back(
    ports: &Ports<'_>,
    flags: &Flags,
    (old_id, new_id): (&str, &str),
    target: Version,
    why: &str,
) -> Error {
    let (engine, name) = (ports.engine, flags.container.as_str());
    let next = format!("{name}-next");
    let logs = engine
        .logs(new_id, LOG_LINES)
        .unwrap_or_else(|e| format!("(the log could not be read: {e})"));
    // A container that is already stopped or gone is what is wanted; a delete that fails is said.
    let _ = engine.stop(new_id, flags.stop_timeout_secs);
    let deleted = engine.remove(new_id);
    let restarted = engine.start(old_id).and_then(|()| {
        let up = engine.container(old_id)?;
        if up.is_some_and(|c| c["State"]["Running"] == true) {
            Ok(())
        } else {
            Err(PortError::new(
                format!("docker start {old_id}"),
                "it started and is not running",
            ))
        }
    });
    let outcome = match &restarted {
        Ok(()) => "the old container runs again".to_owned(),
        Err(e) => format!(
            "the old container {old_id} could not be started again ({e}): {name} IS DOWN; start it (docker start {old_id}), or deploy the coordinator by hand"
        ),
    };
    let headline = format!("{why}: {outcome}");
    let marker = flags.download_dir.join(format!("failed-v{target}"));
    let marked = std::fs::write(&marker, format!("{headline}\n"));
    let mut text = format!(
        "{headline}\nthe last log lines of {next}:\n{}",
        logs.trim_end()
    );
    if let Err(e) = deleted {
        text.push_str(&format!(
            "\n{next} could not be deleted: {e}; delete it (docker rm -f {next}) before the next run"
        ));
    }
    if let Err(e) = marked {
        text.push_str(&format!("\n{} could not be written: {e}", marker.display()));
    }
    Error::Failed(text)
}

/// Removes the `v<version>` directories of `dir` but the newest two and that of `installed`
/// (the version just installed, which an older `--version` may have been); the lines it reports.
fn prune(dir: &Path, installed: Version) -> Result<Vec<String>, Error> {
    let what = format!("read {}", dir.display());
    let mut kept: Vec<(Version, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(release::Error::io(&what))? {
        let entry = entry.map_err(release::Error::io(&what))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir()
            && let Some(version) = Version::of_tag(&name)
        {
            kept.push((version, entry.path()));
        }
    }
    kept.sort();
    let old = kept.len().saturating_sub(2);
    let mut lines = Vec::new();
    for (version, path) in kept[..old].iter().filter(|(v, _)| *v != installed) {
        std::fs::remove_dir_all(path)
            .map_err(release::Error::io(format!("remove {}", path.display())))?;
        lines.push(said(format!("removed the downloads of v{version}")));
    }
    Ok(lines)
}
