//! `docs/operations/console.md`'s move of an installed server to the serial baseline, against the
//! seed: the files the runbook writes by hand are exactly the files a `ttyS1` console adds to a
//! seed, byte for byte, and its commands run the `runcmd` lines that console adds, in the seed's
//! order (docs/design/console-broker.md, section 5.3: a server moved by hand ends as one
//! installed with the console).
//!
//! The runbook writes a file in a fenced `sh` block as a heredoc of exactly the shape
//! `sudo tee <path> >/dev/null <<'EOF'`, its lines, then `EOF`; the quoted delimiter keeps the
//! shell from expanding `$GRUB_CMDLINE_LINUX`, `$TERM` and `\\u`, so the file holds the lines as
//! written. A heredoc's lines lose only the indentation of their block's opening fence (a block
//! inside a list item), so a space added or dropped inside a line, or at its end, is a difference.
//! Any other line whose first two words are `sudo tee` is refused rather than read, so a file
//! written another way does not escape the check, and a file written twice is a problem. A
//! command is a line of a fenced `sh` block outside a heredoc, trimmed; the seed's `runcmd` line
//! `[a, b]` is the command `sudo a b`.

use hostkeeper_pxe_seed::inputs::LinuxPlatform;

use super::files_and_runcmd;
use super::support::Build;

/// The runbook, from this crate's directory.
const RUNBOOK: &str = "../../docs/operations/console.md";

/// The fleet's serial console, COM2.
const PORT: &str = "ttyS1";

/// The start of a heredoc line the check reads, before the path.
const TEE: &str = "sudo tee ";

/// The end of a heredoc line the check reads, after the path.
const TEE_END: &str = " >/dev/null <<'EOF'";

/// What the runbook's `sh` blocks write and run.
#[derive(Debug, Default, PartialEq, Eq)]
struct Steps {
    /// `(path, content)` of each heredoc, in order; the content ends with a newline.
    files: Vec<(String, String)>,
    /// Every other line, trimmed, in order.
    commands: Vec<String>,
}

/// The lines of every fenced `sh` block of `doc`, in order, each without the indentation of its
/// block's opening fence (a block inside a list item) and nothing more; a line indented less (a
/// blank line) loses all of its own. A block still open at the end of `doc` is an error.
fn sh_lines(doc: &str) -> Result<Vec<&str>, String> {
    let mut lines = Vec::new();
    let mut open: Option<usize> = None;
    for line in doc.lines() {
        let fence = line.trim();
        let indent = line.len() - line.trim_start().len();
        match (open, fence) {
            (None, "```sh") => open = Some(indent),
            (Some(_), "```") => open = None,
            (Some(at), _) => lines.push(line.get(at..).filter(|_| indent >= at).unwrap_or(fence)),
            (None, _) => {}
        }
    }
    if open.is_some() {
        return Err("a sh block is not closed".to_owned());
    }
    Ok(lines)
}

/// Whether `line`'s first two words are `sudo tee`, however they are spaced.
fn is_tee(line: &str) -> bool {
    line.split_whitespace().take(2).eq(["sudo", "tee"])
}

/// The files and commands of `doc`'s `sh` blocks, or why they cannot be read.
fn steps(doc: &str) -> Result<Steps, String> {
    let mut steps = Steps::default();
    let mut lines = sh_lines(doc)?.into_iter();
    while let Some(line) = lines.next() {
        if !is_tee(line) {
            steps.commands.push(line.trim().to_owned());
            continue;
        }
        let line = line.trim();
        let path = line
            .strip_prefix(TEE)
            .and_then(|rest| rest.strip_suffix(TEE_END))
            .filter(|path| path.starts_with('/') && !path.contains(char::is_whitespace))
            .ok_or_else(|| format!("{line:?} is not `{TEE}<path>{TEE_END}`"))?;
        let mut content = String::new();
        loop {
            match lines.next() {
                Some("EOF") => break,
                Some(text) => {
                    content.push_str(text);
                    content.push('\n');
                }
                None => return Err(format!("the heredoc of {path} has no EOF in its block")),
            }
        }
        steps.files.push((path.to_owned(), content));
    }
    Ok(steps)
}

/// What a `ttyS1` console adds to an x86_64 seed: its files and its `runcmd` lines, each line
/// as the command the runbook types (`sudo` and the words).
fn baseline() -> Steps {
    let plain = files_and_runcmd(&Build::new(LinuxPlatform::X86_64).seed().user_data);
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.console = Some(PORT.parse().expect("a serial port"));
    let console = files_and_runcmd(&build.seed().user_data);
    Steps {
        files: console
            .0
            .into_iter()
            .filter(|file| !plain.0.contains(file))
            .collect(),
        commands: console
            .1
            .into_iter()
            .filter(|line| !plain.1.contains(line))
            .map(|words| format!("sudo {}", words.join(" ")))
            .collect(),
    }
}

/// Where `doc`'s hand step differs from `want`: a file of `want` it does not write or writes
/// otherwise, a file it writes that `want` does not have or writes more than once, and a command
/// of `want` it does not run after the one before.
fn problems(doc: &str, want: &Steps) -> Vec<String> {
    let have = match steps(doc) {
        Ok(have) => have,
        Err(why) => return vec![why],
    };
    let mut problems = Vec::new();
    for (path, content) in &want.files {
        match have.files.iter().find(|(written, _)| written == path) {
            None => problems.push(format!("the runbook does not write {path}")),
            Some((_, written)) if written != content => problems.push(format!(
                "the runbook writes {path} as {written:?}; the seed writes {content:?}"
            )),
            Some(_) => {}
        }
    }
    for (index, (path, _)) in have.files.iter().enumerate() {
        if have.files[..index]
            .iter()
            .any(|(earlier, _)| earlier == path)
        {
            problems.push(format!("the runbook writes {path} more than once"));
        } else if !want.files.iter().any(|(wanted, _)| wanted == path) {
            problems.push(format!(
                "the runbook writes {path}, which the seed does not"
            ));
        }
    }
    let mut after = have.commands.iter();
    for command in &want.commands {
        if !after.any(|typed| typed == command) {
            problems.push(format!(
                "the runbook does not run `{command}` after the commands before it"
            ));
            after = have.commands.iter();
        }
    }
    problems
}

/// The seed's own baseline: the two files and three commands a `ttyS1` console adds.
///
/// Catches a baseline read from the wrong seed, which would make the runbook's check vacuous.
#[test]
fn the_baseline_is_the_grub_file_and_the_getty_drop_in_and_their_three_commands() {
    let want = baseline();
    assert_eq!(
        want.files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        [
            "/etc/default/grub.d/99_hostkeeper.cfg",
            "/etc/systemd/system/serial-getty@ttyS1.service.d/99_hostkeeper.conf",
        ]
    );
    assert_eq!(
        want.commands,
        [
            "sudo update-grub",
            "sudo systemctl daemon-reload",
            "sudo systemctl enable --now serial-getty@ttyS1.service",
        ]
    );
}

/// The runbook's move to the baseline writes the GRUB file and the getty's drop-in byte for byte
/// as the seed does, and runs the seed's three commands in its order.
///
/// Catches a runbook whose hand step leaves a server different from one installed with
/// `serial-console = "ttyS1"`: a GRUB line or the getty's speed edited in one and not the other,
/// a file of the seed's not written by hand, or `daemon-reload` after the getty is started.
#[test]
fn the_runbooks_hand_step_writes_and_runs_what_the_seed_does() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(RUNBOOK);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {RUNBOOK}: {e}"));
    assert_eq!(
        problems(&doc, &baseline()),
        Vec::<String>::new(),
        "{RUNBOOK}"
    );
}

/// A runbook of the baseline's own files and commands, as the runbook writes them.
fn good() -> String {
    let want = baseline();
    let mut doc = String::from("Text.\n\n1. Step:\n\n   ```sh\n   sudo rm /x\n");
    for (path, content) in &want.files {
        doc.push_str(&format!("   {TEE}{path}{TEE_END}\n"));
        for line in content.lines() {
            doc.push_str(&format!("   {line}\n"));
        }
        doc.push_str("   EOF\n");
    }
    for command in &want.commands {
        doc.push_str(&format!("   {command}\n   echo between\n"));
    }
    doc.push_str("   ```\n\n```text\nsudo tee /y\n```\n");
    doc
}

/// An indented `sh` block with the baseline's heredocs and commands, other commands between
/// them, and a `sudo tee` line outside any `sh` block, has no problem.
///
/// Catches a check that fails a good runbook, or reads a block of another kind.
#[test]
fn a_runbook_with_the_seeds_files_and_commands_passes() {
    assert_eq!(problems(&good(), &baseline()), Vec::<String>::new());
}

/// A file written with one byte changed (a digit, a space at a line's end, a line indented more
/// than its fence), a file not written, a file the seed does not write, a file written twice, a
/// command left out, and the commands out of the seed's order are each a problem.
///
/// Catches a check that compares paths alone or trims a file's lines, ignores a missing file or
/// command or a second write of one, or does not hold the order.
#[test]
fn a_hand_step_that_differs_from_the_seed_is_a_problem() {
    let want = baseline();
    let drop_in = &want.files[1].0;
    let grub = &want.files[0].0;
    let speed = good().replacen("' 115200 - $TERM", "' 9600 - $TERM", 1);
    assert_eq!(
        problems(&speed, &want),
        [format!(
            "the runbook writes {drop_in} as \"[Service]\\nExecStart=\\nExecStart=-/sbin/agetty -o '-p -- \\\\\\\\u' 9600 - $TERM\\n\"; \
             the seed writes \"[Service]\\nExecStart=\\nExecStart=-/sbin/agetty -o '-p -- \\\\\\\\u' 115200 - $TERM\\n\""
        )]
    );
    let renamed = good().replacen("99_hostkeeper.cfg", "90_console.cfg", 1);
    assert_eq!(
        problems(&renamed, &want),
        [
            format!("the runbook does not write {grub}"),
            "the runbook writes /etc/default/grub.d/90_console.cfg, which the seed does not"
                .to_owned(),
        ]
    );
    let missing = good().replacen("   sudo systemctl daemon-reload\n", "", 1);
    assert_eq!(
        problems(&missing, &want),
        ["the runbook does not run `sudo systemctl daemon-reload` after the commands before it"]
    );
    let spaced = good().replacen("--speed=115200\"", "--speed=115200\" ", 1);
    assert_eq!(
        problems(&spaced, &want).len(),
        1,
        "a space at a line's end is a difference"
    );
    let indented = good().replacen("   ExecStart=\n", "     ExecStart=\n", 1);
    assert_eq!(
        problems(&indented, &want).len(),
        1,
        "a line indented more than its fence is a difference"
    );
    let again: String = want.files[0]
        .1
        .lines()
        .map(|line| format!("   {line}\n"))
        .collect();
    let twice = good().replacen(
        "   sudo update-grub\n",
        &format!("   {TEE}{grub}{TEE_END}\n{again}   EOF\n   sudo update-grub\n"),
        1,
    );
    assert_eq!(
        problems(&twice, &want),
        [format!("the runbook writes {grub} more than once")]
    );
    let swapped = good()
        .replacen("sudo update-grub", "<first>", 1)
        .replacen("sudo systemctl daemon-reload", "sudo update-grub", 1)
        .replacen("<first>", "sudo systemctl daemon-reload", 1);
    assert_eq!(
        problems(&swapped, &want),
        ["the runbook does not run `sudo systemctl daemon-reload` after the commands before it"]
    );
}

/// A `sudo tee` line of another shape (no quoted delimiter, so the shell would expand `$TERM`;
/// appending; no path; the two words spaced otherwise; a path with a space), a heredoc with no `EOF` in its block, and a `sh` block that is not
/// closed are each refused rather than read.
///
/// Catches a file written in a way the check would pass over, or one whose end it cannot see.
#[test]
fn a_heredoc_or_block_the_check_cannot_read_is_a_problem() {
    let want = baseline();
    let unquoted = good().replacen(TEE_END, " >/dev/null <<EOF", 1);
    let line = format!("{TEE}{} >/dev/null <<EOF", want.files[0].0);
    assert_eq!(
        problems(&unquoted, &want),
        [format!("{line:?} is not `{TEE}<path>{TEE_END}`")]
    );
    for shape in [
        "sudo tee -a /x >/dev/null <<'EOF'",
        "sudo tee  >/dev/null <<'EOF'",
        "sudo  tee /x >/dev/null <<'EOF'",
        "sudo tee /a b >/dev/null <<'EOF'",
    ] {
        assert_eq!(
            problems(&format!("```sh\n{shape}\nEOF\n```\n"), &want),
            [format!("{shape:?} is not `{TEE}<path>{TEE_END}`")]
        );
    }
    let no_end = format!("```sh\n{TEE}/x{TEE_END}\nline\n```\n");
    assert_eq!(
        problems(&no_end, &want),
        ["the heredoc of /x has no EOF in its block"]
    );
    assert_eq!(
        problems("```sh\nsudo update-grub\n", &want),
        ["a sh block is not closed"]
    );
}
