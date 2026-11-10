use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::path::Path;

use clap::Parser;
use hostkeeper_proto::hostkeeper::release::v1::{Hook, Manifest, StepBounds};
use prost_reflect::DeserializeOptions;
use serde_json::json;

use super::manifest::OLDEST_READER;
use super::*;
use crate::release::contract::Service;
use crate::release::fakes::{FakeHistory, Fakes, scratch};
use crate::release::native::{deb as native_deb, hello as native_hello};
use crate::release::package;
use crate::release::ports::Adapters;
use crate::release::{Verb, file_sha256};
use crate::testdir::TestDir;
use hostkeeper_config::platform::Platform as IndexPlatform;

const REPO: &str = "komira-ai/hostkeeper";

fn release() -> Version {
    "0.1.5".parse().expect("a version")
}

/// A scratch directory and the release's asset directory in it.
fn dist(name: &str) -> (TestDir, PathBuf) {
    let root = scratch(name);
    let dist = root.join("dist");
    std::fs::create_dir(&dist).expect("make dist/");
    (root, dist)
}

/// An unsigned bundle whose statement lists `digest` as its one subject's SHA-256.
fn bundle_listing(digest: &str) -> Vec<u8> {
    let statement = json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": "package", "digest": {"sha256": digest}}],
    });
    json!({"dsseEnvelope": {"payload": STANDARD.encode(statement.to_string())}})
        .to_string()
        .into_bytes()
}

/// Writes each native package of `package` into `dist` with a bundle listing its SHA-256: a
/// `.deb` as `release deb` builds it (hello's files; hostkeeper's and mdm's with their program
/// alone, as nfpm's staged files and mdm's pinned servers are not at hand), and in place of the
/// `.pkg`, which only a Mac builds, bytes of its own.
fn built(dist: &Path, package: Package) {
    let service = package.service().name();
    for platform in package.platforms() {
        // The failing build has no `.pkg`.
        let Some(name) = package.native_name(&release(), *platform) else {
            continue;
        };
        let bytes = match native_deb::arch(*platform) {
            Some(arch) => {
                let program = |path: &str| {
                    vec![native_hello::File {
                        path: path.to_owned(),
                        mode: 0o755,
                        data: b"a program".to_vec(),
                    }]
                };
                let files = match package {
                    Package::Hello => native_hello::deb_files(b"a program".to_vec(), false),
                    Package::HelloFailReady => native_hello::deb_files(b"a program".to_vec(), true),
                    Package::Hostkeeper => program("/usr/bin/hostkeeper"),
                    _ => program("/usr/lib/mdm/mdm"),
                };
                let version = package.deb_version(&release());
                native_deb::build(service, &version, arch, "a service", &files, 1_800_000_000)
                    .expect("build the .deb")
            }
            None => format!("the .pkg of {service}").into_bytes(),
        };
        std::fs::write(dist.join(&name), &bytes).expect("write the native package");
        let digest = file_sha256(&dist.join(&name)).expect("hash the native package");
        std::fs::write(dist.join(bundle_of(&name)), bundle_listing(&digest))
            .expect("write its bundle");
    }
}

/// The hook bounds, units, health URL and params of hello's manifest for `units` (`systemd` or
/// `launchd`), as a native entry's manifest names them in JSON; its hook command the program its
/// `.deb` (or, for `launchd`, its `.pkg`) installs.
fn hello_manifest(package: &str, run: [&str; 2], units: &str) -> Value {
    let program = match units {
        "launchd" => "/Library/Application Support/hello/bin/hello",
        _ => "/usr/lib/hello/hello",
    };
    json!({
        "package": package,
        "hook": {
            "command": [program, "update-hook"],
            "timeoutSecs": {
                "prepare": "30", "cutover": "60", "verify": "30", "rollback": "60",
                "cleanup": "30", "uninstall": "60",
            },
            "verifyDeadlineSecs": "120",
        },
        "run": run,
        "healthUrl": "http://127.0.0.1:9310/readyz",
        "params": {
            "port": {"integer": "9310"},
            "record_dir": {"text": "/var/lib/updater/hooks/hello"},
            "units": {"text": units},
        },
        "minHostkeeper": "0.1.0",
        "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
    })
}

/// The `macos-pkg` entry the verb must write for the `.pkg` `name` of `dist` with `manifest`: in
/// the store `komira-testing` at the installers' testing path, its digest and size, no
/// attestation.
fn expected_pkg(dist: &Path, name: &str, manifest: Value) -> Value {
    let bytes = std::fs::metadata(dist.join(name)).expect("the .pkg").len();
    json!({
        "installer": "macos-pkg",
        "locations": [{"store": "komira-testing", "locator": format!("hostkeeper/0.1.5/{name}")}],
        "digest": file_sha256(&dist.join(name)).expect("hash"),
        "bytes": bytes.to_string(),
        "manifest": manifest,
    })
}

/// The native entry the verb must write for hello's package of `dist` on `platform`: its `.deb`,
/// or on a Mac its `.pkg`.
fn expected_native(dist: &Path, platform: Platform) -> Value {
    let name = Package::Hello
        .native_name(&release(), platform)
        .expect("a native package");
    if platform == Platform::MacosArm64 {
        let manifest = hello_manifest("dev.hostkeeper.hello", ["hello.a", "hello.b"], "launchd");
        return expected_pkg(dist, &name, manifest);
    }
    let digest = file_sha256(&dist.join(&name)).expect("hash");
    json!({
        "installer": "apt",
        "digest": digest,
        "manifest": hello_manifest("hello", ["hello@a.service", "hello@b.service"], "systemd"),
        "repository": "komira-testing",
        "packageVersion": "0.1.5",
    })
}

/// The list of `native` alone, as the verb writes every platform's.
fn alone(native: Value) -> Value {
    json!({"entries": [native]})
}

fn request(dist: &Path, package: Package, output: Option<PathBuf>) -> Request {
    Request {
        package,
        version: release(),
        dir: dist.to_path_buf(),
        output,
    }
}

/// The JSON of the `targets=` line the verb answered.
fn answered(lines: &[String]) -> Value {
    let [line] = lines else {
        panic!("not one line: {lines:?}");
    };
    let json = line.strip_prefix("targets=").expect("a targets= line");
    serde_json::from_str(json).expect("JSON")
}

/// Reads `targets` back as the coordinator does (proto3 JSON, no unknown field), checks them by
/// its rules for `service`, and holds every platform's list to one native entry: `apt` on Linux,
/// `macos-pkg` on macOS, and no archive entry, which a release no longer lists.
fn read_back_as_the_coordinator(service: &str, targets: &Value) {
    let descriptor = TARGET_LIST.clone().expect("the descriptor");
    let options = DeserializeOptions::new().deny_unknown_fields(true);
    let mut lists = BTreeMap::new();
    let object = targets.as_object().expect("an object");
    for (platform, list) in object {
        let read: TargetList =
            DynamicMessage::deserialize_with_options(descriptor.clone(), list.clone(), &options)
                .expect("proto3 JSON")
                .transcode_to()
                .expect("a TargetList");
        let installers: Vec<&str> = read.entries.iter().map(|e| e.installer.as_str()).collect();
        let native = match platform.as_str() {
            "macos-arm64" => "macos-pkg",
            _ => "apt",
        };
        assert_eq!(installers, [native], "{platform}");
        lists.insert(platform.clone(), read);
    }
    let service = ServiceName::new(service).expect("a service");
    target::check(&service, &lists).expect("the coordinator's checks");
}

/// The native entry the verb must write for hostkeeper's own `.deb` on the Linux `platform` of
/// `dist`: the daemon's `.deb` from the testing suite, with its hook bounds, its run
/// unit, and its floor `floor`.
fn expected_hostkeeper_native(dist: &Path, platform: Platform, floor: &str) -> Value {
    let name = Package::Hostkeeper
        .native_name(&release(), platform)
        .expect("a native package");
    if platform == Platform::MacosArm64 {
        let manifest = json!({
            "package": "dev.hostkeeper.hostkeeper",
            "hook": {
                "timeoutSecs": {
                    "prepare": "300", "cutover": "300", "verify": "300", "rollback": "300",
                    "cleanup": "300",
                },
                "verifyDeadlineSecs": "600",
            },
            "minHostkeeper": floor,
            "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
        });
        return expected_pkg(dist, &name, manifest);
    }
    json!({
        "installer": "apt",
        "digest": file_sha256(&dist.join(&name)).expect("hash"),
        "manifest": {
            "package": "hostkeeper",
            "hook": {
                "timeoutSecs": {
                    "prepare": "300", "cutover": "300", "verify": "300", "rollback": "300",
                    "cleanup": "300",
                },
                "verifyDeadlineSecs": "600",
            },
            "run": ["hostkeeper.service"],
            "minHostkeeper": floor,
            "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
        },
        "repository": "komira-testing",
        "packageVersion": "0.1.5",
    })
}

#[test]
fn hostkeeper_lists_its_deb_on_linux_and_its_installer_on_macos_alone() {
    // Catches: hostkeeper's list with its archive entry still in it (stage 9 sets native entries
    // alone, and the channel that serves the archive ends) or without its native entry, no list
    // for macOS (a Mac whose daemon announces `macos-pkg` is then ordered nothing), a `.deb`
    // entry that is not a repository entry of the testing suite pinned by the daemon's `.deb`'s
    // own SHA-256 at the release's version, an installer entry whose location is not where the
    // installers' testing directory holds it, that carries an attestation, a repository or a
    // package version, or names hello's identifier, a native manifest with another floor than
    // `OLDEST_READER`, a hook command (its daemon runs built-in steps), or no run unit on Linux.
    let (_root, dist) = dist("targets-hostkeeper");
    built(&dist, Package::Hostkeeper);
    let floor = release().min(OLDEST_READER).to_string();
    let lines = targets(&request(&dist, Package::Hostkeeper, None)).expect("the targets");
    let got = answered(&lines);
    let entry = |platform: Platform| alone(expected_hostkeeper_native(&dist, platform, &floor));
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry(Platform::LinuxArm64),
            "linux-x86_64": entry(Platform::LinuxX86_64),
            "macos-arm64": entry(Platform::MacosArm64),
        })
    );
    read_back_as_the_coordinator("hostkeeper", &got);
}

#[test]
fn hostkeepers_native_entries_name_the_package_its_daemon_takes_as_its_own() {
    // Catches: a native entry of hostkeeper whose package its daemon does not take as its own
    // package (a `.pkg` identifier other than the daemon's launchd label, which the installer
    // builder and the daemon share, or a `.deb` package other than `hostkeeper`), so the daemon
    // would take the bootstrap install for another service's; a run unit that nfpm.yaml does not
    // install in the daemon's own `.deb`, or any on a Mac (its daemon job is no run unit); or a
    // hook command on it (its daemon runs built-in steps).
    let (_root, dist) = dist("targets-hostkeeper-own");
    built(&dist, Package::Hostkeeper);
    let got = answered(&targets(&request(&dist, Package::Hostkeeper, None)).expect("the targets"));
    for (platform, list) in got.as_object().expect("an object") {
        let native = &list["entries"][0]["manifest"];
        let package = native["package"].as_str().expect("a package");
        assert_eq!(
            native["hook"].get("command"),
            None,
            "{platform}: hostkeeper's daemon runs built-in steps"
        );
        if platform == "macos-arm64" {
            assert_eq!(
                package,
                crate::release::macos_pkg::identifier(),
                "{platform}"
            );
            assert!(
                hostkeeper_install::macos_pkg::own::is_own("hostkeeper", package),
                "the daemon takes {package} as its own package"
            );
            assert_eq!(native.get("run"), None, "{platform}: no run unit");
            continue;
        }
        assert_eq!(package, "hostkeeper", "{platform}");
        let run = native["run"].as_array().expect("run units");
        assert!(!run.is_empty(), "{platform}");
        let nfpm = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/nfpm.yaml"),
        )
        .expect("read nfpm.yaml");
        for unit in run {
            let file = format!(
                "dst: /usr/lib/systemd/system/{}\n",
                unit.as_str().expect("a unit")
            );
            assert!(nfpm.contains(&file), "nfpm.yaml installs no {file}");
        }
    }
}

#[test]
fn hello_lists_its_deb_on_linux_and_its_pkg_on_macos_alone() {
    // Catches: hello's list with its archive entry still in it or without its native entry, no
    // list for macOS, a `.pkg` entry that is not a `macos-pkg` entry of the installers' testing
    // directory pinned by the `.pkg`'s digest and size with hello's identifier, its launchd
    // labels and the program its `.pkg` installs, a `.deb` entry that is not a repository entry of the testing suite pinned
    // by the `.deb`'s own SHA-256 at the release's version, and a native manifest that is not
    // hello's (its hook bounds, run units, health URL, params and floor) with the hook command
    // naming the program the `.deb` installs, the package's name and the installer's bounds.
    let (_root, dist) = dist("targets-hello");
    built(&dist, Package::Hello);
    let got = answered(&targets(&request(&dist, Package::Hello, None)).expect("the targets"));
    let entry = |platform: Platform| alone(expected_native(&dist, platform));
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry(Platform::LinuxArm64),
            "linux-x86_64": entry(Platform::LinuxX86_64),
            "macos-arm64": entry(Platform::MacosArm64),
        })
    );
    read_back_as_the_coordinator("hello", &got);
}

#[test]
fn the_line_of_each_package_is_the_bytes_the_release_writes() {
    // Catches: any change of the JSON `release targets` writes for hostkeeper, hello or mdm at
    // one version: a field, a bound, a unit, the floor, the hook command, a key's order or the
    // spelling of a number, whatever the manifest is built from. The lines are the ones the
    // archive's manifest gave, with the hook command naming the program each service's `.deb`
    // installs.
    assert_eq!(
        line_of("targets-bytes-hostkeeper", Package::Hostkeeper),
        HOSTKEEPER_LINE
    );
    assert_eq!(line_of("targets-bytes-hello", Package::Hello), HELLO_LINE);
    assert_eq!(line_of("targets-bytes-mdm", Package::Mdm), MDM_LINE);
}

const HOSTKEEPER_LINE: &str = r#"targets={"linux-arm64":{"entries":[{"digest":"c920ce476ae57d152131e881c4a0af050a3ee25ff0ae85ee20697be9c6fd38af","installer":"apt","manifest":{"hook":{"timeoutSecs":{"cleanup":"300","cutover":"300","prepare":"300","rollback":"300","verify":"300"},"verifyDeadlineSecs":"600"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.5","package":"hostkeeper","run":["hostkeeper.service"]},"packageVersion":"0.1.5","repository":"komira-testing"}]},"linux-x86_64":{"entries":[{"digest":"109ccc2b56ef785db77ac86d6cff2a0e7e57d1a9444228ef39af7b8bd08d1551","installer":"apt","manifest":{"hook":{"timeoutSecs":{"cleanup":"300","cutover":"300","prepare":"300","rollback":"300","verify":"300"},"verifyDeadlineSecs":"600"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.5","package":"hostkeeper","run":["hostkeeper.service"]},"packageVersion":"0.1.5","repository":"komira-testing"}]},"macos-arm64":{"entries":[{"bytes":"22","digest":"40c982ccbdfd22b12c63216ad9e09c2cf53d28bda2ac471c45efae3950331e31","installer":"macos-pkg","locations":[{"locator":"hostkeeper/0.1.5/hostkeeper-0.1.5-macos-arm64.pkg","store":"komira-testing"}],"manifest":{"hook":{"timeoutSecs":{"cleanup":"300","cutover":"300","prepare":"300","rollback":"300","verify":"300"},"verifyDeadlineSecs":"600"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.5","package":"dev.hostkeeper.hostkeeper"}}]}}"#;
const HELLO_LINE: &str = r#"targets={"linux-arm64":{"entries":[{"digest":"3b1e27a59adf3654ac42e0a1ca31ba49f1691ac532e935c410fab31f01625cbc","installer":"apt","manifest":{"healthUrl":"http://127.0.0.1:9310/readyz","hook":{"command":["/usr/lib/hello/hello","update-hook"],"timeoutSecs":{"cleanup":"30","cutover":"60","prepare":"30","rollback":"60","uninstall":"60","verify":"30"},"verifyDeadlineSecs":"120"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.0","package":"hello","params":{"port":{"integer":"9310"},"record_dir":{"text":"/var/lib/updater/hooks/hello"},"units":{"text":"systemd"}},"run":["hello@a.service","hello@b.service"]},"packageVersion":"0.1.5","repository":"komira-testing"}]},"linux-x86_64":{"entries":[{"digest":"6ddf6fc1307c13d242c03a62b129a0bdf0e2338ecbb46ff3ce4905fcb513416d","installer":"apt","manifest":{"healthUrl":"http://127.0.0.1:9310/readyz","hook":{"command":["/usr/lib/hello/hello","update-hook"],"timeoutSecs":{"cleanup":"30","cutover":"60","prepare":"30","rollback":"60","uninstall":"60","verify":"30"},"verifyDeadlineSecs":"120"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.0","package":"hello","params":{"port":{"integer":"9310"},"record_dir":{"text":"/var/lib/updater/hooks/hello"},"units":{"text":"systemd"}},"run":["hello@a.service","hello@b.service"]},"packageVersion":"0.1.5","repository":"komira-testing"}]},"macos-arm64":{"entries":[{"bytes":"17","digest":"00315b0fd629b9b44612483c354b4c4175ecc94ca2237afd7ed01e5a3b4df761","installer":"macos-pkg","locations":[{"locator":"hostkeeper/0.1.5/hello-0.1.5-macos-arm64.pkg","store":"komira-testing"}],"manifest":{"healthUrl":"http://127.0.0.1:9310/readyz","hook":{"command":["/Library/Application Support/hello/bin/hello","update-hook"],"timeoutSecs":{"cleanup":"30","cutover":"60","prepare":"30","rollback":"60","uninstall":"60","verify":"30"},"verifyDeadlineSecs":"120"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.0","package":"dev.hostkeeper.hello","params":{"port":{"integer":"9310"},"record_dir":{"text":"/var/lib/updater/hooks/hello"},"units":{"text":"launchd"}},"run":["hello.a","hello.b"]}}]}}"#;
const MDM_LINE: &str = r#"targets={"linux-x86_64":{"entries":[{"digest":"0c3015630430a4856eecca558013c9d74bd58e4234e67d7e834b21a759b1ae4d","installer":"apt","manifest":{"healthUrl":"http://127.0.0.1:9004/version","hook":{"command":["/usr/lib/mdm/mdm","update-hook"],"timeoutSecs":{"cleanup":"30","cutover":"120","prepare":"300","rollback":"120","uninstall":"120","verify":"60"},"verifyDeadlineSecs":"600"},"installerBounds":{"activateSecs":"300","removeSecs":"300","stageSecs":"600"},"minHostkeeper":"0.1.5","package":"mdm","params":{"record_dir":{"text":"/var/lib/updater/hooks/mdm"},"settings":{"text":"/etc/mdm/settings"}},"run":["mdm-scep.service","mdm-nanohub.service","mdm-depserver.service"]},"packageVersion":"0.1.5","repository":"komira-testing"}]}}"#;

/// The `targets=` line the verb answers for `package` at the release 0.1.5, in a new `dist`
/// whose packages are built as the release builds them.
fn line_of(name: &str, package: Package) -> String {
    let (_root, dist) = dist(name);
    built(&dist, package);
    let lines = targets(&request(&dist, package, None)).expect("the targets");
    let [line] = lines.as_slice() else {
        panic!("not one line: {lines:?}");
    };
    line.clone()
}

#[test]
fn the_line_is_appended_to_the_output_file() {
    // Catches: the output file overwritten (GITHUB_OUTPUT holds the other steps' outputs), or
    // the line written there differing from the one printed.
    let (root, dist) = dist("targets-output");
    built(&dist, Package::Hello);
    let output = root.join("github_output");
    std::fs::write(&output, "before=1\n").expect("write the output file");
    let lines =
        targets(&request(&dist, Package::Hello, Some(output.clone()))).expect("the targets");
    let written = std::fs::read_to_string(&output).expect("read the output file");
    assert_eq!(written, format!("before=1\n{}\n", lines[0]));
}

#[test]
fn mdm_gets_its_one_platform_its_native_entry_alone_and_the_floor_of_hostkeepers_release() {
    // Catches: a platform the package is not released for given an entry (mdm's .deb is amd64
    // alone), mdm's list without its native entry, a native manifest that is not mdm's (its hook
    // bounds, run units, health URL and params), or a floor other than the oldest reader of the
    // release.
    let (_root, dist) = dist("targets-mdm");
    built(&dist, Package::Mdm);
    let got = answered(&targets(&request(&dist, Package::Mdm, None)).expect("the targets"));
    let deb = "mdm_0.1.5_amd64.deb";
    let native = json!({
        "installer": "apt",
        "digest": file_sha256(&dist.join(deb)).expect("hash"),
        "manifest": {
            "package": "mdm",
            "hook": {
                "command": ["/usr/lib/mdm/mdm", "update-hook"],
                "timeoutSecs": {
                    "prepare": "300", "cutover": "120", "verify": "60", "rollback": "120",
                    "cleanup": "30", "uninstall": "120",
                },
                "verifyDeadlineSecs": "600",
            },
            "run": ["mdm-scep.service", "mdm-nanohub.service", "mdm-depserver.service"],
            "healthUrl": "http://127.0.0.1:9004/version",
            "params": {
                "record_dir": {"text": "/var/lib/updater/hooks/mdm"},
                "settings": {"text": "/etc/mdm/settings"},
            },
            "minHostkeeper": "0.1.5",
            "installerBounds": {"stageSecs": "600", "activateSecs": "300", "removeSecs": "300"},
        },
        "repository": "komira-testing",
        "packageVersion": "0.1.5",
    });
    assert_eq!(got, json!({"linux-x86_64": alone(native)}));
    read_back_as_the_coordinator("mdm", &got);
}

#[test]
fn every_problem_of_the_release_is_named_at_once() {
    // Catches: a native package missing, or a native package's bundle over the daemon's cap,
    // given an entry; or the verb stopping at the first problem, so a release with several is
    // fixed one run at a time; macos-arm64's `.pkg` and its bundle gone.
    let (_root, dist) = dist("targets-every-problem");
    built(&dist, Package::Hello);
    // linux-arm64: its .deb's bundle over 1 MiB.
    let deb = Package::Hello
        .native_name(&release(), Platform::LinuxArm64)
        .expect("a .deb");
    let arm_bundle = bundle_of(&deb);
    std::fs::write(dist.join(&arm_bundle), vec![b' '; (1 << 20) + 1]).expect("rewrite");
    // linux-x86_64: no .deb.
    let x86 = Package::Hello
        .native_name(&release(), Platform::LinuxX86_64)
        .expect("a .deb");
    std::fs::remove_file(dist.join(&x86)).expect("remove");
    let pkg = Package::Hello
        .native_name(&release(), Platform::MacosArm64)
        .expect("a .pkg");
    std::fs::remove_file(dist.join(&pkg)).expect("remove");
    std::fs::remove_file(dist.join(bundle_of(&pkg))).expect("remove");
    assert_eq!(
        targets(&request(&dist, Package::Hello, None)),
        Err(Error::Refused(vec![
            format!(
                "{arm_bundle}: longer than 1048576 bytes, the most the daemon reads of a bundle"
            ),
            format!(
                "{x86}: missing: the release ships the native package of every platform it lists"
            ),
            format!(
                "{pkg}: missing: the release ships the native package of every platform it lists"
            ),
            format!(
                "{}: missing: every native package carries its attestation bundle",
                bundle_of(&pkg)
            ),
        ]))
    );
}

/// One valid entry of hello for linux-x86_64, as the verb writes it: its `.deb` from the
/// testing suite.
fn an_entry() -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: "a".repeat(64),
        manifest: Some(Manifest {
            package: Some("hello".to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(StepBounds {
                    cutover: 60,
                    verify: 30,
                    rollback: 60,
                    cleanup: 30,
                    ..StepBounds::default()
                }),
                verify_deadline_secs: 120,
                ..Hook::default()
            }),
            min_hostkeeper: "0.1.0".to_owned(),
            installer_bounds: Some(INSTALLER_BOUNDS),
            ..Manifest::default()
        }),
        repository: Some(crate::release::contract::TESTING_ENTRY.to_owned()),
        package_version: Some("0.1.5".to_owned()),
        ..Target::default()
    }
}

#[test]
fn entries_the_coordinator_or_the_daemon_would_refuse_are_never_written() {
    // Catches: entries sent on that the coordinator refuses (`target_invalid`, failing the
    // whole set) or the daemon refuses (`manifest_invalid`), found only once CI has called.
    let hello = ServiceName::new("hello").expect("a service");
    let one = |entry: Target| [(IndexPlatform::LinuxX86_64, vec![(Installer::Apt, entry)])];
    assert_eq!(checked(&hello, &one(an_entry())), Ok(()));

    let mut long = an_entry();
    long.repository = Some("r".repeat(2048));
    let Err(Error::Failed(why)) = checked(&hello, &one(long)) else {
        panic!("a repository name too long is written");
    };
    assert!(
        why.starts_with("target_invalid: entry 0 of linux-x86_64: "),
        "{why}"
    );

    let mut more = an_entry();
    if let Some(manifest) = more.manifest.as_mut() {
        manifest.hook = None;
    }
    let Err(Error::Failed(why)) = checked(&hello, &one(more)) else {
        panic!("a manifest with no hook is written");
    };
    assert!(
        why.starts_with("manifest_invalid: linux-x86_64: apt: "),
        "{why}"
    );
}

/// The argv of the `release` command, as clap reads it.
#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    verb: Verb,
}

fn verb(dist: &Path, extra: &[&str]) -> Verb {
    let dir = dist.display().to_string();
    let argv = [
        "release",
        "targets",
        "--version",
        "0.1.5",
        "--dir",
        dir.as_str(),
        "--repo",
        REPO,
    ];
    Args::try_parse_from(argv.iter().chain(extra))
        .map(|a| a.verb)
        .expect("parse the argv")
}

#[test]
fn the_verb_runs_without_adapters_and_refuses_a_failing_build_of_hostkeeper() {
    // Catches: the verb building the real adapters (and so needing `gh` and `git`) though it
    // reads files only, `run_with` failing it instead of handing it back, its flags not reaching
    // it (`--repo` is still taken, and changes nothing), or `--fail-ready` taken for a service
    // with no failing build.
    let (_root, dist) = dist("targets-verb");
    built(&dist, Package::Hello);
    let connected = std::cell::Cell::new(false);
    let hello = crate::release::run(verb(&dist, &["--service", "hello"]), || {
        connected.set(true);
        Fakes::over(FakeHistory::default())
    });
    assert_eq!(hello, targets(&request(&dist, Package::Hello, None)));
    assert!(hello.is_ok());
    assert!(!connected.get());

    let fakes = Fakes::over(FakeHistory::default());
    let lent = crate::release::run_with(verb(&dist, &["--service", "hello"]), &fakes.ports());
    assert_eq!(lent, hello);

    let fail_ready = crate::release::run(verb(&dist, &["--fail-ready"]), || {
        Fakes::over(FakeHistory::default())
    });
    assert_eq!(
        fail_ready,
        Err(Error::Failed(package::FAIL_READY_ONLY_HELLO.to_owned()))
    );
    let service = Args::try_parse_from(["release", "targets", "--service", "tierstore"]);
    assert_eq!(
        service.err().map(|e| e.kind()),
        Some(clap::error::ErrorKind::InvalidValue)
    );
    assert_eq!(Service::default(), Service::Hostkeeper);
}

mod fail_ready;
mod native;
