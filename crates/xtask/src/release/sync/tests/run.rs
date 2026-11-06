//! A run over repositories and releases: the order, a download cut short, two repositories,
//! the work directory, the request, the loop of `--every-secs` and the flags.

use super::*;

#[test]
fn a_download_cut_short_leaves_no_version_in_the_channel_and_the_next_run_stages_it() {
    // Catches: a version made visible in the channel before every asset arrived, or a work
    // directory left behind by a failed download.
    let c = channel("sync-cut-short");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    source.downloads_left.set(Some(1));
    let verifier = FakeVerifier::attests_as_asked();
    let req = request(&c, &[TIERSTORE]);

    let lines = refused_lines(sync(&source, &verifier, &req));

    assert!(snapshot(&c.dir).is_empty(), "{:?}", snapshot(&c.dir));
    let first_other = assets
        .keys()
        .find(|n| *n != "index.json")
        .expect("an asset");
    assert_eq!(
        lines[0],
        format!(
            "refused {TIERSTORE} v0.3.53: download {first_other} of {TIERSTORE} v0.3.53: the \
             connection was reset"
        )
    );
    source.downloads_left.set(None);
    sync(&source, &verifier, &req).expect("the next run");
    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
}

#[test]
fn a_two_repository_run_stages_both_without_mixing_their_assets() {
    // Catches: assets of one repository (the same names: index.json, SHA256SUMS) staged or
    // checked as another's, or one repository's release checked with the other's signer.
    let c = channel("sync-two-repos");
    let source = FakeSource::default();
    let ts = release_of("tierstore", TS_V, "tierstore");
    let other = release_of("other", "2.0.0", "other");
    publish(&source, TIERSTORE, "v0.3.53", 1, &ts);
    publish(&source, OTHER, "v2.0.0", 2, &other);
    let verifier = FakeVerifier::attests_as_asked();

    let lines = sync(&source, &verifier, &request(&c, &[TIERSTORE, OTHER])).expect("synced");

    let mut want = staged("tierstore", TS_V, &ts, "index.json");
    want.extend(staged("other", "2.0.0", &other, "index.json"));
    assert_eq!(snapshot(&c.dir), want);
    assert_eq!(
        lines[..2],
        [
            format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
            format!("staged {OTHER} v2.0.0: other 2.0.0"),
        ]
    );
    let signers = verifier.signers.borrow();
    let other_signer = "komira-ai/other/.github/workflows/artifacts.yml".to_owned();
    assert_eq!(
        *signers,
        [vec![TIERSTORE_SIGNER.to_owned(); 6], vec![other_signer; 6]].concat()
    );
}

#[test]
fn releases_are_synced_oldest_first_and_drafts_other_tags_and_releases_before_since_skipped() {
    // Catches: a draft staged, a tag that is not a version staged or failing the run, `--since`
    // skipping its own tag or not skipping older ones, or releases staged newest first.
    let c = channel("sync-order");
    let source = FakeSource::default();
    for (tag, n) in [("v0.3.10", 3), ("v0.3.9", 2), ("v0.3.8", 1)] {
        let v = format!("{}+{:012x}", &tag[1..], n);
        publish(
            &source,
            TIERSTORE,
            tag,
            n,
            &release_of("tierstore", &v, tag),
        );
    }
    let draft = release_of("tierstore", "0.3.11+00000000000b", "draft");
    let list = draft.into_iter().collect();
    source.add(TIERSTORE, "v0.3.11", nth_commit(11).as_str(), true, list);
    source.publish(TIERSTORE, "nightly", nth_commit(12).as_str(), Vec::new());
    let mut req = request(&c, &[TIERSTORE]);
    req.since = Some(parse_tag("v0.3.9").expect("a tag"));

    let lines = sync(&source, &FakeVerifier::attests_as_asked(), &req).expect("synced");

    assert_eq!(
        lines,
        vec![
            format!("skipped {TIERSTORE} nightly: not a tag v<X.Y.Z>"),
            format!("staged {TIERSTORE} v0.3.9: tierstore 0.3.9+000000000002"),
            format!("staged {TIERSTORE} v0.3.10: tierstore 0.3.10+000000000003"),
            "channel-sync: 2 versions staged, 0 already in the channel, 0 refused".to_owned(),
        ]
    );
    let services: Vec<String> = tree_of(&c.dir.join("tierstore"))
        .into_iter()
        .filter(|p| p.ends_with('/'))
        .collect();
    assert_eq!(services, ["0.3.10+000000000003/", "0.3.9+000000000002/"]);
}

#[test]
fn a_release_without_an_index_is_skipped_and_the_run_succeeds() {
    // Catches: a release that ships no channel package (binaries only) failing every run.
    let c = channel("sync-no-index");
    let source = FakeSource::default();
    let mut assets = release_of("tierstore", TS_V, "a");
    assets.remove("index.json");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);

    let lines = sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    )
    .expect("synced");

    assert_eq!(
        lines[0],
        format!("skipped {TIERSTORE} v0.3.53: no release index among its assets")
    );
    assert!(source.downloads.borrow().is_empty());
    assert!(snapshot(&c.dir).is_empty());
}

/// A source that answers as `inner`, after replacing the channel directory by a file when it
/// lists (`on_list`), or the work directory by a file after each download.
struct Sabotaging<'a> {
    inner: &'a FakeSource,
    channel: PathBuf,
    on_list: bool,
}

impl ReleaseSource for Sabotaging<'_> {
    fn list(&self, repo: &Repo) -> Result<Vec<Release>, PortError> {
        if self.on_list {
            std::fs::remove_dir(&self.channel).expect("remove the empty channel");
            std::fs::write(&self.channel, b"not a directory").expect("write");
        }
        self.inner.list(repo)
    }

    fn download(&self, repo: &Repo, tag: &str, name: &str, dir: &Path) -> Result<(), PortError> {
        self.inner.download(repo, tag, name, dir)?;
        if !self.on_list {
            // The work directory is replaced by a file: its removal fails.
            let root = dir.parent().expect("the work directory");
            std::fs::remove_dir_all(root).expect("remove the work directory");
            std::fs::write(root, b"x").expect("write");
        }
        Ok(())
    }
}

#[test]
fn a_work_directory_that_cannot_be_made_or_removed_is_reported() {
    // Catches: a release counted as done when its work directory could not be made, or when
    // what it left in the channel could not be removed.
    let source = FakeSource::default();
    let mut assets = release_of("tierstore", TS_V, "a");
    assets.remove("index.json");
    assets.insert("x-index.json".to_owned(), b"[]".to_vec());
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    for on_list in [true, false] {
        let c = channel("sync-work-dir");
        let sabotaging = Sabotaging {
            inner: &source,
            channel: c.dir.clone(),
            on_list,
        };
        let lines = refused_lines(sync(
            &sabotaging,
            &FakeVerifier::attests_as_asked(),
            &request(&c, &[TIERSTORE]),
        ));
        let start = format!("refused {TIERSTORE} v0.3.53: ");
        let why = if on_list { "create " } else { "read " };
        assert!(
            lines[0].starts_with(&start) & lines[0].contains(why),
            "{lines:?}"
        );
        if !on_list {
            assert!(
                lines[1].starts_with(&format!("{start}remove ")),
                "{lines:?}"
            );
        }
    }
}

#[test]
fn a_request_with_no_channel_directory_is_refused_before_any_read() {
    // Catches: a mistyped channel path made or synced into.
    let c = channel("sync-request");
    let source = FakeSource::default();
    let verifier = FakeVerifier::attests_as_asked();
    let file = c.dir.join("a-file");
    std::fs::write(&file, b"x").expect("write");
    let mut not_dir = request(&c, &[TIERSTORE]);
    not_dir.channel = file.clone();
    assert_eq!(
        sync(&source, &verifier, &not_dir).map_err(|e| e.to_string()),
        Err(format!("--channel-dir {}: not a directory", file.display()))
    );
    let mut missing = request(&c, &[TIERSTORE]);
    missing.channel = c.dir.join("missing");
    let refused = sync(&source, &verifier, &missing).map_err(|e| e.to_string());
    assert!(refused.is_err_and(|e| e.starts_with("--channel-dir ")));
    assert!(source.downloads.borrow().is_empty());
}

/// A pause that records each round and ends the loop after `rounds` of them.
struct Counting {
    rounds: usize,
    seen: RefCell<Vec<Result<Vec<String>, Error>>>,
}

impl Pause for Counting {
    fn after(&self, round: &Result<Vec<String>, Error>) -> ControlFlow<()> {
        self.seen.borrow_mut().push(round.clone());
        if self.seen.borrow().len() < self.rounds {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }
}

#[test]
fn every_runs_round_after_round_and_a_refused_round_does_not_stop_it() {
    // Catches: the loop ending on a refused round, a release published between two rounds
    // missed, or a pause skipped after a round.
    let c = channel("sync-every");
    let source = FakeSource::default();
    let mut bad = release_of("tierstore", TS_V, "a");
    bad.remove(SUMS);
    publish(&source, TIERSTORE, "v0.3.53", 1, &bad);
    let pause = Counting {
        rounds: 2,
        seen: RefCell::new(Vec::new()),
    };
    let calls = Cell::new(0);
    struct Publishing<'a>(&'a FakeSource, &'a Cell<usize>);
    impl ReleaseSource for Publishing<'_> {
        fn list(&self, repo: &Repo) -> Result<Vec<Release>, PortError> {
            self.1.set(self.1.get() + 1);
            if self.1.get() == 2 {
                let assets = release_of("tierstore", "0.3.54+000000000002", "b");
                publish(self.0, TIERSTORE, "v0.3.54", 2, &assets);
            }
            self.0.list(repo)
        }
        fn download(&self, repo: &Repo, tag: &str, n: &str, dir: &Path) -> Result<(), PortError> {
            self.0.download(repo, tag, n, dir)
        }
    }

    let last = every(
        &Publishing(&source, &calls),
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
        &pause,
    );

    let seen = pause.seen.borrow();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].is_err() & seen[1].is_err());
    assert_eq!(last, seen[1]);
    let lines = refused_lines(last);
    assert_eq!(
        lines[1],
        format!("staged {TIERSTORE} v0.3.54: tierstore 0.3.54+000000000002")
    );
}

#[test]
fn every_refuses_a_bad_request_once_without_a_round() {
    // Catches: a mistyped channel path retried forever by the sidecar's loop.
    let c = channel("sync-every-bad");
    let mut req = request(&c, &[TIERSTORE]);
    req.channel = c.dir.join("missing");
    let pause = Counting {
        rounds: 1,
        seen: RefCell::new(Vec::new()),
    };
    let result = every(
        &FakeSource::default(),
        &FakeVerifier::attests_as_asked(),
        &req,
        &pause,
    );
    assert!(result.is_err());
    assert!(pause.seen.borrow().is_empty());
}

#[test]
fn the_sleep_of_every_secs_prints_each_round_and_never_ends_the_loop() {
    // Catches: the sidecar's loop ending after one round, ok or refused.
    let sleep = Sleep(Duration::ZERO);
    assert_eq!(
        sleep.after(&Ok(vec!["staged x".to_owned()])),
        ControlFlow::Continue(())
    );
    let refused = Err(Error::Refused(vec!["refused x".to_owned()]));
    assert_eq!(sleep.after(&refused), ControlFlow::Continue(()));
}

fn parse(args: &[&str]) -> Result<Verb, clap::Error> {
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        verb: Verb,
    }
    let argv = [&["release", "channel-sync"], args].concat();
    Cli::try_parse_from(argv).map(|cli| cli.verb)
}

#[test]
fn the_flags_take_a_trust_list_a_channel_a_tag_and_a_positive_pause() {
    // Catches: `--since` taking a version without its `v`, `--every-secs 0` spinning the loop,
    // or a run without a trust list, which would trust nothing or everything.
    let Ok(Verb::ChannelSync {
        trust_list,
        channel_dir,
        since,
        every_secs,
    }) = parse(&[
        "--trust-list",
        "t.toml",
        "--channel-dir",
        "c",
        "--since",
        "v0.3.9",
        "--every-secs",
        "600",
    ])
    else {
        panic!("the flags parse");
    };
    assert_eq!(trust_list, PathBuf::from("t.toml"));
    assert_eq!(channel_dir, PathBuf::from("c"));
    assert_eq!(since, Some("0.3.9".parse().expect("a version")));
    assert_eq!(every_secs, Some(600));
    for bad in [
        vec!["--channel-dir", "c"],
        vec!["--trust-list", "t.toml"],
        vec!["--repo", TIERSTORE, "--channel-dir", "c"],
        vec![
            "--trust-list",
            "t.toml",
            "--channel-dir",
            "c",
            "--since",
            "0.3.9",
        ],
        vec![
            "--trust-list",
            "t.toml",
            "--channel-dir",
            "c",
            "--every-secs",
            "0",
        ],
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_verb_runs_once_or_every_through_the_ports() {
    // Catches: the verb wired to another port than the release source and the verifier, a trust
    // list not read from its file, or `--every-secs` not checking the request before its first
    // round.
    let c = channel("sync-verb");
    let fakes = Fakes::over(FakeHistory::default());
    let assets = release_of("tierstore", TS_V, "a");
    publish(&fakes.source, TIERSTORE, "v0.3.53", 1, &assets);
    let fakes = Fakes {
        verifier: FakeVerifier::attests_as_asked(),
        ..fakes
    };
    let list = c.dir.parent().expect("the scratch").join("trust.toml");
    let artifacts = ".github/workflows/artifacts.yml";
    let text = entry("tierstore", TIERSTORE, artifacts, &["refs/heads/main"]);
    std::fs::write(&list, text).expect("write the trust list");
    let list = list.to_str().expect("UTF-8");
    let channel_dir = c.dir.to_str().expect("UTF-8");
    let once = parse(&["--trust-list", list, "--channel-dir", channel_dir]).expect("flags");
    run_with(once, &fakes.ports()).expect("synced");
    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );

    let missing = c.dir.join("missing");
    let missing = missing.to_str().expect("UTF-8");
    let looped = parse(&[
        "--trust-list",
        list,
        "--channel-dir",
        missing,
        "--every-secs",
        "9",
    ])
    .expect("flags");
    assert!(run_with(looped, &fakes.ports()).is_err());
}

#[test]
fn a_trust_list_that_cannot_be_read_or_is_refused_stops_the_verb_naming_its_file() {
    // Catches: a missing or broken trust list read as an empty one, so the run stages nothing and
    // succeeds, or a refusal that does not say which file and which key.
    let c = channel("sync-verb-trust");
    let fakes = Fakes::over(FakeHistory::default());
    let channel_dir = c.dir.to_str().expect("UTF-8");
    let missing = c.dir.join("no-such.toml");
    let run = |list: &Path| {
        let list = list.to_str().expect("UTF-8");
        let verb = parse(&["--trust-list", list, "--channel-dir", channel_dir]).expect("flags");
        run_with(verb, &fakes.ports()).map_err(|e| e.to_string())
    };
    let refused = run(&missing);
    let start = format!("--trust-list {}: ", missing.display());
    assert!(
        refused.as_ref().is_err_and(|e| e.starts_with(&start)),
        "{refused:?}"
    );
    let bad = c.dir.parent().expect("the scratch").join("bad.toml");
    let text = entry(
        "tierstore",
        TIERSTORE,
        "artifacts.yml",
        &["refs/heads/main"],
    );
    std::fs::write(&bad, text.replace("hosted_runner = true", "")).expect("write");
    assert_eq!(
        run(&bad),
        Err(format!(
            "--trust-list {}: the trust list's service[0].hosted_runner is missing",
            bad.display()
        ))
    );
    assert!(fakes.source.downloads.borrow().is_empty());
    assert!(snapshot(&c.dir).is_empty());
}

/// An [`Exec`] that starts nothing: every program fails to start, with its name in the error.
#[derive(Clone)]
struct StartsNothing;

impl crate::release::ports::Exec for StartsNothing {
    fn spawn(
        &self,
        program: &str,
        _args: &[String],
    ) -> std::io::Result<crate::release::ports::Ran> {
        Err(std::io::Error::other(format!("{program} is not started")))
    }
}

#[test]
fn the_wired_release_source_is_gh() {
    // Catches: the real adapters answering the sync's listing or downloads by another program.
    let wired = crate::release::Wired::new(StartsNothing);
    let ports = wired.ports();
    let repo = repo(TIERSTORE);
    let why = "gh is not started".to_owned();
    assert_eq!(
        ports.source.list(&repo).map_err(|e| e.why),
        Err(why.clone())
    );
    let download = ports
        .source
        .download(&repo, "v1.0.0", SUMS, Path::new("in"));
    assert_eq!(download.map_err(|e| e.why), Err(why));
}

#[test]
fn a_work_directory_left_by_a_killed_run_is_removed_and_the_release_synced() {
    // Catches: a work directory of a run that was killed (whose name a restarted process,
    // pid 1 in a container, takes again) refusing a release on every later run, or left in the
    // served tree for good.
    let c = channel("sync-left-work");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let left = c.dir.join(format!("{WORK_PREFIX}{}-0", std::process::id()));
    std::fs::create_dir_all(left.join("assets")).expect("make");
    std::fs::write(left.join("assets").join("index.json"), b"half").expect("write");

    let lines = sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    )
    .expect("synced");

    assert_eq!(
        lines[0],
        format!("removed {}, left by an earlier run", left.display())
    );
    assert_eq!(
        lines[1],
        format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}")
    );
    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
}

#[test]
fn a_work_directory_that_cannot_be_cleared_is_reported_and_the_rest_synced() {
    // Catches: a left work directory that cannot be removed passing silently, or stopping the
    // sync of the releases.
    let c = channel("sync-left-file");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    // A file of the work prefix: removing it as a directory fails.
    let left = c.dir.join(format!("{WORK_PREFIX}x"));
    std::fs::write(&left, b"x").expect("write");

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    ));

    assert!(
        lines[0].starts_with(&format!("refused remove {}: ", left.display())),
        "{lines:?}"
    );
    assert_eq!(
        lines[1],
        format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}")
    );
}

/// A pause that removes the channel directory after the first round and ends after the second.
struct Unmounting(PathBuf, Cell<usize>);

impl Pause for Unmounting {
    fn after(&self, _round: &Result<Vec<String>, Error>) -> ControlFlow<()> {
        self.1.set(self.1.get() + 1);
        if self.1.get() == 1 {
            std::fs::remove_dir_all(&self.0).expect("remove the channel");
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }
}

#[test]
fn a_channel_directory_gone_between_rounds_is_refused_by_the_next_round() {
    // Catches: a round of the loop that finds its channel gone passing as a round with nothing
    // new, or making the directory again.
    let c = channel("sync-gone");
    let pause = Unmounting(c.dir.clone(), Cell::new(0));
    let last = every(
        &FakeSource::default(),
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[OTHER]),
        &pause,
    );
    let lines = refused_lines(last);
    assert!(
        lines[0].starts_with(&format!("refused read {}: ", c.dir.display())),
        "{lines:?}"
    );
    assert!(!c.dir.exists());
}
