//! The tests of a run that meets GitHub's limit of requests without a token: it ends with its
//! report and nothing changed, and the next run takes up where it stopped.

use std::path::PathBuf;

use super::engine::Probed;
use super::public::FetchError;
use super::tests_fake::{FakeEngine, FakePause, FakeProbe, FakeSource, ports};
use super::*;
use crate::release::fakes::{FakeVerifier, nth_commit, scratch};
use crate::release::image_index::{fake_image, image_name};
use crate::release::ports::PortError;

fn v(text: &str) -> Version {
    text.parse().expect("a version")
}

/// A machine whose coordinator runs v0.1.218, the release v0.1.219 published, and the download
/// directory; the flags of a run on it.
fn machine(name: &str) -> (crate::testdir::TestDir, FakeSource, FakeEngine, Flags) {
    let root = scratch(name);
    let download = root.join("downloads");
    std::fs::create_dir(&download).expect("the download directory");
    let source = FakeSource::default();
    let tar = fake_image("amd64", "m");
    let image = image_name(&v("0.1.219"), Platform::LinuxX86_64);
    let sums = format!("{}  {image}\n", hex::encode(sha2::Sha256::digest(&tar)));
    source.publish(
        "komira-ai/hostkeeper",
        "v0.1.219",
        &nth_commit(7).to_string(),
        vec![
            (image.clone(), tar),
            (bundle_of(&image), b"{}".to_vec()),
            (SUMS.to_owned(), sums.into_bytes()),
        ],
    );
    let engine = FakeEngine::default();
    engine.containers.borrow_mut().push(tests_fake::Container {
        id: tests_fake::OLD_ID.to_owned(),
        name: "hostkeeper-coordinator".to_owned(),
        inspect: super::tests::old_container(),
        running: true,
        restarting: false,
        restart_count: 0,
        logs: String::new(),
    });
    engine
        .images
        .borrow_mut()
        .insert("sha256:oldimage".to_owned(), super::tests::old_image());
    let flags = Flags {
        container: "hostkeeper-coordinator".to_owned(),
        download_dir: download,
        docker_socket: PathBuf::from("/run/docker.sock"),
        coordinator_url: "https://coordinator.example.com:8444".to_owned(),
        ca_file: PathBuf::from("/etc/ca.pem"),
        platform: Platform::LinuxX86_64,
        target_version: None,
        ready_checks: 1,
        check_every_secs: 5,
        stop_timeout_secs: 30,
        curl: "curl".to_owned(),
        gh: PathBuf::from(GH),
        trusted_root: PathBuf::from(TRUSTED_ROOT),
        ca_bundle: PathBuf::from(public::CA_BUNDLE),
        github_api_url: public::API.to_owned(),
        github_url: public::SITE.to_owned(),
    };
    (root, source, engine, flags)
}

use sha2::Digest as _;

#[test]
fn a_used_up_limit_on_the_list_ends_the_run_before_anything_is_asked() {
    // Proves: when GitHub refuses the list for its limit, the run reports why and that the
    // next run tries again, and returns success; docker, the coordinator and the verifier are
    // asked nothing, and nothing is downloaded.
    let (_root, source, engine, flags) = machine("cu-wait-list");
    source.list_limited.set(true);
    let verifier = FakeVerifier::good(&nth_commit(7));
    let probe = FakeProbe::new(Probed::Running(v("0.1.218")), Vec::new());
    let pause = FakePause::default();
    let p = ports(&source, &verifier, &engine, &probe, &pause);
    assert_eq!(
        update(&p, &flags),
        Ok(vec![
            "coordinator-update: the list: HTTP 403: nothing changed; the next run tries again"
                .to_owned()
        ])
    );
    assert!(engine.calls().is_empty());
    assert!(probe.calls.borrow().is_empty());
    assert!(verifier.calls.borrow().is_empty());
    assert!(source.downloads.borrow().is_empty());
}

#[test]
fn a_used_up_limit_on_a_download_ends_the_run_and_the_next_run_downloads_only_the_rest() {
    // Proves: a download refused for the limit ends the run with success and its report so
    // far, nothing loaded or created, the files already downloaded kept and no partial file
    // left; the next run, under the limit again, downloads only what is missing and updates.
    let (_root, source, engine, flags) = machine("cu-wait-download");
    source.downloads_left.set(Some(1));
    source.limited.set(true);
    // The fake image's config digest: what a classic image store names the loaded image.
    let config = br#"{"architecture":"amd64","marker":"m"}"#;
    *engine.loads_as.borrow_mut() = Some((
        "hostkeeper-coordinator:0.1.219".to_owned(),
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(config))),
    ));
    let verifier = FakeVerifier::good(&nth_commit(7));
    let probe = FakeProbe::new(
        Probed::Running(v("0.1.218")),
        vec![Probed::Running(v("0.1.219"))],
    );
    let pause = FakePause::default();
    let p = ports(&source, &verifier, &engine, &probe, &pause);
    let image = image_name(&v("0.1.219"), Platform::LinuxX86_64);
    assert_eq!(
        update(&p, &flags),
        Ok(vec![
            "coordinator-update: target v0.1.219".to_owned(),
            "coordinator-update: hostkeeper-coordinator runs v0.1.218".to_owned(),
            format!(
                "coordinator-update: download {image}.sigstore.json of komira-ai/hostkeeper v0.1.219: HTTP 429: nothing changed; the next run tries again"
            ),
        ])
    );
    assert!(!engine.calls().iter().any(|c| c.starts_with("load ")));
    assert!(engine.created.borrow().is_empty());
    assert!(verifier.calls.borrow().is_empty());
    let dir = flags.download_dir.join("v0.1.219");
    assert!(dir.join(&image).is_file());
    assert!(!dir.join(PART).join(bundle_of(&image)).exists());

    source.downloads_left.set(None);
    let lines = update(&p, &flags).expect("the next run");
    assert_eq!(
        lines.last().map(String::as_str),
        Some(
            "coordinator-update: hostkeeper-coordinator now runs v0.1.219 (was v0.1.218); its command line, mounts and volumes are the old container's"
        )
    );
    assert_eq!(
        *source.downloads.borrow(),
        vec![
            format!("komira-ai/hostkeeper v0.1.219 {image}"),
            format!("komira-ai/hostkeeper v0.1.219 {image}.sigstore.json"),
            "komira-ai/hostkeeper v0.1.219 SHA256SUMS".to_owned(),
        ]
    );
}

#[test]
fn a_list_that_github_cannot_give_is_a_failure_of_the_run() {
    // Proves: a list GitHub refuses for any other reason than its limit fails the run (exit 1),
    // naming the request, and changes nothing.
    let e = Error::from(FetchError::Failed(PortError::new(
        "GET https://api.github.com/repos/komira-ai/hostkeeper/releases?per_page=100",
        "HTTP 404",
    )));
    assert_eq!(
        e,
        Error::Engine(PortError::new(
            "GET https://api.github.com/repos/komira-ai/hostkeeper/releases?per_page=100",
            "HTTP 404"
        ))
    );
    assert_eq!(
        Error::from(FetchError::RateLimited("used up".to_owned())),
        Error::Wait("used up".to_owned())
    );
}
