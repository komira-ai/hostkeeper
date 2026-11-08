//! The channel sync's committed deployment, run for real: the systemd unit started by its timer
//! (`docs/deploy/channel-sync/hostkeeper-channel-sync.{timer,service}`) and the compose sidecar
//! (`docs/deploy/channel-sync/compose.yaml`). Each test reads the committed file, takes the
//! command it runs, and runs that command as a process with three substitutions only: the xtask
//! binary of this build for the installed one, a scratch directory for the channel, and the
//! committed trust list (`docs/deploy/channel-sync/trust-list.toml`) for the installed one. `gh` on
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
const JOBSCHEDULER: &str = "komira-ai/jobscheduler";
/// Where the unit and the sidecar read the trust list.
const TRUST_LIST: &str = "/etc/hostkeeper/channel-sync-trust.toml";

/// The committed trust list's path in this checkout.
fn committed_trust_list() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/deploy/channel-sync/trust-list.toml");
    path.to_str().expect("UTF-8").to_owned()
}

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
    // A hard link, not a copy: a copy holds the file open for writing, and a process another
    // test forks meanwhile keeps that open until it execs, so starting `gh` could fail with
    // "text file busy". The fake finds its data beside the path it was started by.
    std::fs::hard_link(GH, bin.join("gh")).expect("install the fake gh");
    Site {
        _root: root,
        bin,
        channel,
    }
}

/// The assets of the release of `service` at the index version `v`, as a release that ships
/// channel packages carries them: a package per platform, its bundle (listing its SHA-256), the
/// index, and `SHA256SUMS` of all but the bundles.
fn release_of(service: &str, v: &str) -> BTreeMap<String, Vec<u8>> {
    let mut assets = BTreeMap::new();
    let mut entries = Vec::new();
    for platform in Platform::ALL {
        let name = format!("{service}-{v}-{platform}.tar.zst");
        let bytes = format!("the {platform} package of {service} {v}").into_bytes();
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
    let service = ServiceName::new(service).expect("a service");
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

/// Publishes `releases` (tag, commit, assets) of `repo` to the fake gh: the assets first, then
/// the listing, renamed into place, so a run reads a whole listing or the one before it.
/// The assets of a release, by name.
type Assets = BTreeMap<String, Vec<u8>>;

/// One published release: its tag, its commit and its assets.
type Published<'a> = (&'a str, &'a str, &'a Assets);

fn publish(site: &Site, repo: &str, releases: &[Published<'_>]) {
    let repo = site.bin.join("releases").join(repo);
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

/// What staging the release of `service` at `v` puts in the channel.
fn staged(service: &str, v: &str, assets: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    let mut want = BTreeMap::new();
    for (name, bytes) in assets {
        let staged = name.ends_with(".tar.zst") | name.ends_with(".tar.zst.sigstore.json");
        if staged | (name == "index.json") {
            want.insert(format!("{service}/{v}/{name}"), bytes.clone());
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
const AGENT_V: &str = "0.1.176";
const AGENT_C: &str = "96fba9bdb406bb16587469a5ecaf33d9c861d965";

#[test]
fn the_timer_starts_the_committed_unit_whose_command_stages_a_release_once_and_then_nothing() {
    // Catches: a timer that starts another unit or can start a run while one is going on; a unit
    // whose command does not parse, does not stage a published release, writes outside the one
    // path its sandbox lets it write, or changes the channel on a run with nothing new; or a
    // committed trust list that does not stage js-agent from jobscheduler by its artifacts.yml.
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
    let list = replace_value(&mut args, "--trust-list", &committed_trust_list());
    assert_eq!(list, TRUST_LIST);
    assert_eq!(
        unit_value(&unit, "Service", "ReadWritePaths"),
        Some(committed.as_str()),
        "the sandbox (ProtectSystem=strict) must let the unit write its channel"
    );
    let release = release_of("tierstore", V1);
    publish(&s, TIERSTORE, &[("v0.3.53", C1, &release)]);
    let agent = release_of("js-agent", AGENT_V);
    publish(&s, JOBSCHEDULER, &[("v0.1.176", AGENT_C, &agent)]);
    let mut both = staged("tierstore", V1, &release);
    both.extend(staged("js-agent", AGENT_V, &agent));
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
    assert!(
        out.contains(&format!(
            "staged {JOBSCHEDULER} v0.1.176: js-agent {AGENT_V}\n"
        )),
        "{out}"
    );
    assert_eq!(files(&s.channel), both);
    // The unit syncs hostkeeper's releases too (none here), through the same gh.
    let calls = std::fs::read_to_string(s.bin.join("releases/calls")).expect("the calls");
    assert!(
        calls.contains("api repos/komira-ai/hostkeeper/releases?per_page=100"),
        "{calls}"
    );
    assert!(
        calls.contains("--signer-workflow komira-ai/tierstore/.github/workflows/artifacts.yml")
    );
    assert!(
        calls.contains("--signer-workflow komira-ai/jobscheduler/.github/workflows/artifacts.yml")
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
    assert_eq!(files(&s.channel), both);
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
    let list = replace_value(&mut args, "--trust-list", &committed_trust_list());
    assert_eq!(list, TRUST_LIST);
    assert!(
        volumes.contains(&format!("./trust-list.toml:{TRUST_LIST}:ro")),
        "{volumes:?}"
    );
    let pause = replace_value(&mut args, "--every-secs", "1");
    assert!(pause.parse::<u64>().is_ok_and(|secs| secs > 0), "{pause}");
    let first = release_of("tierstore", V1);
    publish(&s, TIERSTORE, &[("v0.3.53", C1, &first)]);

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
    assert_eq!(files(&s.channel), staged("tierstore", V1, &first));
    let second = release_of("tierstore", V2);
    publish(
        &s,
        TIERSTORE,
        &[("v0.3.53", C1, &first), ("v0.3.54", C2, &second)],
    );
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
    let mut want = staged("tierstore", V1, &first);
    want.extend(staged("tierstore", V2, &second));
    assert_eq!(files(&s.channel), want);
}

/// Each service the committed trust list stages, with the repository whose releases carry it and
/// the workflow whose attestation its packages carry: hostkeeper's artifacts.yml signs hostkeeper,
/// hello and mdm; tierstore's signs tierstore; and jobscheduler's artifacts.yml, which its
/// release.yml calls, signs js-agent. Every signer is on main and on a hosted runner.
const SIGNERS: [(&str, &str, &str); 5] = [
    (
        "hostkeeper",
        "komira-ai/hostkeeper",
        ".github/workflows/artifacts.yml",
    ),
    (
        "hello",
        "komira-ai/hostkeeper",
        ".github/workflows/artifacts.yml",
    ),
    (
        "mdm",
        "komira-ai/hostkeeper",
        ".github/workflows/artifacts.yml",
    ),
    ("tierstore", TIERSTORE, ".github/workflows/artifacts.yml"),
    ("js-agent", JOBSCHEDULER, ".github/workflows/artifacts.yml"),
];

/// How `trust` differs from [`SIGNERS`] for each service it lists, and, when `all` is true, the
/// services of [`SIGNERS`] it does not list; empty when it agrees.
fn signer_problems(
    trust: &hostkeeper_config::channel_trust::ChannelTrust,
    all: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    for listed in trust.services() {
        let name = listed.service.as_str();
        let Some((_, repository, workflow)) = SIGNERS.iter().find(|(s, _, _)| *s == name) else {
            problems.push(format!("{name} is not a service the trust list stages"));
            continue;
        };
        let got = (
            listed.repository.as_str(),
            listed.signers.workflow.as_str(),
            listed.signers.refs.clone(),
            listed.signers.hosted_runner,
        );
        let want = (
            *repository,
            *workflow,
            vec!["refs/heads/main".to_owned()],
            true,
        );
        if got != want {
            problems.push(format!("{name}: {got:?}, not {want:?}"));
        }
    }
    for (name, _, _) in SIGNERS.iter().filter(|_| all) {
        let service = ServiceName::new(name).expect("a name");
        if trust.service(&service).is_none() {
            problems.push(format!("{name} is not listed"));
        }
    }
    problems
}

/// The runbook's trust list example, the first TOML block of `docs/deploy/release-channel.md`.
fn runbook_trust_list() -> String {
    let doc_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/deploy/release-channel.md");
    let doc = std::fs::read_to_string(&doc_path).expect("read the runbook");
    doc.split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the runbook's trust list example")
        .to_owned()
}

#[test]
fn the_trust_lists_of_the_runbook_and_the_deployment_load_and_name_each_services_signer() {
    // Catches: a runbook example or committed trust list the sync refuses at its first run; a
    // committed list that lost a service; or an entry, in either, naming another repository,
    // workflow, ref or runner than the one that signs that service's packages (js-agent's by
    // jobscheduler's release.yml, which no package since it calls artifacts.yml carries).
    use hostkeeper_config::channel_trust::ChannelTrust;
    let committed = std::fs::read_to_string(committed_trust_list()).expect("read the trust list");
    let committed = ChannelTrust::parse(&committed).expect("the committed trust list loads");
    assert_eq!(signer_problems(&committed, true), Vec::<String>::new());
    let runbook = ChannelTrust::parse(&runbook_trust_list()).expect("the runbook's example loads");
    assert_eq!(signer_problems(&runbook, false), Vec::<String>::new());
    let names: Vec<&str> = runbook
        .services()
        .iter()
        .map(|s| s.service.as_str())
        .collect();
    assert_eq!(names, ["tierstore", "js-agent"]);
}

#[test]
fn the_signer_check_names_each_way_an_entry_departs_from_its_services_signer() {
    // Catches: a signer check that passes an entry of the old js-agent workflow, another
    // repository, a tag ref or a self-hosted runner, a service the list should not stage, or a
    // committed list missing a service.
    use hostkeeper_config::channel_trust::ChannelTrust;
    let entry = |name: &str, repository: &str, workflow: &str, refs: &str, hosted: bool| {
        format!(
            "[[service]]\nname = \"{name}\"\nrepository = \"{repository}\"\n\
             workflow = \"{workflow}\"\nrefs = [\"{refs}\"]\nhosted_runner = {hosted}\n"
        )
    };
    let artifacts = ".github/workflows/artifacts.yml";
    let main = "refs/heads/main";
    let text = [
        entry(
            "js-agent",
            JOBSCHEDULER,
            ".github/workflows/release.yml",
            main,
            true,
        ),
        entry("tierstore", JOBSCHEDULER, artifacts, main, true),
        entry(
            "hello",
            "komira-ai/hostkeeper",
            artifacts,
            "refs/tags/v*",
            true,
        ),
        entry("mdm", "komira-ai/hostkeeper", artifacts, main, false),
        entry("jobscheduler", JOBSCHEDULER, artifacts, main, true),
    ]
    .concat();
    let trust = ChannelTrust::parse(&text).expect("a trust list");
    let main_list = "[\"refs/heads/main\"]";
    assert_eq!(
        signer_problems(&trust, true),
        [
            format!(
                "js-agent: (\"{JOBSCHEDULER}\", \".github/workflows/release.yml\", {main_list}, \
                 true), not (\"{JOBSCHEDULER}\", \"{artifacts}\", {main_list}, true)"
            ),
            format!(
                "tierstore: (\"{JOBSCHEDULER}\", \"{artifacts}\", {main_list}, true), not \
                 (\"{TIERSTORE}\", \"{artifacts}\", {main_list}, true)"
            ),
            format!(
                "hello: (\"komira-ai/hostkeeper\", \"{artifacts}\", [\"refs/tags/v*\"], true), \
                 not (\"komira-ai/hostkeeper\", \"{artifacts}\", {main_list}, true)"
            ),
            format!(
                "mdm: (\"komira-ai/hostkeeper\", \"{artifacts}\", {main_list}, false), not \
                 (\"komira-ai/hostkeeper\", \"{artifacts}\", {main_list}, true)"
            ),
            "jobscheduler is not a service the trust list stages".to_owned(),
            "hostkeeper is not listed".to_owned(),
        ]
    );
    // A partial list (the runbook's) is not held to every service.
    assert_eq!(
        signer_problems(&trust, false).last().map(String::as_str),
        Some("jobscheduler is not a service the trust list stages")
    );
}
