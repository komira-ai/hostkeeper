//! The program the tests run in place of `hkctl`. A program of the tests, not shipped.
//!
//! Its directory is the directory of the file its `--ca-file` names. Each run appends one line
//! to `<dir>/calls.jsonl`: `{"argv": [...], "request": <the JSON its --request file held, or
//! null>}`. It then answers the file `<dir>/answers/<n>.json`, `n` counting the runs from 0, or
//! `<dir>/answer.json` when there is none: `{"code": <exit code>, "stdout": "...", "stderr":
//! "..."}`; with `"abort": true` it ends itself by a signal instead. With neither file, it prints
//! `no answer` on stderr and exits 1, as hkctl does when the coordinator does not answer.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

fn after(argv: &[String], flag: &str) -> Option<String> {
    argv.iter()
        .position(|a| a == flag)
        .and_then(|i| argv.get(i + 1))
        .cloned()
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(ca) = after(&argv, "--ca-file") else {
        eprintln!("no --ca-file");
        return ExitCode::from(2);
    };
    let dir = Path::new(&ca)
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let request = after(&argv, "--request")
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or(Value::Null);
    let log = dir.join("calls.jsonl");
    let n = fs::read_to_string(&log).map_or(0, |t| t.lines().count());
    let line = json!({"argv": argv, "request": request});
    let appended = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .and_then(|mut f| writeln!(f, "{line}"));
    if appended.is_err() {
        eprintln!("cannot record the call");
        return ExitCode::from(1);
    }
    let answer = fs::read_to_string(dir.join("answers").join(format!("{n}.json")))
        .or_else(|_| fs::read_to_string(dir.join("answer.json")));
    let Ok(answer) = answer else {
        eprintln!("no answer");
        return ExitCode::from(1);
    };
    let answer: Value = serde_json::from_str(&answer).unwrap_or(Value::Null);
    if answer["abort"].as_bool() == Some(true) {
        std::process::abort();
    }
    print!("{}", answer["stdout"].as_str().unwrap_or_default());
    eprint!("{}", answer["stderr"].as_str().unwrap_or_default());
    let code = answer["code"].as_u64().unwrap_or(1);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
