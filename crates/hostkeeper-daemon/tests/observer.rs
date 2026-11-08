//! The daemon's observer: what the daemon's status holds of how each installed service runs, that a
//! status read never waits for a running step, which executable a version seals, and the
//! observer of a host.

mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::Reply;
use hostkeeper_daemon::observer::{
    OwnService, STATUS_DEADLINE, SealedExe, host_observer, observe, observer_with, service_health,
    service_input,
};
use hostkeeper_daemon::selfunits::MAIN_UNIT;
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::testing::{manifest, spawn};
use hostkeeper_hook::Step;
use hostkeeper_launchd::Label;
use hostkeeper_observe::{
    ActiveState, Current, ExeStat, ExpectedExe, HealthView, Observation, ServiceInput,
    ServiceState as Observed, UnhealthyReason, UnobservedReason, VersionView, beat_health,
};
use hostkeeper_proto::hostkeeper::agent::v1::Health;
use support::{OWN, Rig, d, update};

/// The SHA-256 of the three bytes `abc`.
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

/// The digest the running executable of the tests' live service has.
const RUNNING: [u8; 32] = [7; 32];

/// The health URL of the tests' services.
const URL: &str = "http://127.0.0.1:9301/healthz";

/// `ABC` as bytes.
fn abc() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (at, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&ABC[at * 2..at * 2 + 2], 16).expect("hex");
    }
    out
}

/// The daemon of `rig` running as `unit`, its binary directory under the rig's state directory.
fn own_service(rig: &Rig, unit: &str) -> OwnService {
    OwnService {
        unit: unit.to_owned(),
        binary_dir: binary_dir(&rig.state),
    }
}

fn name(text: &str) -> ServiceName {
    ServiceName::new(text).expect("a service name")
}

/// `service` at `version` with one run unit, `<service>.service`, and `current` naming it; its
/// manifest has the health URL [`URL`] when `health` is set.
fn installed(rig: &Rig, service: &str, version: &str, health: bool) {
    let unit = format!("{service}.service");
    let dir = rig.add_units(service, version, &[unit.as_str()], &[unit.as_str()], &[]);
    if health {
        let path = dir.join("manifest.toml");
        let text = fs::read_to_string(&path).expect("the manifest");
        fs::write(&path, format!("{text}\n[health]\nurl = \"{URL}\"\n")).expect("add the URL");
    }
    rig.store.link(service, "current", version);
}

fn stat() -> ExeStat {
    ExeStat {
        size: 10,
        mtime_secs: 20,
        mtime_nanos: 30,
    }
}

/// Catches an observation dropped, read for the wrong service or left off the wire: two
/// installed services, one live with a confirmed version and a healthy probe and one stopped,
/// are each reported in the library's observation with its reason token and in the health the
/// heartbeat reports; a service with no `current` has no observation and no health.
#[test]
fn status_reports_the_observation_and_the_installed_version_per_service() {
    let rig = Rig::new("observer-status");
    installed(&rig, "live", "1.0.0", true);
    installed(&rig, "stopped", "1.0.0", false);
    rig.stage("bare", "1.0.0");
    rig.manager
        .set("live.service", ActiveState::Active, Some(4242));
    rig.process.set(4242, 77, stat(), RUNNING);
    rig.expected.seal("live", "1.0.0", RUNNING);
    rig.health.answer(URL, 200);
    rig.manager
        .set("stopped.service", ActiveState::Inactive, None);
    let daemon = Arc::new(rig.daemon());

    let status = daemon.status().expect("status");
    let [bare, live, stopped] = &status.services[..] else {
        panic!("three services");
    };
    assert_eq!(
        live.observation,
        Some(Observation {
            service: "live".to_owned(),
            state: Observed::Live,
            version: VersionView::Recorded {
                version: "1.0.0".to_owned(),
                confirmed: true,
            },
            health: HealthView::Healthy,
            recorded: Some("1.0.0".to_owned()),
        })
    );
    assert_eq!(
        live.observation
            .as_ref()
            .and_then(Observation::reason_token),
        None
    );
    assert_eq!(
        stopped.observation,
        Some(Observation {
            service: "stopped".to_owned(),
            state: Observed::Stopped,
            version: VersionView::NotLive,
            health: HealthView::NotProbed,
            recorded: Some("1.0.0".to_owned()),
        })
    );
    assert_eq!(
        stopped
            .observation
            .as_ref()
            .and_then(Observation::reason_token),
        Some("stopped")
    );
    assert_eq!(bare.observation, None);
    assert_eq!(
        service_health(live),
        Some((Health::Healthy, Some("1.0.0".to_owned())))
    );
    assert_eq!(
        service_health(stopped),
        Some((Health::Unhealthy, Some("1.0.0".to_owned())))
    );
    assert_eq!(service_health(bare), None);
    assert_eq!(rig.health.requests(), [URL]);
}

/// Catches an observation that takes the service's lock or the step lock: a step of `web` is
/// held in `prepare` while an update runs, and `status` answers with the observation of the live
/// service, the lock shown as held, before the step is released.
#[test]
fn an_observation_never_waits_for_a_running_step() {
    let rig = Rig::new("observer-unlocked");
    rig.add_units(
        "web",
        support::BASE,
        &["web.service"],
        &["web.service"],
        &[],
    );
    rig.store.link("web", "current", support::BASE);
    rig.add("web", "2.0.0");
    rig.manager
        .set("web.service", ActiveState::Active, Some(4242));
    rig.process.set(4242, 77, stat(), RUNNING);
    rig.expected.seal("web", support::BASE, RUNNING);
    let gate = rig.launcher.hold("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let running = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || {
            daemon.order(&update("web", "2.0.0", &d(), 1))
        })
    };
    gate.entered(&running).expect("the held step is entered");

    let status = daemon.status().expect("status");
    let [web] = &status.services[..] else {
        panic!("one service");
    };
    assert!(web.locked & web.step_running());
    assert_eq!(
        web.observation,
        Some(Observation {
            service: "web".to_owned(),
            state: Observed::Live,
            version: VersionView::Recorded {
                version: support::BASE.to_owned(),
                confirmed: true,
            },
            health: HealthView::NotProbed,
            recorded: Some(support::BASE.to_owned()),
        })
    );
    assert!(Arc::ptr_eq(daemon.observer(), &rig.observer));

    gate.release();
    let reply = running.join().expect("the update");
    assert!(!matches!(reply, Reply::Refused { .. }), "{reply:?}");
}

/// Catches a service observed when it should not be: one with no `current`, one whose `current`
/// names a version whose manifest does not load, and one whose manifest is gone have no input;
/// one whose manifest loads has the version `current` names as its recorded version and the
/// units and health URL of its manifest.
#[test]
fn a_service_with_no_current_or_an_unreadable_manifest_is_not_observed() {
    let rig = Rig::new("observer-input");
    let observer = &rig.observer;
    let (packages, web) = (&rig.packages, name("web"));
    let bin = &binary_dir(&rig.state);
    rig.stage("web", "1.0.0");
    assert_eq!(service_input(packages, bin, &OWN, &web), None);
    assert_eq!(observe(observer, packages, bin, &OWN, &web), None);

    rig.store.link("web", "current", "1.0.0");
    assert_eq!(
        service_input(packages, bin, &OWN, &web),
        Some(ServiceInput {
            service: "web".to_owned(),
            current: Some(Current {
                run: Vec::new(),
                health_url: None,
            }),
            recorded_version: Some("1.0.0".to_owned()),
        })
    );

    installed(&rig, "api", "1.0.0", true);
    assert_eq!(
        service_input(packages, bin, &OWN, &name("api")).and_then(|input| input.current),
        Some(Current {
            run: vec!["api.service".to_owned()],
            health_url: Some(URL.to_owned()),
        })
    );

    fs::write(packages.join("web/1.0.0/manifest.toml"), "{").expect("break the manifest");
    assert_eq!(service_input(packages, bin, &OWN, &web), None);
    rig.store.link("web", "current", "9.9.9");
    assert_eq!(service_input(packages, bin, &OWN, &web), None);
}

/// A `Live` observation with a recorded version `1.0.0`.
fn live(version: VersionView, health: HealthView) -> Observation {
    observation(Observed::Live, version, health)
}

fn observation(state: Observed, version: VersionView, health: HealthView) -> Observation {
    Observation {
        service: "web".to_owned(),
        state,
        version,
        health,
        recorded: Some("1.0.0".to_owned()),
    }
}

fn confirmed() -> VersionView {
    VersionView::Recorded {
        version: "1.0.0".to_owned(),
        confirmed: true,
    }
}

fn unconfirmed() -> VersionView {
    VersionView::Recorded {
        version: "1.0.0".to_owned(),
        confirmed: false,
    }
}

fn unhealthy(reason: UnhealthyReason) -> HealthView {
    HealthView::Unhealthy { reason }
}

fn unobserved(reason: UnobservedReason) -> Observed {
    Observed::Unobserved {
        reason,
        detail: "why".to_owned(),
    }
}

/// Catches an observation put in the wrong heartbeat field: each state, version view and health
/// view has its `Health` and the version the beat carries (the recorded one, none for an absent
/// service), and its reason token by the rules of `Observation::reason_token`.
#[test]
fn the_heartbeat_health_and_the_reason_say_what_the_observation_says() {
    let mismatch = VersionView::Mismatch {
        recorded: "1.0.0".to_owned(),
        observed_note: "another executable".to_owned(),
    };
    let none = VersionView::NotLive;
    let not_probed = HealthView::NotProbed;
    let v = Some("1.0.0");
    // The observation, then the health and version of the beat and the reason token.
    let table = [
        (
            observation(Observed::Absent, none.clone(), not_probed.clone()),
            (Health::Absent, None, Some("absent")),
        ),
        (
            observation(Observed::Stopped, none.clone(), not_probed.clone()),
            (Health::Unhealthy, v, Some("stopped")),
        ),
        (
            observation(Observed::Transitioning, none.clone(), not_probed.clone()),
            (Health::Unhealthy, v, Some("transitioning")),
        ),
        (
            observation(
                unobserved(UnobservedReason::ManagerError),
                none.clone(),
                not_probed.clone(),
            ),
            (Health::Unhealthy, v, Some("unobserved")),
        ),
        (
            observation(
                unobserved(UnobservedReason::ChangedDuringRound),
                none.clone(),
                not_probed.clone(),
            ),
            (Health::Unhealthy, v, Some("unobserved")),
        ),
        (
            observation(Observed::Unmonitored, none.clone(), HealthView::Healthy),
            (Health::Healthy, v, Some("unmonitored")),
        ),
        (
            observation(
                Observed::Unmonitored,
                none,
                unhealthy(UnhealthyReason::Unreachable),
            ),
            (Health::Unhealthy, v, Some("unreachable")),
        ),
        (
            live(confirmed(), HealthView::Healthy),
            (Health::Healthy, v, None),
        ),
        (
            live(unconfirmed(), not_probed.clone()),
            (Health::Healthy, v, Some("version_unconfirmed")),
        ),
        (
            live(mismatch.clone(), HealthView::Healthy),
            (Health::Unhealthy, v, Some("version_mismatch")),
        ),
        (
            live(mismatch, unhealthy(UnhealthyReason::StatusNon2xx)),
            (Health::Unhealthy, v, Some("version_mismatch")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::StatusNon2xx)),
            (Health::Unhealthy, v, Some("status_non_2xx")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::Unreachable)),
            (Health::Unhealthy, v, Some("unreachable")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::Redirect)),
            (Health::Unhealthy, v, Some("redirect")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::UrlInvalid)),
            (Health::Unhealthy, v, Some("url_invalid")),
        ),
        (
            live(VersionView::Unknown, not_probed.clone()),
            (Health::Healthy, v, Some("version_unknown")),
        ),
        (
            Observation {
                recorded: None,
                ..live(VersionView::Unknown, not_probed)
            },
            (Health::Healthy, None, Some("version_unknown")),
        ),
    ];
    for (observation, (health, version, reason)) in table {
        assert_eq!(
            (beat_health(&observation), observation.reason_token()),
            ((health, version.map(str::to_owned)), reason),
            "{observation:?}"
        );
    }
}

/// Makes the package `web` at 1.0.0 that places a unit whose executable is `bin/hook`, holding
/// the three bytes `abc`.
fn sealed_package(rig: &Rig) {
    let dir = rig.placed("web", "1.0.0", None);
    fs::write(dir.join("bin/hook"), "abc").expect("the executable");
}

/// The `SealedExe` of the rig, whose units spell the packages directory `/packages`.
fn sealed(rig: &Rig) -> SealedExe {
    SealedExe::new(&rig.packages, Path::new("/packages"), OWN)
}

/// Sets the unit file of `web`'s `run` unit to `text`.
fn set_unit(rig: &Rig, file: &str, text: &str) {
    fs::write(rig.packages.join("web/1.0.0/units").join(file), text).expect("the unit file");
}

/// Catches an executable digest taken of the wrong file: the digest is that of the file the
/// `ExecStart=` of the first run unit names, at that place under the version's own directory,
/// whatever systemd prefix is before it and however many arguments follow.
#[test]
fn the_sealed_digest_is_that_of_the_file_exec_start_names() {
    let rig = Rig::new("sealed-digest");
    sealed_package(&rig);
    let exe = sealed(&rig);
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
    for line in [
        "ExecStart=/packages/web/current/bin/hook serve --flag",
        "ExecStart=-@/packages/web/current/bin/hook",
        "  ExecStart=/packages/web/current/bin//./hook",
    ] {
        set_unit(&rig, "web.service", &format!("[Service]\n{line}\n"));
        assert_eq!(exe.digest("web", "1.0.0"), Some(abc()), "{line}");
    }
}

/// Catches an instance read from a unit file it does not have: the run unit
/// `web@9301.service` is started from the template `units/web@.service`, whose `ExecStart=`
/// names the executable.
#[test]
fn an_instance_of_a_template_has_the_digest_of_the_templates_executable() {
    let rig = Rig::new("sealed-template");
    let dir = rig.add_units(
        "web",
        "1.0.0",
        &["web@.service"],
        &["web@9301.service"],
        &[],
    );
    fs::write(dir.join("bin/hook"), "abc").expect("the executable");
    set_unit(
        &rig,
        "web@.service",
        "[Service]\nExecStart=/packages/web/current/bin/hook %i\n",
    );
    assert_eq!(sealed(&rig).digest("web", "1.0.0"), Some(abc()));
}

/// Catches a digest given for a command that is not the version's own executable: a program
/// outside the packages, another service's, a version's own path, a `..` that leaves `current`,
/// `current` itself, a directory, a missing file, a relative path, an empty command and a unit
/// with no command each have none.
#[test]
fn a_command_that_is_not_a_file_through_current_has_no_digest() {
    let rig = Rig::new("sealed-refused");
    sealed_package(&rig);
    fs::create_dir_all(rig.packages.join("other/current/bin")).expect("another service");
    let exe = sealed(&rig);
    for line in [
        "ExecStart=/usr/bin/true",
        "ExecStart=/packages/other/current/bin/hook",
        "ExecStart=/packages/web/1.0.0/bin/hook",
        "ExecStart=/packages/web/current/../1.0.0/bin/hook",
        "ExecStart=/packages/web/current/bin/../../1.0.0/bin/hook",
        "ExecStart=/packages/web/current",
        "ExecStart=/packages/web/current/bin",
        "ExecStart=/packages/web/current/bin/nothing",
        "ExecStart=bin/hook",
        "ExecStart=",
        "ExecStart=-",
        "Description=no command",
    ] {
        set_unit(&rig, "web.service", &format!("[Service]\n{line}\n"));
        assert_eq!(exe.digest("web", "1.0.0"), None, "{line}");
    }
}

/// Catches a digest given for a version that cannot be trusted or has no executable: one with no
/// seal, one with no run unit, one whose manifest does not load, a name or version that is not
/// one, and a version that is not there.
#[test]
fn a_version_with_no_seal_no_run_unit_or_no_manifest_has_no_digest() {
    let rig = Rig::new("sealed-absent");
    sealed_package(&rig);
    let exe = sealed(&rig);
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
    assert_eq!(exe.digest("web", "2.0.0"), None);
    assert_eq!(exe.digest("Bad Name", "1.0.0"), None);
    assert_eq!(exe.digest("web", "../1.0.0"), None);

    rig.add_with("plain", "1.0.0", &manifest("plain", "1.0.0"));
    assert_eq!(exe.digest("plain", "1.0.0"), None);

    fs::write(rig.packages.join("web/1.0.0/manifest.toml"), "{").expect("break the manifest");
    assert_eq!(exe.digest("web", "1.0.0"), None);

    sealed_package(&rig);
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
    fs::remove_file(rig.packages.join("web/1.0.0.seal")).expect("remove the seal");
    assert_eq!(exe.digest("web", "1.0.0"), None);
}

/// The example service's launchd plist for its slot `a`, whose first `ProgramArguments` string
/// is `/var/lib/hostkeeper/packages/hello/current/bin/hello`.
const HELLO_PLIST: &str = include_str!("../../../examples/hello/package/macos/units/hello.a.plist");

/// Makes the package `web` at 1.0.0 whose one run unit is the launchd label `web`, started from
/// `units/web.plist` holding `plist`, with the executable `bin/hook` holding the three bytes
/// `abc`; `current` names it.
fn mac_package(rig: &Rig, plist: &str) {
    let dir = rig.add_units("web", "1.0.0", &["web.plist"], &["web"], &[]);
    fs::create_dir_all(dir.join("bin")).expect("the bin directory");
    fs::write(dir.join("bin/hook"), "abc").expect("the executable");
    set_unit(rig, "web.plist", plist);
    rig.store.link("web", "current", "1.0.0");
}

/// A launchd plist whose `ProgramArguments` array starts with `first`.
fn arguments_plist(first: &str) -> String {
    format!(
        "<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>web</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{first}</string>\n\t\t<string>--port</string>\n\t</array>\n</dict>\n</plist>\n"
    )
}

/// A launchd plist with a `Program` of `program` and no `ProgramArguments`.
fn program_plist(program: &str) -> String {
    format!(
        "<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>web</string>\n\t<key>Program</key>\n\t<string>{program}</string>\n</dict>\n</plist>\n"
    )
}

/// Catches a Mac service whose version is never confirmed: the example service's own plist,
/// whose first `ProgramArguments` string goes through `current` as a Mac spells the packages
/// directory, has the digest of `bin/hello` under the version's directory.
#[test]
fn the_example_services_plist_has_the_digest_of_its_first_argument() {
    let rig = Rig::new("sealed-hello-plist");
    let dir = rig.add_units("hello", "1.2.3", &["hello.a.plist"], &["hello.a"], &[]);
    fs::write(dir.join("units/hello.a.plist"), HELLO_PLIST).expect("the plist");
    fs::create_dir_all(dir.join("bin")).expect("the bin directory");
    fs::write(dir.join("bin/hello"), "abc").expect("the executable");
    let exe = SealedExe::new(
        &rig.packages,
        Path::new("/var/lib/hostkeeper/packages"),
        OWN,
    );
    assert_eq!(exe.digest("hello", "1.2.3"), Some(abc()));
}

/// Catches a plist read for the wrong key: the digest is that of the file the first
/// `ProgramArguments` string names, or the `Program` string when there is no array, and the
/// `Program` string when there are both, as launchd starts it.
#[test]
fn a_plist_has_the_digest_of_its_program_else_its_first_argument() {
    let rig = Rig::new("sealed-plist");
    mac_package(&rig, &arguments_plist("/packages/web/current/bin/hook"));
    let exe = sealed(&rig);
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
    set_unit(
        &rig,
        "web.plist",
        &program_plist("/packages/web/current/bin//./hook"),
    );
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
    let both = arguments_plist("/packages/web/current/bin/nothing").replace(
        "<key>Label</key>",
        "<key>Program</key><string>/packages/web/current/bin/hook</string><key>Label</key>",
    );
    set_unit(&rig, "web.plist", &both);
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
}

/// Catches a plist's program given a digest when it is not the version's own executable, by the
/// same rules as a systemd unit's: a program outside the packages, another service's, a
/// version's own path, a `..` that leaves `current` or stays under it, `current` itself, a
/// directory, a missing file, a relative path and an empty string have none, by either key; so
/// has a plist with neither key, one whose `Program` is not a string, and one that is not text.
#[test]
fn a_plist_program_that_is_not_a_file_through_current_has_no_digest() {
    let rig = Rig::new("sealed-plist-refused");
    mac_package(&rig, &arguments_plist("/packages/web/current/bin/hook"));
    fs::create_dir_all(rig.packages.join("other/current/bin")).expect("another service");
    let exe = sealed(&rig);
    assert_eq!(exe.digest("web", "1.0.0"), Some(abc()));
    for path in [
        "/usr/bin/true",
        "/packages/other/current/bin/hook",
        "/packages/web/1.0.0/bin/hook",
        "/packages/web/current/../1.0.0/bin/hook",
        "/packages/web/current/bin/../hook",
        "/packages/web/current/bin/../../1.0.0/bin/hook",
        "/packages/web/current",
        "/packages/web/current/bin",
        "/packages/web/current/bin/nothing",
        "bin/hook",
        "",
    ] {
        set_unit(&rig, "web.plist", &arguments_plist(path));
        assert_eq!(exe.digest("web", "1.0.0"), None, "ProgramArguments {path}");
        set_unit(&rig, "web.plist", &program_plist(path));
        assert_eq!(exe.digest("web", "1.0.0"), None, "Program {path}");
    }
    let neither = "<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>web</string>\n</dict>\n</plist>\n";
    set_unit(&rig, "web.plist", neither);
    assert_eq!(exe.digest("web", "1.0.0"), None);
    let not_a_string = arguments_plist("/packages/web/current/bin/hook").replace(
        "<key>Label</key>",
        "<key>Program</key><true/><key>Label</key>",
    );
    set_unit(&rig, "web.plist", &not_a_string);
    assert_eq!(exe.digest("web", "1.0.0"), None);
    fs::write(rig.packages.join("web/1.0.0/units/web.plist"), [0xff, 0xfe]).expect("not text");
    assert_eq!(exe.digest("web", "1.0.0"), None);
}

/// Catches a Mac service reported `version_unconfirmed` while it runs its sealed executable:
/// the observer of the daemon, over the package tree, confirms the recorded version of a live
/// launchd job whose process runs the file the plist names, and reports a mismatch, not an
/// unconfirmed version, when the process runs another file.
#[test]
fn the_observer_confirms_the_version_of_a_mac_service() {
    let rig = Rig::new("observer-mac");
    let program = rig.packages.join("web/current/bin/hook");
    mac_package(&rig, &arguments_plist(&program.display().to_string()));
    rig.manager.set("web", ActiveState::Active, Some(4242));
    rig.process.set(4242, 77, stat(), abc());
    let observer = observer_with(
        Box::new(rig.manager.clone()),
        Box::new(rig.process.clone()),
        &rig.packages,
        OWN,
        &own_service(&rig, &Label::daemon().to_string()),
    );

    let seen = observe(
        &observer,
        &rig.packages,
        &binary_dir(&rig.state),
        &OWN,
        &name("web"),
    )
    .expect("observed");
    assert_eq!(
        (seen.state.clone(), seen.version.clone()),
        (
            Observed::Live,
            VersionView::Recorded {
                version: "1.0.0".to_owned(),
                confirmed: true,
            }
        )
    );
    assert_eq!(seen.reason_token(), None);

    rig.process.set(4243, 78, stat(), RUNNING);
    rig.manager.set("web", ActiveState::Active, Some(4243));
    let seen = observe(
        &observer,
        &rig.packages,
        &binary_dir(&rig.state),
        &OWN,
        &name("web"),
    )
    .expect("observed");
    assert_eq!(seen.reason_token(), Some("version_mismatch"));
}

/// Catches an observer of the host that reads the host while it is made, or is wired to nothing:
/// it is made from the packages directory, answers `absent` for a service with no `current` and
/// `unmonitored` for one that runs no unit, asking the service manager, the processes and the
/// health URL nothing.
#[test]
fn the_host_observer_answers_without_asking_the_host() {
    let rig = Rig::new("host-observer");
    let observer = host_observer(&rig.packages, OWN, &own_service(&rig, MAIN_UNIT));
    let input = |current: Option<Current>| ServiceInput {
        service: "web".to_owned(),
        current,
        recorded_version: None,
    };
    let quiet = Current {
        run: Vec::new(),
        health_url: None,
    };
    assert_eq!(
        observer.observe(&input(None), STATUS_DEADLINE).state,
        Observed::Absent
    );
    assert_eq!(
        observer.observe(&input(Some(quiet)), STATUS_DEADLINE).state,
        Observed::Unmonitored
    );
    assert_eq!(
        (STATUS_DEADLINE.connect, STATUS_DEADLINE.read),
        (
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1)
        )
    );
}
