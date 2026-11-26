//! The settings file `$HOME/.hostkeeper/hk.toml` and `hk settings init`.
//!
//! The file gives the default of the global flags `--endpoint`, `--ca-file` and `--token-file`,
//! as the keys `endpoint`, `ca_file` and `token_file`, all strings. [`fill`] is what every verb
//! that uses these flags (a gateway call, the ssh verbs, `xcode-push`) runs before it checks
//! them: a flag given on the command line wins, and a flag neither gives stays missing, so the
//! verb's own usage error is the one said.
//!
//! - The file is read only when a flag a verb needs is missing, so a command line that gives all
//!   of them never fails because of the file, and `--help` and `--version` never read it.
//! - `token_file` is a default only when no other credential is given: `--token-stdin` or
//!   `--github-oidc-audience` on the command line leaves it unused.
//! - `~/.hostkeeper` and the file must be the user's own, not links, and not writable by group or
//!   others ([`crate::home`]); a path that is not is a usage error naming it.
//! - A key of another name is ignored with `hk: <path>: unknown key <name> ignored` on stderr,
//!   so that an older `hk` reads a file a later one wrote. A known key that is not a string is
//!   a usage error.
//!
//! `hk settings init` writes the file: mode 0600, and never over a file that is there. It makes
//! one `Xcode.List` call with the three values first, and writes only when it is answered, so
//! a file that is there has been shown to work; a call that fails writes nothing.

use crate::args::{Globals, flag_of};
use crate::call::{exchange, is_endpoint};
use crate::help::SETTINGS_HELP;
use crate::home::Home;
use crate::{Flow, Stop, World};

/// The path of the method `hk settings init` calls to show that the values work.
pub(crate) const LIST_PATH: &str = "/hostkeeper.operator.v1.Xcode/List";

/// What the file gives.
#[derive(Default)]
struct Settings {
    endpoint: Option<Vec<u8>>,
    ca_file: Option<Vec<u8>>,
    token_file: Option<Vec<u8>>,
}

/// The settings file of the user, `None` when there is none (no home, no `~/.hostkeeper`, no
/// file). Unknown keys are said on stderr.
fn load(w: &mut World<'_>) -> Flow<Option<Settings>> {
    let Some(home) = Home::of(w) else {
        return Ok(None);
    };
    let path = home.settings();
    if !home.exists()? || !home.check_file(&path)? {
        return Ok(None);
    }
    let shown = path.display();
    let bytes = std::fs::read(&path)
        .map_err(|error| Stop::usage(format!("cannot read {shown}: {error}")))?;
    let text = String::from_utf8(bytes).map_err(|_| Stop::usage(format!("{shown} is not text")))?;
    let table: toml::Table = toml::from_str(&text)
        .map_err(|error| Stop::usage(format!("{shown}: {}", error.message())))?;
    let mut settings = Settings::default();
    for (key, value) in table {
        let slot = match key.as_str() {
            "endpoint" => &mut settings.endpoint,
            "ca_file" => &mut settings.ca_file,
            "token_file" => &mut settings.token_file,
            _ => {
                let _ = writeln!(
                    w.stderr,
                    "{}: {shown}: unknown key {key} ignored",
                    crate::PROGRAM
                );
                continue;
            }
        };
        let Some(text) = value.as_str() else {
            return Err(Stop::usage(format!("{shown}: {key} must be a string")));
        };
        *slot = Some(text.as_bytes().to_vec());
    }
    Ok(Some(settings))
}

/// Gives each of `--endpoint`, `--ca-file` and `--token-file` that `globals` lack the value the
/// settings file has for it, when it has one.
pub(crate) fn fill(w: &mut World<'_>, globals: &mut Globals) -> Flow<()> {
    let token_default =
        globals.token_file.is_none() && !globals.token_stdin && globals.audience.is_none();
    if globals.endpoint.is_some() && globals.ca_file.is_some() && !token_default {
        return Ok(());
    }
    let Some(file) = load(w)? else {
        return Ok(());
    };
    globals.endpoint = globals.endpoint.take().or(file.endpoint);
    globals.ca_file = globals.ca_file.take().or(file.ca_file);
    if token_default {
        globals.token_file = file.token_file;
    }
    Ok(())
}

/// `hk settings`: `args` are the words after `settings`, among which the global flags are taken;
/// `globals` are those given before it.
pub(crate) fn settings(w: &mut World<'_>, mut globals: Globals, args: &[Vec<u8>]) -> Flow<()> {
    let mut words: Vec<&[u8]> = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if matches!(arg.as_slice(), b"--help" | b"-h") {
            let _ = writeln!(w.stdout, "{SETTINGS_HELP}");
            return Ok(());
        }
        if let Some(took) = globals.take(args.get(at..).unwrap_or_default())? {
            at += took;
            continue;
        }
        if arg.starts_with(b"-") {
            return Err(Stop::usage(
                [
                    format!("{} settings has no flag ", crate::PROGRAM).as_bytes(),
                    flag_of(arg),
                ]
                .concat(),
            ));
        }
        words.push(arg);
        at += 1;
    }
    match words.as_slice() {
        [] => {
            let _ = writeln!(w.stderr, "{SETTINGS_HELP}");
            Err(Stop::Exit(2))
        }
        [b"init"] => init(w, &globals),
        _ => Err(Stop::usage(format!(
            "settings takes the one word init; '{} settings --help' says what it does",
            crate::PROGRAM
        ))),
    }
}

/// The text of a flag's value, or the usage error that it is not text.
fn text_of(value: &[u8], flag: &str) -> Flow<String> {
    String::from_utf8(value.to_vec())
        .map_err(|_| Stop::usage(format!("{flag} is kept in the settings file as text")))
}

/// A path that is kept in the file is absolute, so that it means the same from any directory.
fn absolute(value: &[u8], flag: &str) -> Flow<String> {
    if !value.starts_with(b"/") {
        return Err(Stop::usage(format!(
            "{flag} is kept in the settings file, so it takes an absolute path"
        )));
    }
    text_of(value, flag)
}

/// `hk settings init`: the file for the three global flags, after one call that uses them.
fn init(w: &mut World<'_>, globals: &Globals) -> Flow<()> {
    let name = crate::PROGRAM;
    if globals.token_stdin || globals.audience.is_some() || globals.response_file.is_some() {
        return Err(Stop::usage(format!(
            "{name} settings init takes --token-file only: --token-stdin, \
             --github-oidc-audience and --response-file are not for it"
        )));
    }
    let (Some(endpoint), Some(ca_file), Some(token_file)) =
        (&globals.endpoint, &globals.ca_file, &globals.token_file)
    else {
        return Err(Stop::usage(format!(
            "{name} settings init needs --endpoint, --ca-file and --token-file"
        )));
    };
    if !is_endpoint(endpoint) {
        return Err(Stop::usage("--endpoint takes https://<host>[:<port>]"));
    }
    let Some(command) = w.table.iter().find(|c| c.path == LIST_PATH) else {
        return Err(Stop::fail(format!(
            "this {name} has no Xcode.List method to try the settings with"
        )));
    };
    let mut values = toml::Table::new();
    values.insert(
        "endpoint".to_owned(),
        text_of(endpoint, "--endpoint")?.into(),
    );
    values.insert("ca_file".to_owned(), absolute(ca_file, "--ca-file")?.into());
    values.insert(
        "token_file".to_owned(),
        absolute(token_file, "--token-file")?.into(),
    );
    let Some(home) = Home::of(w) else {
        return Err(Stop::fail(format!(
            "{name} settings init keeps its file under $HOME, which is not set"
        )));
    };
    let path = home.settings();
    home.ensure()?;
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(Stop::usage(format!(
            "{} exists; settings init does not replace it",
            path.display()
        )));
    }
    let given = crate::args::Given::new(command.fields);
    let unwritten = |note: &str| format!("{note}; {} was not written", path.display());
    match exchange(w, globals, command, &given, None) {
        Ok(_) => {}
        Err(Stop::Usage(message)) => {
            return Err(Stop::usage(unwritten(&String::from_utf8_lossy(&message))));
        }
        Err(Stop::Fail(message)) => {
            return Err(Stop::fail(unwritten(&String::from_utf8_lossy(&message))));
        }
        Err(stop) => {
            let _ = writeln!(w.stderr, "{name}: {} was not written", path.display());
            return Err(stop);
        }
    }
    home.create_file(&path, values.to_string().as_bytes())?;
    let _ = writeln!(
        w.stdout,
        "wrote {}; the coordinator answered Xcode.List",
        path.display()
    );
    Ok(())
}
