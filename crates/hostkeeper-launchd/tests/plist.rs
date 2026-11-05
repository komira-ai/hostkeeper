//! The plists of the daemon job and of a step job: their exact bytes, their key sets read back
//! through a small reader, the escaping of text, and the refusal of text that cannot be written.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use hostkeeper_hook::Step;
use hostkeeper_launchd::label::Label;
use hostkeeper_launchd::plist::{
    DAEMON_LOG_DIR, DAEMON_PROGRAM, DaemonJob, PlistError, StepJob, daemon_plist, step_plist,
};

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n";

/// A value of the subset of property lists these files use.
#[derive(Debug, PartialEq)]
enum Val {
    Str(String),
    Bool(bool),
    Array(Vec<Val>),
    Dict(Vec<(String, Val)>),
}

#[derive(Debug)]
enum Tok {
    Open(String),
    Close(String),
    Empty(String),
    Text(String),
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn tokens(body: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        let text = &rest[..start];
        let after_text_tag =
            matches!(toks.last(), Some(Tok::Open(name)) if name == "string" || name == "key");
        if after_text_tag && !text.is_empty() {
            toks.push(Tok::Text(unescape(text)));
        }
        let end = rest.find('>').expect("a tag ends with >");
        let tag = &rest[start + 1..end];
        if let Some(name) = tag.strip_prefix('/') {
            toks.push(Tok::Close(name.to_owned()));
        } else if let Some(name) = tag.strip_suffix('/') {
            toks.push(Tok::Empty(name.to_owned()));
        } else {
            toks.push(Tok::Open(tag.to_owned()));
        }
        rest = &rest[end + 1..];
    }
    toks
}

struct Reader {
    toks: std::vec::IntoIter<Tok>,
}

impl Reader {
    fn advance(&mut self) -> Tok {
        self.toks.next().expect("the plist ended early")
    }

    fn text_until_close(&mut self) -> String {
        match self.advance() {
            Tok::Text(text) => {
                assert!(matches!(self.advance(), Tok::Close(_)));
                text
            }
            Tok::Close(_) => String::new(),
            other => panic!("expected text, found {other:?}"),
        }
    }

    fn value(&mut self) -> Val {
        match self.advance() {
            Tok::Open(name) if name == "string" => Val::Str(self.text_until_close()),
            Tok::Empty(name) if name == "true" => Val::Bool(true),
            Tok::Empty(name) if name == "false" => Val::Bool(false),
            Tok::Open(name) if name == "array" => {
                let mut items = Vec::new();
                loop {
                    match self.toks.as_slice().first() {
                        Some(Tok::Close(_)) => {
                            self.advance();
                            return Val::Array(items);
                        }
                        _ => items.push(self.value()),
                    }
                }
            }
            Tok::Open(name) if name == "dict" => {
                let mut entries = Vec::new();
                loop {
                    match self.advance() {
                        Tok::Close(_) => return Val::Dict(entries),
                        Tok::Open(key) if key == "key" => {
                            let key = self.text_until_close();
                            entries.push((key, self.value()));
                        }
                        other => panic!("expected a key, found {other:?}"),
                    }
                }
            }
            other => panic!("unexpected token {other:?}"),
        }
    }
}

/// Reads the top-level dictionary of a rendered plist, after checking its header and trailer.
fn parse(xml: &str) -> Vec<(String, Val)> {
    let body = xml.strip_prefix(HEADER).expect("the Apple header");
    assert!(xml.ends_with("</dict>\n</plist>\n"));
    let mut reader = Reader {
        toks: tokens(body).into_iter(),
    };
    match reader.value() {
        Val::Dict(entries) => entries,
        other => panic!("expected a dict, found {other:?}"),
    }
}

fn keys(dict: &[(String, Val)]) -> Vec<&str> {
    dict.iter().map(|(key, _)| key.as_str()).collect()
}

fn strs(items: &[&str]) -> Val {
    Val::Array(
        items
            .iter()
            .map(|item| Val::Str((*item).to_owned()))
            .collect(),
    )
}

fn s(text: &str) -> Val {
    Val::Str(text.to_owned())
}

fn step_label() -> Label {
    Label::step("web", Step::Cutover, 7).expect("a valid step label")
}

fn standard_step_job() -> StepJob {
    let label = step_label();
    let dir = PathBuf::from(format!("/var/run/hostkeeper/steps/{label}"));
    StepJob {
        stdout: dir.join("stdout"),
        stderr: dir.join("stderr"),
        label,
        program: PathBuf::from("/var/lib/hostkeeper/packages/web/current/bin/web"),
        args: [
            "update-hook",
            "cutover",
            "--params",
            "/var/lib/updater/state/web/params.toml",
        ]
        .iter()
        .map(OsString::from)
        .collect(),
    }
}

fn daemon_with_program(program: PathBuf) -> DaemonJob {
    DaemonJob {
        program,
        ..DaemonJob::standard()
    }
}

fn control_error(value: &str) -> PlistError {
    PlistError::ControlCharacter {
        value: value.to_owned(),
    }
}

/// Catches a change to the daemon plist's bytes: key order, tabs, header or trailer.
#[test]
fn the_daemon_plist_matches_the_golden_bytes() {
    let xml = daemon_plist(&DaemonJob::standard()).expect("the standard job renders");
    assert_eq!(xml, include_str!("golden/daemon.plist"));
}

/// Catches a change to a step plist's bytes: key order, tabs, header or trailer.
#[test]
fn the_step_plist_matches_the_golden_bytes() {
    let xml = step_plist(&standard_step_job()).expect("the step job renders");
    assert_eq!(xml, include_str!("golden/step.plist"));
}

/// Catches a daemon job that is not kept alive, runs as another user, is sandboxed, or carries
/// an environment or sockets.
#[test]
fn the_daemon_plist_runs_as_root_with_keepalive_and_nothing_else() {
    let job = DaemonJob::standard();
    assert_eq!(job.program, PathBuf::from(DAEMON_PROGRAM));
    assert_eq!(
        job.stdout,
        PathBuf::from(DAEMON_LOG_DIR).join("daemon.out.log")
    );
    let dict = parse(&daemon_plist(&job).expect("renders"));
    assert_eq!(
        keys(&dict),
        [
            "Label",
            "ProgramArguments",
            "RunAtLoad",
            "KeepAlive",
            "StandardOutPath",
            "StandardErrorPath"
        ]
    );
    let expected = vec![
        ("Label".to_owned(), s(Label::daemon().as_str())),
        (
            "ProgramArguments".to_owned(),
            strs(&[DAEMON_PROGRAM, "daemon"]),
        ),
        ("RunAtLoad".to_owned(), Val::Bool(true)),
        ("KeepAlive".to_owned(), Val::Bool(true)),
        (
            "StandardOutPath".to_owned(),
            s("/var/log/hostkeeper/daemon.out.log"),
        ),
        (
            "StandardErrorPath".to_owned(),
            s("/var/log/hostkeeper/daemon.err.log"),
        ),
    ];
    assert_eq!(dict, expected);
}

/// Catches a step job that restarts, runs as another user, is sandboxed, has an environment, or
/// abandons its process group.
#[test]
fn a_step_job_plist_has_no_keepalive_user_or_sandbox() {
    let job = standard_step_job();
    let dict = parse(&step_plist(&job).expect("renders"));
    assert_eq!(
        keys(&dict),
        [
            "Label",
            "ProgramArguments",
            "RunAtLoad",
            "AbandonProcessGroup",
            "StandardOutPath",
            "StandardErrorPath"
        ]
    );
    for absent in ["KeepAlive", "UserName", "GroupName", "SandboxProfile"] {
        assert!(!keys(&dict).contains(&absent), "{absent} must be absent");
    }
    for absent in ["EnvironmentVariables", "Sockets"] {
        assert!(!keys(&dict).contains(&absent), "{absent} must be absent");
    }
    let label = step_label();
    let expected = vec![
        ("Label".to_owned(), s(label.as_str())),
        (
            "ProgramArguments".to_owned(),
            strs(&[
                "/var/lib/hostkeeper/packages/web/current/bin/web",
                "update-hook",
                "cutover",
                "--params",
                "/var/lib/updater/state/web/params.toml",
            ]),
        ),
        ("RunAtLoad".to_owned(), Val::Bool(true)),
        ("AbandonProcessGroup".to_owned(), Val::Bool(false)),
        (
            "StandardOutPath".to_owned(),
            s(&format!("/var/run/hostkeeper/steps/{label}/stdout")),
        ),
        (
            "StandardErrorPath".to_owned(),
            s(&format!("/var/run/hostkeeper/steps/{label}/stderr")),
        ),
    ];
    assert_eq!(dict, expected);
}

/// Catches text written unescaped (markup injection through a path or argument), and an escape
/// that does not read back as the same text.
#[test]
fn plist_text_is_escaped() {
    let nasty = "a&b<c>d\"e'f &amp; g";
    let mut job = standard_step_job();
    job.program = PathBuf::from(format!("/bin/{nasty}"));
    job.args = vec![OsString::from(nasty), OsString::new()];
    let xml = step_plist(&job).expect("escapable text renders");
    assert!(xml.contains("<string>a&amp;b&lt;c&gt;d&quot;e&apos;f &amp;amp; g</string>"));
    assert!(!xml.contains(nasty));
    let dict = parse(&xml);
    let program = format!("/bin/{nasty}");
    assert_eq!(dict[1].1, strs(&[program.as_str(), nasty, ""]));
}

/// Catches a control character reaching the file through an argument, the program path or an
/// output path, and a refusal whose message does not name the text.
#[test]
fn plist_refuses_control_characters() {
    for bad in ["a\nb", "a\tb", "a\u{7}b", "a\u{7f}b"] {
        let mut job = standard_step_job();
        job.args.push(OsString::from(bad));
        assert_eq!(step_plist(&job), Err(control_error(bad)));

        let mut job = standard_step_job();
        job.program = PathBuf::from(bad);
        assert_eq!(step_plist(&job), Err(control_error(bad)));

        let mut job = standard_step_job();
        job.stdout = PathBuf::from(bad);
        assert_eq!(step_plist(&job), Err(control_error(bad)));

        let mut job = standard_step_job();
        job.stderr = PathBuf::from(bad);
        assert_eq!(step_plist(&job), Err(control_error(bad)));

        assert_eq!(
            daemon_plist(&daemon_with_program(PathBuf::from(bad))),
            Err(control_error(bad))
        );
        let job = DaemonJob {
            stdout: PathBuf::from(bad),
            ..DaemonJob::standard()
        };
        assert_eq!(daemon_plist(&job), Err(control_error(bad)));
    }
    let error = step_plist(&StepJob {
        args: vec![OsString::from("a\nb")],
        ..standard_step_job()
    })
    .expect_err("a newline is refused");
    assert_eq!(
        error.to_string(),
        "plist text \"a\\nb\" holds a control character"
    );
    let error = daemon_plist(&daemon_with_program(PathBuf::from("/bin/\u{7}")))
        .expect_err("a bell is refused");
    assert_eq!(
        error.to_string(),
        "plist text \"/bin/\\u{7}\" holds a control character"
    );
}

/// Catches a non-UTF-8 argument or path being written lossily, which would start a different
/// program than the one asked for.
#[test]
fn plist_refuses_text_that_is_not_utf8() {
    let bad = || OsString::from_vec(vec![0xff]);
    let expected = Err(PlistError::NotUtf8 {
        value: "\u{fffd}".to_owned(),
    });

    let mut job = standard_step_job();
    job.args.push(bad());
    assert_eq!(step_plist(&job), expected);

    let mut job = standard_step_job();
    job.program = PathBuf::from(bad());
    assert_eq!(step_plist(&job), expected);

    let mut job = standard_step_job();
    job.stdout = PathBuf::from(bad());
    assert_eq!(step_plist(&job), expected);

    let mut job = standard_step_job();
    job.stderr = PathBuf::from(bad());
    assert_eq!(step_plist(&job), expected);

    assert_eq!(
        daemon_plist(&daemon_with_program(PathBuf::from(bad()))),
        expected
    );
    let job = DaemonJob {
        stdout: PathBuf::from(bad()),
        ..DaemonJob::standard()
    };
    assert_eq!(daemon_plist(&job), expected);
    let job = DaemonJob {
        stderr: PathBuf::from(bad()),
        ..DaemonJob::standard()
    };
    assert_eq!(daemon_plist(&job), expected);

    let error = expected.expect_err("an error");
    assert_eq!(
        error.to_string(),
        "plist text \"\u{fffd}\" is not valid UTF-8"
    );
}
