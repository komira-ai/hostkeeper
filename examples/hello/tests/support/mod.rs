//! What the integration tests share: a scratch world for the hook (record directory, staged
//! release, params file), fakes for the two ports, and the checks of the contract.

#![allow(dead_code, reason = "each test binary uses some of the helpers")]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hello::hook::Units;
use hello::ports::{Host, Probe, Runner};
use hostkeeper_hook::{Class, Context, Next, Outcome, Params, Report, Scalar, Step};
use hostkeeper_testkit::Scratch;

pub const PORT: u16 = 9310;
pub const TARGET: &str = "1.2.3";

pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// A runner that records what it is asked to run and answers from a script.
#[derive(Default)]
pub struct FakeRunner {
    pub calls: RefCell<Vec<String>>,
    /// The answer for a command line (argv joined by spaces); `Ok(0)` for any other.
    pub script: RefCell<BTreeMap<String, Result<i32, String>>>,
}

impl FakeRunner {
    pub fn answer(&self, argv: &str, answer: Result<i32, String>) {
        self.script.borrow_mut().insert(argv.to_owned(), answer);
    }

    pub fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut self.calls.borrow_mut())
    }
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[&str]) -> Result<i32, String> {
        let line = argv.join(" ");
        self.calls.borrow_mut().push(line.clone());
        self.script.borrow().get(&line).cloned().unwrap_or(Ok(0))
    }
}

/// A probe that records what it is asked and answers from a script.
#[derive(Default)]
pub struct FakeProbe {
    pub calls: RefCell<Vec<(u16, String)>>,
    pub script: RefCell<BTreeMap<String, Result<(u16, String), String>>>,
}

impl FakeProbe {
    /// A server that is ready and runs `version`.
    pub fn ready(version: &str) -> FakeProbe {
        let probe = FakeProbe::default();
        probe.answer("/readyz", Ok((200, "ready\n".to_owned())));
        probe.answer("/version", Ok((200, format!("{version}\n"))));
        probe
    }

    pub fn answer(&self, path: &str, answer: Result<(u16, String), String>) {
        self.script.borrow_mut().insert(path.to_owned(), answer);
    }

    pub fn take_calls(&self) -> Vec<(u16, String)> {
        std::mem::take(&mut self.calls.borrow_mut())
    }
}

impl Probe for FakeProbe {
    fn get(&self, port: u16, path: &str) -> Result<(u16, String), String> {
        self.calls.borrow_mut().push((port, path.to_owned()));
        self.script
            .borrow()
            .get(path)
            .cloned()
            .unwrap_or_else(|| Err("no answer scripted".to_owned()))
    }
}

/// How the hook is reached: by `update-hook <step>`, by the bare step, or as `hook::run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    UpdateHook,
    Bare,
    Direct,
}

/// What one run of the hook did.
#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The values of the params file; `None` leaves the key out.
#[derive(Clone)]
pub struct ParamsSpec {
    pub units: Option<Scalar>,
    pub port: Option<Scalar>,
    pub record_dir: Option<Scalar>,
    pub target: Option<Scalar>,
    /// Keys the daemon writes that the hook does not use, and an unknown one.
    pub extras: bool,
}

pub struct World {
    pub dir: Scratch,
    pub units: Units,
    pub form: Form,
}

impl World {
    pub fn new(name: &str, units: Units) -> World {
        let dir = scratch(name);
        fs::create_dir(dir.path().join("record")).expect("the record directory");
        let bin = dir.path().join("artifact").join("bin");
        fs::create_dir_all(&bin).expect("the staged release");
        fs::write(bin.join("hello"), "#!/bin/sh\n").expect("the staged program");
        let world = World {
            dir,
            units,
            form: Form::UpdateHook,
        };
        world.write_params(&world.spec());
        world
    }

    pub fn systemd(name: &str) -> World {
        World::new(name, Units::Systemd)
    }

    pub fn record(&self) -> PathBuf {
        self.dir.path().join("record")
    }

    pub fn artifact(&self) -> PathBuf {
        self.dir.path().join("artifact")
    }

    pub fn params_file(&self) -> PathBuf {
        self.dir.path().join("params.toml")
    }

    pub fn spec(&self) -> ParamsSpec {
        ParamsSpec {
            units: Some(Scalar::Text(
                match self.units {
                    Units::Systemd => "systemd",
                    Units::Launchd => "launchd",
                }
                .to_owned(),
            )),
            port: Some(Scalar::Integer(i64::from(PORT))),
            record_dir: Some(Scalar::Text(self.record().display().to_string())),
            target: Some(Scalar::Text(TARGET.to_owned())),
            extras: true,
        }
    }

    /// Writes the params file as the daemon does: with `Params::to_toml`.
    pub fn write_params(&self, spec: &ParamsSpec) {
        let mut params = Params::default();
        for (key, value) in [
            ("units", &spec.units),
            ("port", &spec.port),
            ("record_dir", &spec.record_dir),
            ("updater.target_version", &spec.target),
        ] {
            if let Some(value) = value {
                params.insert(key, value.clone());
            }
        }
        if spec.extras {
            params.insert(
                "updater.staged_release",
                Scalar::Text(self.artifact().display().to_string()),
            );
            params.insert("updater.previous_version", Scalar::Text("1.0.0".to_owned()));
            params.insert(
                "updater.previous_release",
                Scalar::Text("/var/lib/hostkeeper/packages/hello/previous".to_owned()),
            );
            params.insert(
                "updater.artifact_digest",
                Scalar::Text(format!("sha256:{}", "ab".repeat(32))),
            );
            params.insert("updater.conformance_probe", Scalar::Integer(1));
            params.insert("unknown_float", Scalar::Float(1.5));
            params.insert("unknown_flag", Scalar::Bool(true));
            params.insert("unknown_text", Scalar::Text("a \"b\"\n\\c".to_owned()));
        }
        fs::write(self.params_file(), params.to_toml()).expect("the params file");
    }

    /// The arguments of a step as the updater passes them, before the form's prefix.
    pub fn step_args(&self, step: Step) -> Vec<OsString> {
        let mut args = vec![OsString::from(step.name())];
        if step == Step::Prepare {
            args.push(self.artifact().into_os_string());
        }
        args.push(OsString::from("--params"));
        args.push(self.params_file().into_os_string());
        args
    }

    /// Runs `args` (from the step on) in the world's form.
    pub fn run_args(&self, args: &[OsString], runner: &FakeRunner, probe: &FakeProbe) -> Ran {
        let host = Host {
            runner,
            probe,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = match self.form {
            Form::UpdateHook => {
                let mut all = vec![OsString::from("update-hook")];
                all.extend(args.iter().cloned());
                hello::run(&all, &host, &mut out, &mut err)
            }
            Form::Bare => hello::run(args, &host, &mut out, &mut err),
            Form::Direct => hello::hook::run(args, &host, &mut out, &mut err),
        };
        Ran {
            code,
            stdout: String::from_utf8(out).expect("stdout is text"),
            stderr: String::from_utf8(err).expect("stderr is text"),
        }
    }

    pub fn step(&self, step: Step, runner: &FakeRunner, probe: &FakeProbe) -> Ran {
        self.run_args(&self.step_args(step), runner, probe)
    }

    /// The record: every file under the record directory, by relative path, with its content;
    /// a directory is listed with `None`.
    pub fn snapshot(&self) -> BTreeMap<String, Option<String>> {
        let mut found = BTreeMap::new();
        walk(&self.record(), &self.record(), &mut found);
        found
    }
}

fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<String, Option<String>>) {
    for entry in fs::read_dir(dir).expect("a directory") {
        let path = entry.expect("an entry").path();
        let name = path.strip_prefix(root).expect("below the root").display().to_string();
        if path.is_dir() {
            found.insert(name, None);
            walk(root, &path, found);
        } else {
            found.insert(name, Some(fs::read_to_string(&path).expect("a file")));
        }
    }
}

/// What the contract says about one run.
#[derive(Debug)]
pub struct Want<'a> {
    pub outcome: Outcome,
    pub class: Class,
    pub reason: Option<&'a str>,
    /// `None`: the line does not state it (the contract reads that as reversible).
    pub reversible: Option<bool>,
    pub promotion_pending: bool,
    pub retry_after_secs: Option<u64>,
    pub detail: Option<&'a str>,
}

impl Want<'_> {
    pub fn new(outcome: Outcome) -> Want<'static> {
        Want {
            outcome,
            class: Class::Release,
            reason: None,
            reversible: None,
            promotion_pending: false,
            retry_after_secs: None,
            detail: None,
        }
    }
}

/// Reads a run as the daemon does: the exit code through `Outcome`, the last line with both
/// readers of the contract crate, which must agree. Checks it against `want` and returns what
/// the updater runs next.
pub fn contract(step: Step, ran: &Ran, want: &Want<'_>, ctx: &Context) -> Next {
    assert_eq!(
        Outcome::from_code(ran.code),
        Some(want.outcome),
        "exit code {} of {ran:?}",
        ran.code
    );
    assert_eq!(
        ran.stdout.lines().count(),
        1,
        "the line is the only output: {:?}",
        ran.stdout
    );
    assert!(ran.stdout.ends_with('\n'), "{:?}", ran.stdout);
    let strict = Report::from_stdout(&ran.stdout)
        .unwrap_or_else(|why| panic!("the strict reader refuses {:?}: {why}", ran.stdout));
    let lenient = Report::read(&ran.stdout);
    assert_eq!(strict, lenient, "the two readers disagree on {:?}", ran.stdout);
    assert_eq!(strict.class, want.class, "{ran:?}");
    assert_eq!(strict.reason.as_deref(), want.reason, "{ran:?}");
    assert_eq!(strict.retry_after_secs, want.retry_after_secs, "{ran:?}");
    assert_eq!(strict.promotion_pending, want.promotion_pending, "{ran:?}");
    assert_eq!(strict.reversible_stated, want.reversible.is_some(), "{ran:?}");
    assert_eq!(strict.reversible, want.reversible.unwrap_or(true), "{ran:?}");
    match want.detail {
        Some(detail) => assert!(
            strict.detail.as_deref().is_some_and(|got| got.contains(detail)),
            "detail {:?} lacks {detail:?}",
            strict.detail
        ),
        None => assert_eq!(strict.detail, None, "{ran:?}"),
    }
    hostkeeper_hook::next(step, Outcome::from_exit(Some(ran.code)), &strict, ctx)
}

/// `Context::default()` with the knobs the updater would set.
pub fn ctx() -> Context {
    Context::default()
}
