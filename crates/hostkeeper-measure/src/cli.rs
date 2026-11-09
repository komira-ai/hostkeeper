//! The command line: `check`, `run` and `analyze` (the crate's documentation describes each).

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use thiserror::Error;

use crate::analysis::{self, AnalysisError};
use crate::clock::Clock;
use crate::coordinator::Hkctl;
use crate::params::{ParamError, Params};
use crate::plan::{Plan, PlanError};
use crate::record::{self, RecordError, RunInfo};
use crate::render;
use crate::runner::{self, Io, Names, RunError};
use crate::template::id_ok;

/// hostkeeper's measurements: carry a measurement plan out against a coordinator with hkctl, and
/// turn a run's records into a section of the results page and a result file.
#[derive(Debug, Parser)]
#[command(name = "hostkeeper-measure", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read and check plans, and print each plan's id and title.
    Check {
        /// A plan file; repeat for more.
        #[arg(long = "plan", required = true)]
        plans: Vec<PathBuf>,
    },
    /// Run a plan against a coordinator, or with --dry-run print what that would do.
    Run(RunArgs),
    /// Turn a run's records into a results page section and a result file.
    Analyze {
        #[arg(long)]
        plan: PathBuf,
        /// The run directory `run` wrote.
        #[arg(long)]
        run_dir: PathBuf,
        /// Where to write `<plan>-<run>.md` and `<plan>-<run>.tsv`: docs/report/results.
        #[arg(long)]
        out_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long)]
    pub plan: PathBuf,
    /// The run's id, 1 to 32 bytes of [a-z0-9-] starting with a letter; it names the result.
    #[arg(long)]
    pub run_id: String,
    /// A parameter: name=value, or name@repetition=value for a per-repetition one.
    #[arg(long = "param")]
    pub params: Vec<String>,
    /// Print the steps with their parameters, and run nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// A new or empty directory for the records.
    #[arg(long, required_unless_present = "dry_run")]
    pub run_dir: Option<PathBuf>,
    /// The hkctl program.
    #[arg(long, required_unless_present = "dry_run")]
    pub hkctl: Option<PathBuf>,
    /// The coordinator's private listener, as hkctl's --endpoint takes it.
    #[arg(long, required_unless_present = "dry_run")]
    pub endpoint: Option<String>,
    /// The CA the coordinator's certificate chains to, as hkctl's --ca-file takes it.
    #[arg(long, required_unless_present = "dry_run")]
    pub ca_file: Option<PathBuf>,
    /// The operator token's file, as hkctl's --token-file takes it.
    #[arg(long, required_unless_present = "dry_run")]
    pub token_file: Option<PathBuf>,
}

/// Why a command failed.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("{path}: {source}")]
    File {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {source}")]
    Plan {
        path: PathBuf,
        #[source]
        source: PlanError,
    },
    #[error(transparent)]
    Params(#[from] ParamError),
    #[error("the run id {0:?} is not 1 to 32 bytes of [a-z0-9-] starting with a letter")]
    RunId(String),
    #[error("{0}: the run directory already holds records")]
    Used(PathBuf),
    #[error("{path}: {source}")]
    RunInfo {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: {source}")]
    Records {
        path: PathBuf,
        #[source]
        source: RecordError,
    },
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Analysis(#[from] AnalysisError),
    #[error("writing the output: {0}")]
    Output(#[source] io::Error),
}

/// The program: parses the command line and runs it with the machine's clock and standard
/// streams.
pub fn main() -> ExitCode {
    let clock = crate::clock::SystemClock::new();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    run_with(
        std::env::args_os(),
        &clock,
        &mut input,
        &mut io::stdout(),
        &mut io::stderr(),
    )
}

/// The program with its arguments, clock and streams given: 0 done, 1 failed, 2 a usage error.
pub fn run_with(
    args: impl IntoIterator<Item = OsString>,
    clock: &dyn Clock,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // Help and the version go to stdout and are not a failure; a usage error is 2.
            if e.use_stderr() {
                let _ = write!(err, "{}", e.render());
                return ExitCode::from(2);
            }
            let _ = write!(out, "{}", e.render());
            return ExitCode::SUCCESS;
        }
    };
    match execute(cli.command, clock, input, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "hostkeeper-measure: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &Path) -> Result<String, CliError> {
    fs::read_to_string(path).map_err(|source| CliError::File {
        path: path.to_owned(),
        source,
    })
}

fn write(path: &Path, text: &str) -> Result<(), CliError> {
    fs::write(path, text).map_err(|source| CliError::File {
        path: path.to_owned(),
        source,
    })
}

/// Reads and checks the plan file at `path`.
pub fn load_plan(path: &Path) -> Result<Plan, CliError> {
    Plan::parse(&read(path)?).map_err(|source| CliError::Plan {
        path: path.to_owned(),
        source,
    })
}

/// Runs one command.
pub fn execute(
    command: Command,
    clock: &dyn Clock,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    match command {
        Command::Check { plans } => {
            for path in plans {
                let plan = load_plan(&path)?;
                writeln!(out, "{}: {}", plan.id, plan.title).map_err(CliError::Output)?;
            }
            Ok(())
        }
        Command::Run(args) => run(&args, clock, input, out),
        Command::Analyze {
            plan,
            run_dir,
            out_dir,
        } => {
            let plan = load_plan(&plan)?;
            let info_path = run_dir.join("run.json");
            let info: RunInfo =
                serde_json::from_str(&read(&info_path)?).map_err(|source| CliError::RunInfo {
                    path: info_path,
                    source,
                })?;
            let records_path = run_dir.join("records.jsonl");
            let records =
                record::parse(&read(&records_path)?).map_err(|source| CliError::Records {
                    path: records_path,
                    source,
                })?;
            let analysis = analysis::analyze(&plan, &info, &records)?;
            let stem = format!("{}-{}", plan.id, info.run);
            for (ext, text) in [
                ("md", render::markdown(&analysis)),
                ("tsv", render::tsv(&analysis)),
            ] {
                let path = out_dir.join(format!("{stem}.{ext}"));
                write(&path, &text)?;
                writeln!(out, "wrote {}", path.display()).map_err(CliError::Output)?;
            }
            Ok(())
        }
    }
}

fn run(
    args: &RunArgs,
    clock: &dyn Clock,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let plan = load_plan(&args.plan)?;
    if !id_ok(&args.run_id, 32) {
        return Err(CliError::RunId(args.run_id.clone()));
    }
    let params = Params::parse(&args.params)?;
    // Clap holds the live flags present unless --dry-run is given.
    let (Some(dir), Some(hkctl), Some(endpoint), Some(ca), Some(token), false) = (
        &args.run_dir,
        &args.hkctl,
        &args.endpoint,
        &args.ca_file,
        &args.token_file,
        args.dry_run,
    ) else {
        return Ok(crate::dry::dry_run(&plan, &params, out)?);
    };
    let records_path = dir.join("records.jsonl");
    if fs::metadata(&records_path).is_ok_and(|m| m.len() > 0) {
        return Err(CliError::Used(dir.clone()));
    }
    let requests = dir.join("requests");
    fs::create_dir_all(&requests).map_err(|source| CliError::File {
        path: requests.clone(),
        source,
    })?;
    let info = RunInfo {
        plan: plan.id.clone(),
        run: args.run_id.clone(),
        repetitions: plan.repetitions,
    };
    write(
        &dir.join("run.json"),
        &format!(
            "{:#}\n",
            serde_json::json!({"plan": info.plan, "run": info.run, "repetitions": info.repetitions})
        ),
    )?;
    let mut file: File = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&records_path)
        .map_err(|source| CliError::File {
            path: records_path.clone(),
            source,
        })?;
    let mut sink = |r: &record::Record| {
        file.write_all(r.line().as_bytes())?;
        file.flush()
    };
    let mut coordinator = Hkctl::new(hkctl, endpoint, ca, token, &requests);
    let mut names = Names::default();
    let result = runner::run(
        &plan,
        &params,
        Io {
            coordinator: &mut coordinator,
            clock,
            operator: input,
            console: out,
            records: &mut sink,
            names: &mut names,
        },
    );
    write(&dir.join("hosts.tsv"), &names.tsv())?;
    let summary = result?;
    writeln!(
        out,
        "{} of {} repetitions complete; the records are in {}",
        summary.complete,
        plan.repetitions,
        records_path.display()
    )
    .map_err(CliError::Output)?;
    for (rep, why) in &summary.ended {
        writeln!(out, "repetition {rep} ended early: {why}").map_err(CliError::Output)?;
    }
    Ok(())
}
