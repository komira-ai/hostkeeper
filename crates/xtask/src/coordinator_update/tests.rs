//! The tests of the `coordinator-update` steps, against Docker, the coordinator and the release
//! held in memory (`tests_fake`).

use std::path::PathBuf;

use clap::Parser as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::engine::Probed;
use super::tests_fake::FakeSource;
use super::tests_fake::{FakeEngine, FakePause, FakeProbe, NEW_ID, NewBehaviour, OLD_ID, ports};
use super::*;
use crate::release::fakes::{FakeVerifier, good_attestation, nth_commit, scratch};
use crate::release::image_index::{fake_image, image_name};
use crate::release::ports::PortError;
use crate::testdir::TestDir;

const NAME: &str = "hostkeeper-coordinator";
const NEXT: &str = "hostkeeper-coordinator-next";

fn v(text: &str) -> Version {
    text.parse().expect("a version")
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The id a classic image store gives the fake image: its config digest.
fn config_id() -> String {
    format!(
        "sha256:{}",
        sha(br#"{"architecture":"amd64","marker":"m"}"#)
    )
}

/// The manifest digest the fake image's `index.json` names, read with the tar crate.
fn manifest_id() -> String {
    let bytes = image_tar();
    let mut archive = tar::Archive::new(bytes.as_slice());
    for entry in archive.entries().expect("entries") {
        let entry = entry.expect("entry");
        if entry.path().expect("path").to_str() == Some("index.json") {
            let index: Value = serde_json::from_reader(entry).expect("index.json");
            return index["manifests"][0]["digest"]
                .as_str()
                .expect("digest")
                .to_owned();
        }
    }
    panic!("no index.json");
}

fn commit() -> String {
    nth_commit(7).to_string()
}

fn image_tar() -> Vec<u8> {
    fake_image("amd64", "m")
}

/// Publishes the release `tag` with the image tarball `tar` and the sums that list it.
fn publish(source: &FakeSource, tag: &str, draft: bool, tar: Vec<u8>) {
    let version = Version::of_tag(tag).unwrap_or(v("0.0.0"));
    let name = image_name(&version, Platform::LinuxX86_64);
    let sums = format!("{}  {name}\n{}  other\n", sha(&tar), sha(b"other"));
    source.add(
        "komira-ai/hostkeeper",
        tag,
        &commit(),
        draft,
        vec![
            (name.clone(), tar),
            (bundle_of(&name), b"{}".to_vec()),
            (SUMS.to_owned(), sums.into_bytes()),
        ],
    );
}

/// The inspect of the running coordinator container of the tests.
pub(super) fn old_container() -> Value {
    json!({
        "Id": OLD_ID,
        "Image": "sha256:oldimage",
        "Config": {
            "Hostname": "abc123",
            "Domainname": "",
            "User": "65532",
            "Image": "hostkeeper-coordinator:0.1.218",
            "Entrypoint": ["/hostkeeper-coordinator"],
            "Cmd": ["serve", "--listen", "192.0.2.5:8444", "--database-dsn-file", "/run/secrets/db-dsn"],
            "WorkingDir": "/",
            "Env": ["PATH=/usr/local/bin", "TZ=UTC", "SSL_CERT_FILE=/etc/ssl/ca.pem"],
            "Labels": {
                "org.opencontainers.image.version": "0.1.218",
                "com.docker.compose.service": "coordinator"
            }
        },
        "HostConfig": {
            "Binds": [
                "/srv/hostkeeper-coordinator/etc:/etc/hostkeeper-coordinator:ro",
                "hostkeeper-coordinator_hostkeeper-coordinator:/var/lib/hostkeeper-coordinator"
            ],
            "RestartPolicy": {"Name": "unless-stopped", "MaximumRetryCount": 0},
            "NetworkMode": "host"
        },
        "Mounts": [
            {"Type": "bind", "Source": "/srv/hostkeeper-coordinator/etc",
             "Destination": "/etc/hostkeeper-coordinator", "RW": false},
            {"Type": "volume", "Name": "hostkeeper-coordinator_hostkeeper-coordinator",
             "Destination": "/var/lib/hostkeeper-coordinator", "RW": true}
        ],
        "NetworkSettings": {"Networks": {"host": {}}}
    })
}

/// The image the old container runs.
pub(super) fn old_image() -> Value {
    json!({
        "Id": "sha256:oldimage",
        "RepoTags": ["hostkeeper-coordinator:0.1.218"],
        "Config": {
            "User": "65532",
            "Entrypoint": ["/hostkeeper-coordinator"],
            "Cmd": ["serve"],
            "WorkingDir": "/",
            "Env": ["PATH=/usr/local/bin", "SSL_CERT_FILE=/etc/ssl/ca.pem"],
            "Labels": {"org.opencontainers.image.version": "0.1.218"}
        }
    })
}

/// Everything one update runs against: a machine with the old coordinator running, and a
/// published release 0.1.219 (and older, draft and foreign ones).
struct World {
    _root: TestDir,
    download: PathBuf,
    source: FakeSource,
    verifier: FakeVerifier,
    engine: FakeEngine,
    probe: FakeProbe,
    pause: FakePause,
}

fn world(name: &str) -> World {
    let root = scratch(name);
    let download = root.join("downloads");
    std::fs::create_dir(&download).expect("the download directory");
    let source = FakeSource::default();
    publish(&source, "v0.1.218", false, image_tar());
    publish(&source, "v0.1.219", false, image_tar());
    publish(&source, "v0.1.220", true, image_tar());
    source.publish("komira-ai/hostkeeper", "nightly", &commit(), Vec::new());
    let engine = FakeEngine::default();
    engine.containers.borrow_mut().push(tests_fake::Container {
        id: OLD_ID.to_owned(),
        name: NAME.to_owned(),
        inspect: old_container(),
        running: true,
        restarting: false,
        restart_count: 0,
        logs: "the old coordinator".to_owned(),
    });
    engine
        .images
        .borrow_mut()
        .insert("sha256:oldimage".to_owned(), old_image());
    *engine.loads_as.borrow_mut() =
        Some(("hostkeeper-coordinator:0.1.219".to_owned(), config_id()));
    World {
        _root: root,
        download,
        source,
        verifier: FakeVerifier::good(&nth_commit(7)),
        engine,
        probe: FakeProbe::new(
            Probed::Running(v("0.1.218")),
            vec![
                Probed::Failed("not yet".to_owned()),
                Probed::Running(v("0.1.219")),
            ],
        ),
        pause: FakePause::default(),
    }
}

impl World {
    fn flags(&self) -> Flags {
        Flags {
            container: NAME.to_owned(),
            download_dir: self.download.clone(),
            docker_socket: PathBuf::from("/run/docker.sock"),
            coordinator_url: "https://coordinator.example.com:8444".to_owned(),
            ca_file: PathBuf::from("/etc/ca.pem"),
            platform: Platform::LinuxX86_64,
            version: None,
            ready_checks: 3,
            check_every_secs: 5,
            stop_timeout_secs: 30,
            curl: "curl".to_owned(),
        }
    }

    fn update_with(&self, flags: &Flags) -> Result<Vec<String>, Error> {
        let p = ports(
            &self.source,
            &self.verifier,
            &self.engine,
            &self.probe,
            &self.pause,
        );
        update(&p, flags)
    }

    fn update(&self) -> Result<Vec<String>, Error> {
        self.update_with(&self.flags())
    }

    fn refused(&self) -> String {
        self.update()
            .expect_err("the update is refused")
            .to_string()
    }

    fn loaded(&self) -> bool {
        self.engine.calls().iter().any(|c| c.starts_with("load "))
    }

    fn marker(&self) -> Option<String> {
        std::fs::read_to_string(self.download.join("failed-v0.1.219")).ok()
    }
}

fn dir_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_ready_update_replaces_the_container_and_reports_each_step() {
    // Proves: the newest published release (not the draft, not a tag that is no version) is the
    // target; the old container is stopped only after the new one is created, the new one is
    // checked until ready with a pause between checks, the old one is deleted without its volumes
    // and the new one takes its name; the downloads beyond the newest two versions are removed.
    let w = world("cu-ready");
    for old in ["v0.1.215", "v0.1.217", "v0.1.218"] {
        std::fs::create_dir(w.download.join(old)).expect("an old download");
    }
    std::fs::write(w.download.join("failed-v0.1.216"), "x").expect("an old marker");
    let tar = image_name(&v("0.1.219"), Platform::LinuxX86_64);
    let lines = w.update().expect("the update");
    let dir = w.download.join("v0.1.219");
    assert_eq!(
        lines,
        vec![
            "coordinator-update: target v0.1.219".to_owned(),
            "coordinator-update: hostkeeper-coordinator runs v0.1.218".to_owned(),
            format!(
                "coordinator-update: {} is the release's image: it matches SHA256SUMS and its attestation verified online and from its bundle",
                dir.join(&tar).display()
            ),
            format!(
                "coordinator-update: the tarball's manifest is {}, its config {}",
                manifest_id(),
                config_id()
            ),
            format!("coordinator-update: docker loaded {}", config_id()),
            "coordinator-update: hostkeeper-coordinator now runs v0.1.219 (was v0.1.218); its command line, mounts and volumes are the old container's".to_owned(),
            "coordinator-update: removed the downloads of v0.1.215".to_owned(),
            "coordinator-update: removed the downloads of v0.1.217".to_owned(),
        ]
    );
    assert_eq!(
        w.engine.calls(),
        vec![
            format!("inspect {NAME}"),
            format!("inspect {NEXT}"),
            "image sha256:oldimage".to_owned(),
            format!("load {tar}"),
            "image hostkeeper-coordinator:0.1.219".to_owned(),
            format!("create {NEXT}"),
            format!("stop {OLD_ID} 30"),
            format!("start {NEW_ID}"),
            format!("inspect {NEW_ID}"),
            format!("inspect {NEW_ID}"),
            format!("remove {OLD_ID}"),
            format!("rename {NEW_ID} {NAME}"),
        ]
    );
    assert_eq!(*w.pause.waits.borrow(), vec![5]);
    assert_eq!(
        *w.probe.calls.borrow(),
        vec![None, Some(v("0.1.219")), Some(v("0.1.219"))]
    );
    assert_eq!(
        dir_names(&w.download),
        ["failed-v0.1.216", "v0.1.218", "v0.1.219"].map(str::to_owned)
    );
    assert!(w.engine.exists(NAME) && !w.engine.exists(NEXT) && !w.engine.exists(OLD_ID));
    // The release's files are the ones the attestation was asked about, online and offline.
    assert_eq!(
        *w.verifier.calls.borrow(),
        vec![
            format!("online {tar}"),
            format!("offline {tar} {tar}.sigstore.json")
        ]
    );
    assert_eq!(
        *w.verifier.signers.borrow(),
        vec![Signer::hostkeeper().workflow; 2]
    );
    assert_eq!(dir_names(&dir).len(), 3);
}

#[test]
fn the_new_container_is_created_with_the_rebased_body_before_the_old_one_is_stopped() {
    // Proves: the create body is the rebase of the old container on the loaded image (the
    // command line and mounts are the old container's, the image the new id), under the name
    // `<name>-next`.
    let w = world("cu-body");
    w.update().expect("the update");
    let created = w.engine.created.borrow().clone();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].0, NEXT);
    let want = rebase::create_body(&old_container(), &old_image(), &config_id()).expect("a body");
    assert_eq!(created[0].1, want);
    assert_eq!(created[0].1["Cmd"], old_container()["Config"]["Cmd"]);
    assert_eq!(created[0].1["HostConfig"], old_container()["HostConfig"]);
    assert_eq!(created[0].1["Image"], config_id());
}

#[test]
fn a_coordinator_at_least_the_target_is_left_alone() {
    // Proves: a running version equal to the target, or newer than a `--version` older than it,
    // reports up to date and downloads, loads and changes nothing.
    for (running, version) in [("0.1.219", None), ("0.1.220", Some(v("0.1.219")))] {
        let mut w = world("cu-uptodate");
        w.probe = FakeProbe::new(Probed::Running(v(running)), Vec::new());
        let mut flags = w.flags();
        flags.version = version;
        let lines = w.update_with(&flags).expect("up to date");
        assert_eq!(
            lines,
            vec![
                "coordinator-update: target v0.1.219".to_owned(),
                format!("coordinator-update: hostkeeper-coordinator runs v{running}"),
                format!(
                    "coordinator-update: up to date: hostkeeper-coordinator runs v{running}, which is at least v0.1.219; nothing changed"
                ),
            ]
        );
        assert_eq!(
            w.engine.calls(),
            vec![
                format!("inspect {NAME}"),
                format!("inspect {NEXT}"),
                "image sha256:oldimage".to_owned()
            ]
        );
        assert!(w.source.downloads.borrow().is_empty());
        assert_eq!(dir_names(&w.download), Vec::<String>::new());
    }
}

#[test]
fn an_older_coordinator_without_the_route_is_known_by_its_images_tag() {
    // Proves: a 404 from /v1/version reads the version from the image's tag; with no such tag
    // the version is unknown and the update goes on.
    let cases = [
        (
            Probed::NotFound,
            json!([
                "x:1",
                "hostkeeper-coordinator:0.1.9",
                "hostkeeper-coordinator:0.1.218"
            ]),
            "v0.1.218",
        ),
        (
            Probed::NotFound,
            json!(["x:1", "hostkeeper-coordinator:latest"]),
            "an unknown version",
        ),
        (Probed::Older, json!([]), "an unknown version"),
    ];
    for (plain, tags, was) in cases {
        let mut w = world("cu-tags");
        w.probe = FakeProbe::new(plain, vec![Probed::Running(v("0.1.219"))]);
        let mut image = old_image();
        image["RepoTags"] = tags;
        w.engine
            .images
            .borrow_mut()
            .insert("sha256:oldimage".to_owned(), image);
        let lines = w.update().expect("the update");
        assert_eq!(
            lines[1],
            format!("coordinator-update: hostkeeper-coordinator runs {was}")
        );
        assert_eq!(
            lines[5],
            format!(
                "coordinator-update: hostkeeper-coordinator now runs v0.1.219 (was {was}); its command line, mounts and volumes are the old container's"
            )
        );
    }
}

#[test]
fn a_version_route_that_cannot_be_read_refuses_the_run_and_changes_nothing() {
    // Proves: when /v1/version gives no answer it can read (a wrong CA, an outage), the run stops
    // before anything is downloaded or stopped: the same probe judges the new container, so going
    // on would stop a coordinator that may be up to date and roll it back.
    let mut w = world("cu-unreadable");
    w.probe = FakeProbe::new(
        Probed::Failed("GET https://coordinator.example.com:8444/v1/version: HTTP 502".to_owned()),
        vec![Probed::Running(v("0.1.219"))],
    );
    assert_eq!(
        w.refused(),
        "the coordinator's version cannot be read (GET https://coordinator.example.com:8444/v1/version: HTTP 502): check --coordinator-url and --ca-file; nothing changed"
    );
    assert_eq!(
        w.engine.calls(),
        vec![
            format!("inspect {NAME}"),
            format!("inspect {NEXT}"),
            "image sha256:oldimage".to_owned(),
        ]
    );
    assert!(!w.loaded() && w.engine.is_running(NAME) && w.marker().is_none());
    assert!(dir_names(&w.download).is_empty());
}

#[test]
fn an_older_wanted_version_keeps_its_downloads_when_newer_ones_are_there() {
    // Proves: the version just installed is never pruned, even when two newer download
    // directories are there (an operator's `--version` of an older release).
    let mut w = world("cu-prune-installed");
    for newer in ["v0.1.230", "v0.1.231"] {
        std::fs::create_dir(w.download.join(newer)).expect("a newer download");
    }
    let mut flags = w.flags();
    flags.version = Some(v("0.1.218"));
    w.probe = FakeProbe::new(
        Probed::Running(v("0.1.100")),
        vec![Probed::Running(v("0.1.218"))],
    );
    w.engine
        .loads_as
        .borrow_mut()
        .replace(("hostkeeper-coordinator:0.1.218".to_owned(), config_id()));
    let lines = w.update_with(&flags).expect("the older release");
    assert!(
        !lines.iter().any(|l| l.contains("removed the downloads")),
        "{lines:?}"
    );
    assert_eq!(
        dir_names(&w.download),
        ["v0.1.218", "v0.1.230", "v0.1.231"].map(str::to_owned)
    );
}

#[test]
fn the_target_is_the_wanted_version_or_the_newest_published_release() {
    // Proves: `--version` picks that release, a draft or an unpublished one is no release, and a
    // repository with no release (or one that cannot be listed) refuses.
    let mut w = world("cu-choose");
    let mut flags = w.flags();
    flags.version = Some(v("0.1.220"));
    assert_eq!(
        w.update_with(&flags).expect_err("a draft").to_string(),
        "komira-ai/hostkeeper has no published release v0.1.220"
    );
    flags.version = Some(v("0.1.218"));
    w.probe = FakeProbe::new(
        Probed::Running(v("0.1.100")),
        vec![Probed::Running(v("0.1.218"))],
    );
    w.engine
        .loads_as
        .borrow_mut()
        .replace(("hostkeeper-coordinator:0.1.218".to_owned(), config_id()));
    let lines = w.update_with(&flags).expect("the older release");
    assert_eq!(lines[0], "coordinator-update: target v0.1.218");

    let empty = FakeSource::default();
    empty.add(
        "komira-ai/hostkeeper",
        "v0.1.5",
        &commit(),
        true,
        Vec::new(),
    );
    let w = world("cu-choose-none");
    let p = ports(&empty, &w.verifier, &w.engine, &w.probe, &w.pause);
    assert_eq!(
        update(&p, &w.flags()).expect_err("no release").to_string(),
        "komira-ai/hostkeeper has no published release"
    );
    let unlistable = FakeSource::default();
    let p = ports(&unlistable, &w.verifier, &w.engine, &w.probe, &w.pause);
    assert_eq!(
        update(&p, &w.flags()).expect_err("no list").to_string(),
        "list the releases of komira-ai/hostkeeper: HTTP 404"
    );
}

#[test]
fn the_container_must_exist_run_and_have_no_leftover_next() {
    // Proves: each refusal of step 2 names what is wrong and changes nothing.
    let w = world("cu-container");
    w.engine.containers.borrow_mut().clear();
    assert_eq!(
        w.refused(),
        "no container hostkeeper-coordinator on this machine: deploy the coordinator first"
    );

    let w = world("cu-stopped");
    w.engine.containers.borrow_mut()[0].running = false;
    assert_eq!(
        w.refused(),
        "container hostkeeper-coordinator is not running: this verb rolls a running coordinator forward and never starts a stopped one"
    );
    assert_eq!(w.engine.calls(), vec![format!("inspect {NAME}")]);

    let w = world("cu-next");
    let mut next = w.engine.containers.borrow()[0].clone();
    next.name = NEXT.to_owned();
    next.id = "other".to_owned();
    w.engine.containers.borrow_mut().push(next);
    assert_eq!(
        w.refused(),
        "container hostkeeper-coordinator-next exists: an earlier run was cut short; read its logs (docker logs hostkeeper-coordinator-next) and remove it (docker rm hostkeeper-coordinator-next)"
    );
    assert!(!w.loaded());

    let w = world("cu-noimage");
    w.engine.images.borrow_mut().clear();
    assert_eq!(
        w.refused(),
        "the image of hostkeeper-coordinator is not in docker, so what it supplies cannot be told from what the container sets"
    );
    assert!(w.source.downloads.borrow().is_empty());
}

#[test]
fn docker_errors_before_any_change_are_the_errors_of_the_run() {
    // Proves: an inspect that fails ends the run with docker's error, naming the call.
    let w = world("cu-inspect-fails");
    w.engine.failing(&format!("inspect {NAME}"));
    assert_eq!(
        w.refused(),
        "docker inspect hostkeeper-coordinator: HTTP 500: it failed"
    );
    let w = world("cu-image-fails");
    w.engine.failing("image sha256:oldimage");
    assert_eq!(
        w.refused(),
        "docker image sha256:oldimage: HTTP 500: it failed"
    );
}

#[test]
fn a_field_the_inspect_lacks_is_a_refusal() {
    assert_eq!(
        field(&json!({"Image": 3}), &["Image"], "the container")
            .expect_err("not a string")
            .to_string(),
        "the container has no Image"
    );
    assert_eq!(
        field(&json!({"A": {"B": "x"}}), &["A", "B"], "it").expect("found"),
        "x"
    );
}

#[test]
fn a_version_that_failed_before_stays_refused_until_its_marker_is_removed() {
    // Proves: the marker of the target version refuses the run, quoting its text; the marker of
    // another version does not.
    let w = world("cu-marker");
    std::fs::write(w.download.join("failed-v0.1.219"), "it crashed\n").expect("marker");
    assert_eq!(
        w.refused(),
        format!(
            "{} says v0.1.219 did not become ready on an earlier run: it crashed; remove it to try v0.1.219 again",
            w.download.join("failed-v0.1.219").display()
        )
    );
    assert!(w.source.downloads.borrow().is_empty());
    std::fs::remove_file(w.download.join("failed-v0.1.219")).expect("remove");
    std::fs::write(w.download.join("failed-v0.1.218"), "old news").expect("marker");
    w.update().expect("another version's marker does not count");
}

#[test]
fn a_release_that_does_not_check_out_is_refused_before_anything_is_loaded() {
    // Proves: a sum that is not the file's, a sums file that lacks the image, an attestation
    // refused online or only offline, one by another commit, a download that fails and a release
    // with no commit each refuse, with the problem named, and docker loads nothing.
    let tar = image_name(&v("0.1.219"), Platform::LinuxX86_64);
    let other_commit = nth_commit(8).to_string();
    let wrong = {
        let other = other_commit.clone();
        FakeVerifier::with(Box::new(move |file, _| {
            Ok(vec![good_attestation(file, &other)])
        }))
    };
    let offline_only = FakeVerifier::with(Box::new(|file, bundle| match bundle {
        None => Ok(vec![good_attestation(file, &nth_commit(7).to_string())]),
        Some(_) => Err(PortError::new("verify", "the bundle is not valid")),
    }));
    let never = FakeVerifier::with(Box::new(|_, _| {
        Err(PortError::new("verify", "no attestation"))
    }));
    let cases: Vec<(FakeVerifier, Option<Vec<u8>>, String)> = vec![
        (
            never,
            None,
            format!(
                "{tar} (online): verify: no attestation\n{tar} (offline): verify: no attestation"
            ),
        ),
        (
            offline_only,
            None,
            format!("{tar} (offline): verify: the bundle is not valid"),
        ),
        (
            wrong,
            None,
            format!(
                "{tar} (online): attests the commit {other_commit}, not the pin {}\n{tar} (offline): attests the commit {other_commit}, not the pin {}",
                commit(),
                commit()
            ),
        ),
    ];
    for (verifier, _, want) in cases {
        let mut w = world("cu-attest");
        w.verifier = verifier;
        assert_eq!(w.refused(), want);
        assert!(!w.loaded());
        assert!(w.engine.created.borrow().is_empty());
    }

    // A sum that is not the file's.
    let w = world("cu-sum");
    let wrong_sum = format!("{}  {tar}\n", "0".repeat(64));
    let dir = w.download.join("v0.1.219");
    std::fs::create_dir(&dir).expect("dir");
    std::fs::write(dir.join(SUMS), wrong_sum).expect("sums");
    let real = sha(&image_tar());
    assert_eq!(
        w.refused(),
        format!(
            "{tar}: its SHA-256 is {real}, SHA256SUMS says {}",
            "0".repeat(64)
        )
    );
    assert!(!w.loaded());

    // Sums that do not list the image.
    let w = world("cu-nosum");
    std::fs::create_dir(w.download.join("v0.1.219")).expect("dir");
    std::fs::write(
        w.download.join("v0.1.219").join(SUMS),
        format!("{}  other\nnot a line\n", "1".repeat(64)),
    )
    .expect("sums");
    assert_eq!(
        w.refused(),
        "SHA256SUMS does not list hostkeeper-coordinator-0.1.219-linux-x86_64.image.tar"
    );

    // A transfer cut short, and a release whose commit is not one.
    let w = world("cu-download");
    w.source.downloads_left.set(Some(1));
    assert_eq!(
        w.refused(),
        "download hostkeeper-coordinator-0.1.219-linux-x86_64.image.tar.sigstore.json of v0.1.219: download hostkeeper-coordinator-0.1.219-linux-x86_64.image.tar.sigstore.json of komira-ai/hostkeeper v0.1.219: the connection was reset"
    );
    assert!(!w.dir_has_partial_file());
    let w = world("cu-commit");
    let source = FakeSource::default();
    source.publish(
        "komira-ai/hostkeeper",
        "v0.1.219",
        "main",
        vec![
            (tar.clone(), image_tar()),
            (format!("{tar}.sigstore.json"), b"{}".to_vec()),
            (
                SUMS.to_owned(),
                format!("{}  {tar}\n", sha(&image_tar())).into_bytes(),
            ),
        ],
    );
    let p = ports(&source, &w.verifier, &w.engine, &w.probe, &w.pause);
    assert_eq!(
        update(&p, &w.flags())
            .expect_err("a bad commit")
            .to_string(),
        "not a commit: \"main\" (expected 40 lowercase hex digits)"
    );
}

impl World {
    /// Whether the partial-download directory of the target version is left behind with a file.
    fn dir_has_partial_file(&self) -> bool {
        let part = self.download.join("v0.1.219").join(PART);
        part.join("hostkeeper-coordinator-0.1.219-linux-x86_64.image.tar.sigstore.json")
            .exists()
    }
}

#[test]
fn files_already_downloaded_are_kept_and_checked_again() {
    // Proves: a run after an interrupted one downloads only what is missing and still checks the
    // files it finds; a stale partial download is discarded.
    let w = world("cu-resume");
    let name = image_name(&v("0.1.219"), Platform::LinuxX86_64);
    let dir = w.download.join("v0.1.219");
    std::fs::create_dir_all(dir.join(PART)).expect("partial");
    std::fs::write(dir.join(PART).join("stale"), "x").expect("stale");
    std::fs::write(dir.join(&name), image_tar()).expect("the tarball");
    w.update().expect("the update");
    assert_eq!(
        *w.source.downloads.borrow(),
        vec![
            format!("komira-ai/hostkeeper v0.1.219 {name}.sigstore.json"),
            "komira-ai/hostkeeper v0.1.219 SHA256SUMS".to_owned(),
        ]
    );
    assert!(!dir.join(PART).exists());

    let w = world("cu-resume-bad");
    std::fs::create_dir_all(dir_of(&w)).expect("dir");
    std::fs::write(dir_of(&w).join(&name), b"cut short").expect("a cut-short tarball");
    assert_eq!(
        w.refused(),
        format!(
            "{name}: its SHA-256 is {}, SHA256SUMS says {}",
            sha(b"cut short"),
            sha(&image_tar())
        )
    );
}

fn dir_of(w: &World) -> PathBuf {
    w.download.join("v0.1.219")
}

/// A tar of the files `(path, bytes)`.
fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, path, *data)
            .expect("append");
    }
    builder.into_inner().expect("tar")
}

#[test]
fn a_tarball_that_names_no_manifest_and_config_is_refused_before_loading() {
    // Proves: each way the OCI layout can fail to name the manifest digest and the config digest
    // is a refusal naming the tarball and the missing piece.
    let manifest = br#"{"config":{"digest":"sha256:cfg"}}"#;
    let manifest_blob = format!("blobs/sha256/{}", sha(manifest));
    let index = format!(
        r#"{{"manifests":[{{"digest":"sha256:{}"}}]}}"#,
        sha(manifest)
    );
    let junk = vec![b'x'; 700];
    let cases: Vec<(Vec<u8>, String)> = vec![
        (junk, "it is not a readable tar archive".to_owned()),
        (
            tar_of(&[("oci-layout", b"{}")]),
            "index.json is missing".to_owned(),
        ),
        (
            tar_of(&[("index.json", b"nope")]),
            "index.json is not JSON: expected ident at line 1 column 2".to_owned(),
        ),
        (
            tar_of(&[("index.json", b"{}")]),
            "index.json names no digest".to_owned(),
        ),
        (
            tar_of(&[("index.json", index.as_bytes())]),
            format!("{manifest_blob} is missing"),
        ),
        (
            tar_of(&[("index.json", index.as_bytes()), (&manifest_blob, b"{}")]),
            "the manifest names no digest".to_owned(),
        ),
    ];
    for (tar, why) in cases {
        let w = world("cu-tar");
        let name = image_name(&v("0.1.219"), Platform::LinuxX86_64);
        let source = FakeSource::default();
        publish(&source, "v0.1.219", false, tar);
        let p = ports(&source, &w.verifier, &w.engine, &w.probe, &w.pause);
        let err = update(&p, &w.flags()).expect_err("an unreadable image");
        assert_eq!(
            err.to_string(),
            format!(
                "{}: {why}",
                w.download.join("v0.1.219").join(&name).display()
            )
        );
        assert!(!w.loaded());
    }
}

#[test]
fn what_docker_loaded_must_be_the_verified_image() {
    // Proves: a load docker reports as failed, an image whose id is neither digest, and no image
    // under the tag each refuse; either digest is accepted (the classic and containerd stores).
    let w = world("cu-load-fails");
    let tar = image_name(&v("0.1.219"), Platform::LinuxX86_64);
    w.engine.failing(&format!("load {tar}"));
    assert_eq!(
        w.refused(),
        format!("docker load {tar}: HTTP 500: it failed")
    );
    assert!(w.engine.created.borrow().is_empty());

    let w = world("cu-wrong-id");
    *w.engine.loads_as.borrow_mut() = Some((
        "hostkeeper-coordinator:0.1.219".to_owned(),
        "sha256:else".to_owned(),
    ));
    assert_eq!(
        w.refused(),
        format!(
            "the image docker loaded is not the verified one: hostkeeper-coordinator:0.1.219 is sha256:else, the tarball's manifest is {} and its config {}",
            manifest_id(),
            config_id()
        )
    );
    assert!(w.engine.created.borrow().is_empty());

    let w = world("cu-not-tagged");
    *w.engine.loads_as.borrow_mut() = None;
    assert!(
        w.refused()
            .contains("hostkeeper-coordinator:0.1.219 is not there, the tarball's manifest is")
    );

    // The containerd store reports the manifest digest.
    let manifest = manifest_id();
    let w = world("cu-manifest-id-ok");
    *w.engine.loads_as.borrow_mut() = Some((
        "hostkeeper-coordinator:0.1.219".to_owned(),
        manifest.clone(),
    ));
    let lines = w.update().expect("the manifest digest is the image's id");
    assert_eq!(
        lines[4],
        format!("coordinator-update: docker loaded {manifest}")
    );
    assert_eq!(w.engine.created.borrow()[0].1["Image"], manifest);
}

#[test]
fn the_verb_needs_an_existing_download_directory_and_runs_docker_through_curl() {
    // Proves: a missing download directory is refused before any program runs; with one, the
    // first thing asked is the container, through curl.
    let w = world("cu-run");
    let mut flags = w.flags();
    flags.download_dir = w.download.join("missing");
    let verifier = &w.verifier;
    let exec = crate::release::fakes::FakeExec::default();
    let err = run(&flags, &w.source, verifier, &exec).expect_err("no directory");
    assert_eq!(
        err.to_string(),
        format!(
            "--download-dir {}: not a directory",
            flags.download_dir.display()
        )
    );
    assert!(exec.calls.borrow().is_empty());

    flags.download_dir = w.download.clone();
    let err = run(&flags, &w.source, verifier, &exec).expect_err("curl is not scripted");
    assert_eq!(
        err.to_string(),
        "docker GET /containers/hostkeeper-coordinator/json: curl could not be started: not scripted: curl -q --silent --show-error --unix-socket /run/docker.sock -X GET --write-out \n%{http_code} http://localhost/v1.43/containers/hostkeeper-coordinator/json"
    );
}

mod flags;
mod replace;

#[test]
fn github_releases_lists_from_the_store_and_downloads_an_asset_by_its_release_url() {
    // Proves: the list is the release store's, and an asset is fetched from
    // `https://github.com/<repo>/releases/download/<tag>/<name>` into `<dir>/<name>`; a failed
    // fetch is the download's error.
    use crate::release::fakes::{FakeDownload, FakeStore};
    let root = scratch("cu-github-releases");
    let store = FakeStore::default();
    store.insert(Release {
        tag: "v0.1.219".to_owned(),
        commit: commit(),
        draft: false,
        assets: Vec::new(),
    });
    let url = "https://github.com/komira-ai/hostkeeper/releases/download/v0.1.219/SHA256SUMS";
    let mut fetch = FakeDownload::default();
    fetch.served.insert(url.to_owned(), b"the sums".to_vec());
    let releases = GitHubReleases {
        store: &store,
        fetch: &fetch,
    };
    let repo = Repo::hostkeeper();
    let listed = releases.list(&repo).expect("the list");
    assert_eq!(
        listed.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
        ["v0.1.219"]
    );
    releases
        .download(&repo, "v0.1.219", "SHA256SUMS", &root)
        .expect("the download");
    assert_eq!(
        std::fs::read(root.join("SHA256SUMS")).expect("the file"),
        b"the sums"
    );
    let missing = releases
        .download(&repo, "v0.1.219", "missing", &root)
        .expect_err("not served");
    assert_eq!(
        missing.to_string(),
        "curl https://github.com/komira-ai/hostkeeper/releases/download/v0.1.219/missing: exit status 22"
    );
    assert_eq!(
        fetch
            .calls
            .borrow()
            .iter()
            .map(|(u, _)| u.as_str())
            .collect::<Vec<_>>(),
        [
            url,
            "https://github.com/komira-ai/hostkeeper/releases/download/v0.1.219/missing"
        ]
    );
}
