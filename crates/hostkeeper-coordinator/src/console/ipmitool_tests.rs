//! The real BMC tool's process, run over this test binary as its child: the binary stands in for
//! `ipmitool shell`, and the ignored helper tests below are what it is asked to run.

use std::io::{BufRead, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use hostkeeper_console::credential::BmcCredential;
use hostkeeper_console::ipmi::{
    Invocation, PASSWORD_VARIABLE, PROMPT, Settings, Shell, ShellError, Tool, ToolText,
};
use hostkeeper_console::targets::Address;

use super::{Ipmitool, Process, activated, deadline};

const LONG: Duration = Duration::from_secs(60);

/// The command that runs the helper test `name` of this binary as the child.
fn helper(name: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("the test binary"));
    let path = format!(
        "{}::{name}",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    command
        .args([
            path.as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

/// Catches nothing itself: a shell that prints its prompt in two pieces, answers each command
/// with a line naming it, ends at `exit` or end of input, and ends with a refusal on its standard
/// error at `die`.
#[test]
#[ignore = "the child process of the shell tests of this module; run only by them"]
fn child_answers_as_a_shell() {
    let mut out = std::io::stdout();
    let prompt = |out: &mut std::io::Stdout| {
        let (first, second) = PROMPT.split_at(4);
        let _ = out.write_all(first.as_bytes());
        let _ = out.flush();
        let _ = out.write_all(second.as_bytes());
        let _ = out.flush();
    };
    prompt(&mut out);
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap_or_default();
        match line.as_str() {
            "exit" => {
                let _ = writeln!(out, "bye");
                let _ = out.flush();
                std::process::exit(0);
            }
            "die" => {
                let _ = std::io::stderr().write_all(b"insufficient resources for session\n");
                std::process::exit(1);
            }
            command => {
                let _ = writeln!(out, "answer to {command}");
            }
        }
        prompt(&mut out);
    }
    std::process::exit(0);
}

/// Catches nothing itself: a tool that cannot open its session.
#[test]
#[ignore = "the child process of the shell tests of this module; run only by them"]
fn child_fails_at_start() {
    let _ = std::io::stderr().write_all(b"Error: Unable to establish IPMI v2 / RMCP+ session\n");
    std::process::exit(1);
}

/// Catches nothing itself: a tool that never prompts and never ends.
#[test]
#[ignore = "the child process of the shell tests of this module; run only by them"]
fn child_never_prompts() {
    loop {
        std::thread::park();
    }
}

/// Catches nothing itself: a shell that prompts once, then hangs, ignoring its input.
#[test]
#[ignore = "the child process of the shell tests of this module; run only by them"]
fn child_prompts_then_hangs() {
    let _ = std::io::stdout().write_all(PROMPT.as_bytes());
    let _ = std::io::stdout().flush();
    loop {
        std::thread::park();
    }
}

/// Catches nothing itself: an activation, which prints a banner and from then on echoes every
/// byte of its input as it comes, until it reads `q`, when it says on its standard error that the
/// BMC closed the session and ends.
#[test]
#[ignore = "the child process of the shell tests of this module; run only by them"]
fn child_activates_sol() {
    let mut out = std::io::stdout();
    let _ = out.write_all(BANNER);
    let _ = out.flush();
    let mut stdin = std::io::stdin().lock();
    let mut byte = [0u8; 1];
    while stdin.read_exact(&mut byte).is_ok() {
        if byte[0] == b'q' {
            let _ = std::io::stderr().write_all(b"SOL session closed by BMC\n");
            std::process::exit(1);
        }
        let _ = out.write_all(&byte);
        let _ = out.flush();
    }
    std::process::exit(0);
}

/// The banner of [`child_activates_sol`].
const BANNER: &[u8] = b"[SOL Session operational.  Use \xff? for help]\n";

/// Reads from `output` until `count` bytes came.
fn read_count(output: &mut dyn Read, count: usize) -> Vec<u8> {
    let mut all = vec![0u8; count];
    output.read_exact(&mut all).expect("read");
    all
}

/// Reads from `output` until it has printed `needle` (the test harness prints its own lines
/// first).
fn read_past(output: &mut dyn Read, needle: &[u8]) {
    let mut all = Vec::new();
    while !all.ends_with(needle) {
        all.extend(read_count(output, 1));
    }
}

/// Proves: an activation is the tool's raw streams from its start, with no prompt waited for:
/// what is written reaches it and what it prints is read; when the tool ends on its own its
/// output ends and the end answers its standard error; ending one whose tool still runs kills it
/// at once (its output ends), sending it nothing (no `exit`), and so does dropping the end
/// uncalled.
///
/// Catches: an activation whose output is lost or buffered, a tool left running after its stream
/// ended or its end was dropped, and an `exit` typed at the host.
#[test]
fn an_activated_tool_is_its_streams_and_its_end_kills_it() {
    let run = activated(helper("child_activates_sol")).expect("activated");
    let hostkeeper_console::ipmi::Activated {
        mut input,
        mut output,
        end,
    } = run;
    read_past(output.as_mut(), BANNER);
    input.write_all(b"hi").expect("written");
    input.flush().expect("flushed");
    assert_eq!(read_count(output.as_mut(), 2), b"hi");
    input.write_all(b"q").expect("written");
    input.flush().expect("flushed");
    assert_eq!(output.read(&mut [0u8; 8]).expect("the end"), 0);
    assert_eq!(end().as_str(), "SOL session closed by BMC\n");

    let run = activated(helper("child_activates_sol")).expect("activated");
    let hostkeeper_console::ipmi::Activated {
        input,
        mut output,
        end,
    } = run;
    read_past(output.as_mut(), BANNER);
    assert_eq!(end().as_str(), "");
    assert_eq!(output.read(&mut [0u8; 8]).expect("the end"), 0);
    drop(input);

    let run = activated(helper("child_activates_sol")).expect("activated");
    let hostkeeper_console::ipmi::Activated {
        input,
        mut output,
        end,
    } = run;
    read_past(output.as_mut(), BANNER);
    drop(end);
    assert_eq!(output.read(&mut [0u8; 8]).expect("the end"), 0);
    drop(input);
}

/// Proves: an activation of a tool that cannot be started, or that is not on pipes, is a start
/// failure, and the second's child is not left running.
///
/// Catches: an activation reported on a tool that never ran, or whose streams are not the
/// coordinator's.
#[test]
fn a_tool_that_cannot_start_or_has_no_pipes_is_not_activated() {
    let missing = Ipmitool::new(Path::new("/nonexistent/ipmitool"), &Settings::default());
    let credential =
        BmcCredential::parse("user = \"ADMIN\"\npassword = \"pw\"\n").expect("a credential");
    let address = Address::Ip("192.0.2.12".parse().expect("an address"));
    let invocation = Invocation::sol(&address, &credential, &Settings::default());
    assert_eq!(missing.activate(&invocation).err(), Some(ShellError::Start));
    let mut unpiped = helper("child_activates_sol");
    unpiped.stdin(std::process::Stdio::null());
    assert_eq!(activated(unpiped).err(), Some(ShellError::Start));
}

/// Proves: the first prompt opens the session and what came before it is dropped; each command
/// is answered with exactly what the tool printed up to its next prompt, prompts split across
/// writes included; and a tool that ends at a command answers its standard error, whole.
///
/// Catches: the test harness's banner or the prompt read as an answer, an answer cut at a split
/// prompt, and a tool's refusal lost.
#[test]
fn each_answer_is_what_the_tool_printed_up_to_its_prompt() {
    let mut shell = Process::start(helper("child_answers_as_a_shell"), LONG).expect("a shell");
    assert_eq!(
        shell.ask("chassis power status"),
        Ok("answer to chassis power status\n".to_owned())
    );
    assert_eq!(shell.ask("raw 0x0c"), Ok("answer to raw 0x0c\n".to_owned()));
    assert_eq!(
        shell.ask("die"),
        Err(ShellError::Ended(ToolText::new(
            "insufficient resources for session\n".to_owned()
        )))
    );
    drop(shell);
    let shell = Process::start(helper("child_answers_as_a_shell"), LONG).expect("a shell");
    drop(shell);
}

/// Proves: a tool that ends before its first prompt is refused with its standard error, and one
/// that cannot be started is a start failure.
///
/// Catches: a refused session read as open, and a missing tool hanging the call.
#[test]
fn a_tool_that_ends_or_is_missing_opens_no_session() {
    assert_eq!(
        Process::start(helper("child_fails_at_start"), LONG).err(),
        Some(ShellError::Ended(ToolText::new(
            "Error: Unable to establish IPMI v2 / RMCP+ session\n".to_owned()
        )))
    );
    let missing = Ipmitool::new(Path::new("/nonexistent/ipmitool"), &Settings::default());
    let credential =
        BmcCredential::parse("user = \"ADMIN\"\npassword = \"pw\"\n").expect("a credential");
    let address = Address::Ip("192.0.2.12".parse().expect("an address"));
    let invocation = Invocation::new(&address, &credential, &Settings::default(), false);
    assert_eq!(missing.open(&invocation).err(), Some(ShellError::Start));
}

/// Proves: a tool that gives no prompt by the deadline is killed and the wait answers
/// `Deadline`, at its start or at a command; a session that ignores its end is killed at the
/// deadline when it is dropped.
///
/// Catches: a hung tool that holds a power call, and the BMC's lock, for ever.
#[test]
fn a_tool_with_no_prompt_by_the_deadline_is_killed() {
    let short = Duration::from_millis(200);
    assert_eq!(
        Process::start(helper("child_never_prompts"), short).err(),
        Some(ShellError::Deadline)
    );
    // Each hanging shell is started with the long deadline, so its first prompt is waited for;
    // only then is the short one set, which ends the waits for a prompt that never comes.
    let mut shell = Process::start(helper("child_prompts_then_hangs"), LONG).expect("a shell");
    shell.set_deadline(short);
    assert_eq!(shell.ask("chassis power status"), Err(ShellError::Deadline));
    let mut shell = Process::start(helper("child_prompts_then_hangs"), LONG).expect("a shell");
    shell.set_deadline(short);
    drop(shell);
}

/// Proves: the tool's command carries the invocation's arguments, and its environment holds the
/// password under `IPMI_PASSWORD` and nothing else the coordinator set; no argument holds the
/// password.
///
/// Catches: the password passed in the arguments, where the process table shows it to every user
/// of the host.
#[test]
fn the_credential_is_in_the_childs_environment_and_not_in_argv() {
    let tool = Ipmitool::new(Path::new("/usr/bin/ipmitool"), &Settings::default());
    let credential = BmcCredential::parse("user = \"ADMIN\"\npassword = \"s3cret pass\"\n")
        .expect("a credential");
    let address = Address::Ip("192.0.2.12".parse().expect("an address"));
    let invocation = Invocation::new(&address, &credential, &Settings::default(), false);
    let command = tool.command(&invocation);
    assert_eq!(command.get_program(), "/usr/bin/ipmitool");
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args, invocation.args());
    assert!(args.iter().all(|arg| !arg.contains("s3cret")), "{args:?}");
    let envs: Vec<(String, Option<String>)> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(
        envs,
        [(PASSWORD_VARIABLE.to_owned(), Some("s3cret pass".to_owned()))]
    );
}

/// Proves: a run that activates Serial over LAN starts with `-e` and the one raw byte 0xff, then
/// the invocation's arguments, which end in `sol activate` and not `shell`.
///
/// Catches: an activation with the default escape `~`, which any client's `~.` after a newline
/// would use to end the session, and an escape byte written as text (`0xff`, `ÿ`).
#[test]
fn the_escape_byte_is_one_raw_byte_before_the_arguments() {
    let tool = Ipmitool::new(Path::new("/usr/bin/ipmitool"), &Settings::default());
    let credential =
        BmcCredential::parse("user = \"ADMIN\"\npassword = \"pw\"\n").expect("a credential");
    let address = Address::Ip("192.0.2.12".parse().expect("an address"));
    let invocation = Invocation::sol(&address, &credential, &Settings::default());
    let command = tool.command(&invocation);
    let args: Vec<Vec<u8>> = command
        .get_args()
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    assert_eq!(args[..2], [b"-e".to_vec(), vec![0xff]]);
    let rest: Vec<Vec<u8>> = invocation
        .args()
        .iter()
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    assert_eq!(args[2..], rest);
    assert_eq!(
        rest[rest.len() - 2..],
        [b"sol".to_vec(), b"activate".to_vec()]
    );
}

/// Proves: the deadline of a wait is eight of the tool's worst exchanges and ten seconds, 34
/// seconds by default.
///
/// Catches: a deadline shorter than the tool's own retries, which would kill a slow but working
/// BMC session.
#[test]
fn the_deadline_covers_the_tools_own_retries() {
    assert_eq!(deadline(&Settings::default()), Duration::from_secs(34));
    let slow = Settings {
        timeout_secs: 5,
        retries: 0,
        ..Settings::default()
    };
    assert_eq!(deadline(&slow), Duration::from_secs(50));
}
