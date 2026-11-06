//! The channel sync's committed deployment, run for real: the systemd unit started by its timer
//! (`docs/deploy/channel-sync/hostkeeper-channel-sync.{timer,service}`) and the compose sidecar
//! (`docs/deploy/channel-sync/compose.yaml`). Each test reads the committed file, takes the
//! command it runs, and runs that command as a process with two substitutions only: the xtask
//! binary of this build for the installed one, and a scratch directory for the channel. `gh` on
//! its `PATH` is the test program `tests/bin/gh_fixture.rs`, which answers from scratch files, so
//! no test reaches GitHub. What is asserted is what an operator sees: the exit status, the lines
//! printed, and the bytes in the channel.

#[path = "../src/testdir.rs"]
mod testdir;
#[path = "workflows/yaml.rs"]
#[expect(
    dead_code,
    reason = "the workflow lint's YAML reader, of which this test needs only the loader and the scalars"
)]
mod yaml;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine as _;
use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::index::{PackageEntry, ReleaseIndex};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform;
use sha2::Digest;
use testdir::TestDir;
use yaml::{Node, Value};

const XTASK: &str = env!("CARGO_BIN_EXE_xtask");
const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");
const TIERSTORE: &str = "komira-ai/tierstore";

fn deploy_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/deploy/channel-sync")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A scratch directory: `bin/gh` (the fixture), `bin/releases/` (what it serves), `channel/`.
struct Site {
    _root: TestDir,
    bin: PathBuf,
    channel: PathBuf,
}

fn site(name: &str) -> Site {
    let root = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name);
    let bin = root.join("bin");
    let channel = root.join("channel");
    for dir in [&bin.join("releases"), &channel] {
        std::fs::create_dir_all(dir).expect("make a scratch directory");
    }
    std::fs::copy(GH, bin.join("gh")).expect("install the fake gh");
    Site {
        _root: root,
        bin,
        channel,
    }
}

/// The assets of tierstore's release of the index version `v`, as a release that ships channel
/// packages carries them: a package per platform, its bundle (listing its SHA-256), the index,
/// and `SHA256SUMS` of all but the bundles.
fn tierstore_release(v: &str) -> BTreeMap<String, Vec<u8>> {
    let mut assets = BTreeMap::new();
    let mut entries = Vec::new();
    for platform in Platform::ALL {
        let name = format!("tierstore-{v}-{platform}.tar.zst");
        let bytes = format!("the {platform} package of tierstore {v}").into_bytes();
        let statement = serde_json::json!({"subject": [{"digest": {"sha256": sha256(&bytes)}}]});
        let payload = base64::engine::general_purpose::STANDARD.encode(statement.to_string());
        let bundle = serde_json::json!({"dsseEnvelope": {"payload": payload}}).to_string();
        entries.push(PackageEntry {
            platform,
            sha256: Sha256Hex::parse(&sha256(&bytes)).expect("a SHA-256"),
            bytes: bytes.len() as u64,
        });
        assets.insert(format!("{name}.sigstore.json"), bundle.into_bytes());
        assets.insert(name, bytes);
    }
    let service = ServiceName::new("tierstore").expect("a service");
    let version = Version::new(v).expect("a version");
    let index = ReleaseIndex::new(service, version, entries).expect("an index");
    assets.insert("index.json".to_owned(), index.to_json().into_bytes());
    let sums: String = assets
        .iter()
        .filter(|(name, _)| !name.ends_with(".sigstore.json"))
        .map(|(name, bytes)| format!("{}  {name}\n", sha256(bytes)))
        .collect();
    assets.insert("SHA256SUMS".to_owned(), sums.into_bytes());
    assets
}

/// Publishes `releases` (tag, commit, assets) of tierstore to the fake gh: the assets first, then
/// the listing, renamed into place, so a run reads a whole listing or the one before it.
/// The assets of a release, by name.
type Assets = BTreeMap<String, Vec<u8>>;

/// One published release: its tag, its commit and its assets.
type Published<'a> = (&'a str, &'a str, &'a Assets);

fn publish(site: &Site, releases: &[Published<'_>]) {
    let repo = site.bin.join("releases").join(TIERSTORE);
    let mut list = Vec::new();
    for (tag, commit, assets) in releases {
        let dir = repo.join(tag);
        std::fs::create_dir_all(&dir).expect("make the release");
        for (name, bytes) in *assets {
            std::fs::write(dir.join(name), bytes).expect("write an asset");
        }
        let names: Vec<_> = assets
            .keys()
            .map(|n| serde_json::json!({"name": n}))
            .collect();
        list.push(serde_json::json!({
            "tag_name": tag, "target_commitish": commit, "draft": false, "assets": names,
        }));
    }
    let temp = repo.join("list-json.part");
    std::fs::write(&temp, serde_json::Value::Array(list).to_string()).expect("write");
    std::fs::rename(&temp, repo.join("list.json")).expect("publish the listing");
}

/// Every file under `dir`, from `dir`, with its bytes.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_owned()];
    while let Some(next) = todo.pop() {
        for entry in std::fs::read_dir(&next).expect("read a directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let rel = path.strip_prefix(dir).expect("under dir").to_string_lossy();
                out.insert(rel.into_owned(), std::fs::read(&path).expect("read a file"));
            }
        }
    }
    out
}

/// What staging tierstore's release of `v` puts in the channel.
fn staged(v: &str, assets: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    let mut want = BTreeMap::new();
    for (name, bytes) in assets {
        let staged = name.ends_with(".tar.zst") | name.ends_with(".tar.zst.sigstore.json");
        if staged | (name == "index.json") {
            want.insert(format!("tierstore/{v}/{name}"), bytes.clone());
        }
    }
    want
}

/// The value of `key` in the section `[section]` of a unit file, the last when it repeats.
fn unit_value<'a>(unit: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut current = "";
    let mut found = None;
    for line in unit.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if let Some(v) = line
            .strip_prefix(key)
            .and_then(|l| l.strip_prefix('='))
            .filter(|_| current == section)
        {
            found = Some(v);
        }
    }
    found
}

/// `args` with the value after `flag` replaced by `with`; panics when the flag is not there, so
/// a command that lost the flag fails the test.
fn replace_value(args: &mut [String], flag: &str, with: &str) -> String {
    let at = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("the committed command has no {flag}"));
    std::mem::replace(&mut args[at + 1], with.to_owned())
}

const V1: &str = "0.3.53+0771c30f87f3";
const C1: &str = "0771c30f87f375ffd8b39e8b4630cd6ea377b764";
const V2: &str = "0.3.54+1a2b3c4d5e6f";
const C2: &str = "1a2b3c4d5e6f1a2b3c4d5e6f1a2b3c4d5e6f1a2b";

#[test]
fn the_timer_starts_the_committed_unit_whose_command_stages_a_release_once_and_then_nothing() {
    // Catches: a timer that starts another unit or can start a run while one is going on; a unit
    // whose command does not parse, does not stage a published release, writes outside the one
    // path its sandbox lets it write, or changes the channel on a run with nothing new.
    let timer = deploy_file("hostkeeper-channel-sync.timer");
    let service = unit_value(&timer, "Timer", "Unit").unwrap_or("hostkeeper-channel-sync.service");
    assert_eq!(service, "hostkeeper-channel-sync.service");
    assert!(unit_value(&timer, "Timer", "OnUnitInactiveSec").is_some());
    assert_eq!(unit_value(&timer, "Timer", "OnCalendar"), None);
    assert_eq!(
        unit_value(&timer, "Install", "WantedBy"),
        Some("timers.target")
    );

    let unit = deploy_file(service);
    assert_eq!(unit_value(&unit, "Service", "Type"), Some("oneshot"));
    let exec = unit_value(&unit, "Service", "ExecStart").expect("the unit's ExecStart");
    let mut words = exec.split_whitespace().map(str::to_owned);
    let program = words.next().expect("a program");
    assert_eq!(program, "/usr/local/libexec/hostkeeper/xtask");
    let mut args: Vec<String> = words.collect();
    let s = site("channel-sync-unit");
    let channel = s.channel.to_str().expect("UTF-8").to_owned();
    let committed = replace_value(&mut args, "--channel-dir", &channel);
    assert_eq!(
        unit_value(&unit, "Service", "ReadWritePaths"),
        Some(committed.as_str()),
        "the sandbox (ProtectSystem=strict) must let the unit write its channel"
    );
    let release = tierstore_release(V1);
    publish(&s, &[("v0.3.53", C1, &release)]);
    let run = || {
        Command::new(XTASK)
            .args(&args)
            .env("PATH", &s.bin)
            .output()
            .expect("run the unit's command")
    };

    let first = run();
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let out = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(
        out.contains(&format!("staged {TIERSTORE} v0.3.53: tierstore {V1}\n")),
        "{out}"
    );
    assert_eq!(files(&s.channel), staged(V1, &release));
    // The unit syncs hostkeeper's releases too (none here), through the same gh.
    let calls = std::fs::read_to_string(s.bin.join("releases/calls")).expect("the calls");
    assert!(
        calls.contains("api repos/komira-ai/hostkeeper/releases?per_page=100"),
        "{calls}"
    );
    assert!(
        calls.contains("--signer-workflow komira-ai/tierstore/.github/workflows/artifacts.yml")
    );

    let second = run();
    assert_eq!(
        second.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let out = String::from_utf8_lossy(&second.stdout).into_owned();
    assert!(
        out.contains(&format!("unchanged {TIERSTORE} v0.3.53: tierstore {V1}\n")),
        "{out}"
    );
    assert!(!out.contains("staged "), "{out}");
    assert_eq!(files(&s.channel), staged(V1, &release));
}

/// The entry `key` of the mapping `map`.
fn entry<'a>(map: &'a Node, key: &str) -> &'a Node {
    let Value::Map(entries) = &map.value else {
        panic!("{key}: not under a mapping");
    };
    entries
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {key}"))
}

/// The scalars of the sequence `node`.
fn strings(node: &Node) -> Vec<String> {
    let Value::Seq(items) = &node.value else {
        panic!("not a sequence");
    };
    items
        .iter()
        .map(|i| i.as_str().expect("a scalar").to_owned())
        .collect()
}

#[test]
fn the_committed_sidecar_stages_a_release_and_the_next_published_one_round_after_round() {
    // Catches: a sidecar whose entrypoint and command do not run the sync, run it once and exit
    // (the container would then restart it in a tight loop), stop at the first round, or miss a
    // release published while it runs; or a gh or channel mount the command does not use.
    let compose = yaml::load(&deploy_file("compose.yaml")).expect("compose.yaml is YAML");
    let sidecar = entry(entry(&compose, "services"), "channel-sync");
    let entrypoint = strings(entry(sidecar, "entrypoint"));
    assert_eq!(entrypoint, ["/usr/local/bin/xtask"]);
    let volumes = strings(entry(sidecar, "volumes"));
    assert!(
        volumes.contains(&"./bin/xtask:/usr/local/bin/xtask:ro".to_owned()),
        "{volumes:?}"
    );
    // distroless's PATH holds /usr/local/bin, where gh is mounted.
    assert!(
        volumes.contains(&"./bin/gh:/usr/local/bin/gh:ro".to_owned()),
        "{volumes:?}"
    );
    let mut args = strings(entry(sidecar, "command"));
    let s = site("channel-sync-sidecar");
    let committed = replace_value(
        &mut args,
        "--channel-dir",
        s.channel.to_str().expect("UTF-8"),
    );
    assert!(
        volumes
            .iter()
            .any(|v| v.ends_with(&format!(":{committed}"))),
        "{volumes:?}"
    );
    // The committed pause is minutes; the test waits on the rounds' lines, not on time, and a
    // pause of one second keeps it short.
    let pause = replace_value(&mut args, "--every-secs", "1");
    assert!(pause.parse::<u64>().is_ok_and(|secs| secs > 0), "{pause}");
    let first = tierstore_release(V1);
    publish(&s, &[("v0.3.53", C1, &first)]);

    let mut child = Command::new(XTASK)
        .args(&args)
        .env("PATH", &s.bin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the sidecar's command");
    let mut lines = BufReader::new(child.stdout.take().expect("its output")).lines();
    let mut next_summary = || loop {
        let line = lines
            .next()
            .expect("the sidecar ended; it must run round after round")
            .expect("a line");
        if line.starts_with("channel-sync: ") {
            return line;
        }
    };

    assert_eq!(
        next_summary(),
        "channel-sync: 1 versions staged, 0 already in the channel, 0 refused"
    );
    assert_eq!(files(&s.channel), staged(V1, &first));
    let second = tierstore_release(V2);
    publish(&s, &[("v0.3.53", C1, &first), ("v0.3.54", C2, &second)]);
    // Rounds that ran before the new listing was in place stage nothing; the first after it
    // stages the new release and leaves the first alone.
    loop {
        let summary = next_summary();
        if summary != "channel-sync: 0 versions staged, 1 already in the channel, 0 refused" {
            assert_eq!(
                summary,
                "channel-sync: 1 versions staged, 1 already in the channel, 0 refused"
            );
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut want = staged(V1, &first);
    want.extend(staged(V2, &second));
    assert_eq!(files(&s.channel), want);
}
